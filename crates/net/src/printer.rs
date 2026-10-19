//! Protocol independent printer connection.

use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::chitu::{sanitize_name, ChituClient};
use crate::discovery::{discover_addr, Protocol, DISCOVERY_PORT};
use crate::sdcp::{SdcpClient, WS_PORT};
use crate::status::{FileEntry, MachineState, PrinterStatus};
use crate::{NetError, ProgressFn, Result};

/// Latest known state of a printer.
#[derive(Debug, Clone, Default)]
pub struct DeviceState {
    pub status: PrinterStatus,
    /// Protocol specific details (SDCP attributes), if any.
    pub attributes: Option<Value>,
}

enum Kind {
    Sdcp(SdcpClient),
    Chitu(ChituClient),
}

pub struct PrinterClient {
    kind: Kind,
    state: watch::Receiver<DeviceState>,
    forward: JoinHandle<()>,
}

impl Drop for PrinterClient {
    fn drop(&mut self) {
        self.forward.abort();
    }
}

impl PrinterClient {
    /// Finds out which protocol the printer at `ip` speaks and connects.
    pub async fn connect(ip: IpAddr) -> Result<Self> {
        Self::connect_with_ports(ip, DISCOVERY_PORT, WS_PORT).await
    }

    /// Like [`connect`](Self::connect) for printers (or port forwards) that
    /// use non-standard ports: `udp_port` for discovery and the legacy
    /// protocol, `sdcp_port` for the SDCP WebSocket/HTTP server.
    pub async fn connect_with_ports(ip: IpAddr, udp_port: u16, sdcp_port: u16) -> Result<Self> {
        let found = discover_addr(SocketAddr::new(ip, udp_port), Duration::from_secs(3)).await?;
        match found {
            Some(mut p) if p.protocol == Protocol::Sdcp => {
                // the address that answered, not the one the reply claims (NAT, VPN)
                p.ip = ip;
                Ok(Self::from_sdcp(SdcpClient::connect_with_port(&p, sdcp_port).await?))
            }
            _ => match ChituClient::connect_addr(SocketAddr::new(ip, udp_port)).await {
                Ok(c) => Ok(Self::from_chitu(c)),
                Err(e) if found.is_none() => Err(NetError::Protocol(format!(
                    "no printer answered at {ip} ({e}); check the address and that the printer is on"
                ))),
                Err(e) => Err(e),
            },
        }
    }

    fn from_sdcp(c: SdcpClient) -> Self {
        let mut rx = c.watch_state();
        let first = rx.borrow().clone();
        let (tx, state) = watch::channel(DeviceState {
            status: first.status,
            attributes: first.attributes,
        });
        let forward = tokio::spawn(async move {
            while rx.changed().await.is_ok() {
                let s = rx.borrow().clone();
                if tx
                    .send(DeviceState {
                        status: s.status,
                        attributes: s.attributes,
                    })
                    .is_err()
                {
                    break;
                }
            }
            tx.send_modify(|s| s.status.state = MachineState::Offline);
        });
        Self {
            kind: Kind::Sdcp(c),
            state,
            forward,
        }
    }

    fn from_chitu(c: ChituClient) -> Self {
        let mut rx = c.watch();
        let (tx, state) = watch::channel(DeviceState {
            status: rx.borrow().clone(),
            attributes: None,
        });
        let forward = tokio::spawn(async move {
            while rx.changed().await.is_ok() {
                let s = rx.borrow().clone();
                if tx
                    .send(DeviceState {
                        status: s,
                        attributes: None,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            kind: Kind::Chitu(c),
            state,
            forward,
        }
    }

    pub fn protocol(&self) -> Protocol {
        match self.kind {
            Kind::Sdcp(_) => Protocol::Sdcp,
            Kind::Chitu(_) => Protocol::ChituUdp,
        }
    }

    pub fn state(&self) -> DeviceState {
        self.state.borrow().clone()
    }

    pub fn watch(&self) -> watch::Receiver<DeviceState> {
        self.state.clone()
    }

    pub fn supports_camera(&self) -> bool {
        matches!(self.kind, Kind::Sdcp(_))
    }

    pub async fn refresh(&self) -> Result<()> {
        match &self.kind {
            Kind::Sdcp(c) => c.refresh().await,
            Kind::Chitu(c) => c.refresh().await,
        }
    }

    pub async fn list_files(&self, dir: &str) -> Result<Vec<FileEntry>> {
        match &self.kind {
            Kind::Sdcp(c) => c.list_files(dir).await,
            Kind::Chitu(c) => c.list_files().await,
        }
    }

    /// Deletes files; returns the paths that could not be deleted.
    pub async fn delete(&self, files: &[String]) -> Result<Vec<String>> {
        match &self.kind {
            Kind::Sdcp(c) => c.delete(files, &[]).await,
            Kind::Chitu(c) => {
                let mut failed = Vec::new();
                for f in files {
                    if c.delete(f).await.is_err() {
                        failed.push(f.clone());
                    }
                }
                Ok(failed)
            }
        }
    }

    /// Recent print jobs (SDCP only).
    pub async fn history(&self, limit: usize) -> Result<Value> {
        match &self.kind {
            Kind::Sdcp(c) => {
                let ids: Vec<String> = c.history().await?.into_iter().take(limit).collect();
                if ids.is_empty() {
                    return Ok(Value::Array(Vec::new()));
                }
                c.task_details(&ids).await
            }
            Kind::Chitu(_) => Ok(Value::Array(Vec::new())),
        }
    }

    pub async fn start_print(&self, file: &str) -> Result<()> {
        match &self.kind {
            Kind::Sdcp(c) => c.start_print(file, 0).await,
            Kind::Chitu(c) => c.start_print(file).await,
        }
    }

    pub async fn pause(&self) -> Result<()> {
        match &self.kind {
            Kind::Sdcp(c) => c.pause().await,
            Kind::Chitu(c) => c.pause().await,
        }
    }

    pub async fn resume(&self) -> Result<()> {
        match &self.kind {
            Kind::Sdcp(c) => c.resume().await,
            Kind::Chitu(c) => c.resume().await,
        }
    }

    pub async fn stop(&self) -> Result<()> {
        match &self.kind {
            Kind::Sdcp(c) => c.stop().await,
            Kind::Chitu(c) => c.stop().await,
        }
    }

    /// Name the file will get on the printer (some protocols restrict names).
    pub fn remote_name(&self, wanted: &str) -> String {
        match self.kind {
            Kind::Sdcp(_) => wanted.to_string(),
            Kind::Chitu(_) => sanitize_name(wanted),
        }
    }

    pub async fn upload(
        &self,
        path: &Path,
        remote_name: &str,
        progress: &ProgressFn,
        cancel: &AtomicBool,
    ) -> Result<()> {
        match &self.kind {
            Kind::Sdcp(c) => c.upload(path, remote_name, progress, cancel).await,
            Kind::Chitu(c) => c.upload(path, remote_name, progress, cancel).await,
        }
    }

    /// Whether stored files can be downloaded (and previewed).
    pub fn can_download(&self, path: &str) -> bool {
        match &self.kind {
            Kind::Sdcp(c) => c.file_url(path).is_some(),
            Kind::Chitu(_) => false,
        }
    }

    pub async fn read_range(&self, path: &str, offset: u64, len: u64) -> Result<Vec<u8>> {
        match &self.kind {
            Kind::Sdcp(c) => c.read_range(path, offset, len).await,
            Kind::Chitu(_) => Err(NetError::Unsupported("reading files is not available".into())),
        }
    }

    pub async fn download(&self, path: &str, dest: &Path, progress: &ProgressFn, cancel: &AtomicBool) -> Result<u64> {
        match &self.kind {
            Kind::Sdcp(c) => c.download(path, dest, progress, cancel).await,
            Kind::Chitu(_) => Err(NetError::Unsupported("downloading is not available".into())),
        }
    }

    /// Turns the printer's time-lapse recording on or off (SDCP only).
    pub async fn set_time_lapse(&self, enable: bool) -> Result<()> {
        match &self.kind {
            Kind::Sdcp(c) => c.set_time_lapse(enable).await,
            Kind::Chitu(_) => Err(NetError::Unsupported("time-lapse is not available".into())),
        }
    }

    /// Renames the printer (SDCP only).
    pub async fn rename(&self, name: &str) -> Result<()> {
        match &self.kind {
            Kind::Sdcp(c) => c.rename(name).await,
            Kind::Chitu(_) => Err(NetError::Unsupported("renaming is not available".into())),
        }
    }

    /// Enables/disables the camera; returns the stream URL when enabling.
    pub async fn enable_video(&self, enable: bool) -> Result<Option<String>> {
        match &self.kind {
            Kind::Sdcp(c) => c.enable_video(enable).await,
            Kind::Chitu(_) => Err(NetError::Unsupported("this printer has no network camera".into())),
        }
    }
}
