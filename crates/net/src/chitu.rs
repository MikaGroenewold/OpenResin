//! Legacy Chitu WiFi protocol (G-code over UDP port 3000) – experimental.
//!
//! Implemented from publicly available community documentation; it has not
//! been verified against a printer by the OpenResin developers. Commands are
//! ASCII G-code lines, replies are text starting with `ok`. Files are uploaded
//! with `M28 <name>`, binary chunks (`data | offset u32 LE | xor | 0x83`) and
//! `M29`; printing starts with `M6030 ':<name>'`.

use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::net::UdpSocket;
use tokio::sync::{watch, Mutex};
use tokio::task::JoinHandle;

use crate::discovery::DISCOVERY_PORT;
use crate::status::{FileEntry, MachineState, PrinterStatus};
use crate::{NetError, ProgressFn, Result};

pub const CHUNK_SIZE: usize = 0x500;
const REPLY_TIMEOUT: Duration = Duration::from_secs(3);

/// Builds one upload packet: payload, offset (LE), XOR checksum, 0x83 trailer.
pub fn data_packet(data: &[u8], offset: u32) -> Vec<u8> {
    let mut p = Vec::with_capacity(data.len() + 6);
    p.extend_from_slice(data);
    p.extend_from_slice(&offset.to_le_bytes());
    let x = p.iter().fold(0u8, |a, b| a ^ b);
    p.push(x);
    p.push(0x83);
    p
}

/// Parses `SD printing byte 123/456`.
pub fn parse_m27(reply: &str) -> Option<(u64, u64)> {
    let rest = reply.split("byte").nth(1)?.trim();
    let (a, b) = rest.split_once('/')?;
    let a = a.trim().parse().ok()?;
    let b = b.split_whitespace().next()?.trim().parse().ok()?;
    Some((a, b))
}

/// Parses the `D:done/total/paused` and `Z:` fields of an `M4000` reply.
pub fn parse_m4000(reply: &str) -> (Option<(u64, u64, bool)>, Option<f32>) {
    let mut d = None;
    let mut z = None;
    for tok in reply.split_whitespace() {
        if let Some(v) = tok.strip_prefix("D:") {
            let parts: Vec<&str> = v.split('/').collect();
            if parts.len() >= 2 {
                if let (Ok(a), Ok(b)) = (parts[0].parse(), parts[1].parse()) {
                    let paused = parts.get(2).is_some_and(|p| *p == "1");
                    d = Some((a, b, paused));
                }
            }
        } else if let Some(v) = tok.strip_prefix("Z:") {
            z = v.parse().ok();
        }
    }
    (d, z)
}

/// Keeps file names to a conservative character set; the firmware's FAT
/// file system and text protocol do not cope with everything.
pub fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect()
}

pub struct ChituClient {
    pub ip: IpAddr,
    sock: Arc<Mutex<UdpSocket>>,
    state: watch::Receiver<PrinterStatus>,
    state_tx: watch::Sender<PrinterStatus>,
    poll: JoinHandle<()>,
    paused_hint: Arc<AtomicBool>,
}

impl Drop for ChituClient {
    fn drop(&mut self) {
        self.poll.abort();
    }
}

impl ChituClient {
    pub async fn connect(ip: IpAddr) -> Result<Self> {
        Self::connect_addr(SocketAddr::new(ip, DISCOVERY_PORT)).await
    }

    /// Connects to a mainboard listening on a non-standard address/port.
    pub async fn connect_addr(addr: SocketAddr) -> Result<Self> {
        let ip = addr.ip();
        let sock = UdpSocket::bind((crate::bind_addr_for(ip), 0)).await?;
        sock.connect(addr).await?;
        let sock = Arc::new(Mutex::new(sock));
        let (state_tx, state) = watch::channel(PrinterStatus::default());
        let paused_hint = Arc::new(AtomicBool::new(false));
        // verify the printer answers
        command(&sock, "M4000", REPLY_TIMEOUT).await?;
        let poll = {
            let sock = sock.clone();
            let tx = state_tx.clone();
            let paused = paused_hint.clone();
            tokio::spawn(async move {
                loop {
                    let st = poll_status(&sock, &paused).await;
                    let offline = st.is_err();
                    let _ = tx.send(st.unwrap_or(PrinterStatus {
                        state: MachineState::Offline,
                        ..Default::default()
                    }));
                    tokio::time::sleep(Duration::from_secs(if offline { 5 } else { 2 })).await;
                }
            })
        };
        Ok(Self {
            ip,
            sock,
            state,
            state_tx,
            poll,
            paused_hint,
        })
    }

    pub fn status(&self) -> PrinterStatus {
        self.state.borrow().clone()
    }

    pub fn watch(&self) -> watch::Receiver<PrinterStatus> {
        self.state.clone()
    }

    pub async fn refresh(&self) -> Result<()> {
        let st = poll_status(&self.sock, &self.paused_hint).await?;
        let _ = self.state_tx.send(st);
        Ok(())
    }

    pub async fn command(&self, cmd: &str) -> Result<String> {
        command(&self.sock, cmd, REPLY_TIMEOUT).await
    }

    pub async fn list_files(&self) -> Result<Vec<FileEntry>> {
        let sock = self.sock.lock().await;
        drain(&sock).await;
        sock.send(b"M20").await?;
        let mut files = Vec::new();
        let mut buf = vec![0u8; 4096];
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            let n = match tokio::time::timeout(left, sock.recv(&mut buf)).await {
                Ok(r) => r?,
                Err(_) => break,
            };
            let text = String::from_utf8_lossy(&buf[..n]).to_string();
            let mut done = false;
            for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
                if line.starts_with("Begin file list") {
                    continue;
                }
                if line.starts_with("End file list") || line.starts_with("ok") {
                    done = line.starts_with("ok") || done;
                    if line.starts_with("End file list") {
                        done = true;
                    }
                    continue;
                }
                let (name, size) = match line.rsplit_once(' ') {
                    Some((n, s)) if s.parse::<u64>().is_ok() => (n.to_string(), s.parse().ok()),
                    _ => (line.to_string(), None),
                };
                files.push(FileEntry {
                    path: name,
                    is_dir: false,
                    size,
                });
            }
            if done {
                break;
            }
        }
        Ok(files)
    }

    pub async fn delete(&self, name: &str) -> Result<()> {
        expect_ok(&self.command(&format!("M30 {name}")).await?)
    }

    pub async fn start_print(&self, name: &str) -> Result<()> {
        self.paused_hint.store(false, Ordering::Relaxed);
        expect_ok(&self.command(&format!("M6030 ':{name}'")).await?)
    }

    pub async fn pause(&self) -> Result<()> {
        expect_ok(&self.command("M25").await?)?;
        self.paused_hint.store(true, Ordering::Relaxed);
        Ok(())
    }

    pub async fn resume(&self) -> Result<()> {
        expect_ok(&self.command("M24").await?)?;
        self.paused_hint.store(false, Ordering::Relaxed);
        Ok(())
    }

    pub async fn stop(&self) -> Result<()> {
        self.paused_hint.store(false, Ordering::Relaxed);
        expect_ok(&self.command("M33 I5").await?)
    }

    pub async fn upload(
        &self,
        path: &Path,
        remote_name: &str,
        progress: &ProgressFn,
        cancel: &AtomicBool,
    ) -> Result<()> {
        let total = tokio::fs::metadata(path).await?.len();
        if total > u32::MAX as u64 {
            return Err(NetError::Unsupported("file is larger than 4 GiB".into()));
        }
        let mut f = tokio::fs::File::open(path).await?;
        let sock = self.sock.lock().await;
        drain(&sock).await;
        let r = send_wait(&sock, format!("M28 {remote_name}").as_bytes(), REPLY_TIMEOUT).await?;
        expect_ok(&r)?;
        let mut offset = 0u64;
        let mut buf = vec![0u8; CHUNK_SIZE];
        progress(0, total);
        while offset < total {
            if cancel.load(Ordering::Relaxed) {
                let _ = send_wait(&sock, b"M29", REPLY_TIMEOUT).await;
                let _ = send_wait(&sock, format!("M30 {remote_name}").as_bytes(), REPLY_TIMEOUT).await;
                return Err(NetError::Cancelled);
            }
            let n = {
                let mut read = 0;
                while read < buf.len() {
                    let r = f.read(&mut buf[read..]).await?;
                    if r == 0 {
                        break;
                    }
                    read += r;
                }
                read
            };
            if n == 0 {
                break;
            }
            let pkt = data_packet(&buf[..n], offset as u32);
            let mut attempts = 0;
            loop {
                match send_wait(&sock, &pkt, Duration::from_millis(1500)).await {
                    Ok(reply) if reply.starts_with("ok") => break,
                    Ok(reply) if reply.starts_with("resend") => {
                        let to: u64 = reply
                            .split_whitespace()
                            .nth(1)
                            .and_then(|v| v.trim().parse().ok())
                            .unwrap_or(offset);
                        if to != offset {
                            return Err(NetError::Protocol(format!("printer requested data from offset {to}")));
                        }
                    }
                    Ok(reply) => return Err(NetError::Rejected(reply)),
                    Err(NetError::Timeout) => {}
                    Err(e) => return Err(e),
                }
                attempts += 1;
                if attempts > 5 {
                    return Err(NetError::Timeout);
                }
            }
            offset += n as u64;
            progress(offset, total);
        }
        let r = send_wait(&sock, b"M29", Duration::from_secs(10)).await?;
        expect_ok(&r)
    }
}

fn expect_ok(reply: &str) -> Result<()> {
    if reply.trim_start().starts_with("ok") {
        Ok(())
    } else {
        Err(NetError::Rejected(reply.trim().to_string()))
    }
}

async fn drain(sock: &UdpSocket) {
    let mut b = [0u8; 2048];
    while let Ok(Ok(_)) = tokio::time::timeout(Duration::from_millis(5), sock.recv(&mut b)).await {}
}

async fn send_wait(sock: &UdpSocket, data: &[u8], timeout: Duration) -> Result<String> {
    sock.send(data).await?;
    let mut b = vec![0u8; 4096];
    match tokio::time::timeout(timeout, sock.recv(&mut b)).await {
        Ok(Ok(n)) => Ok(String::from_utf8_lossy(&b[..n])
            .trim_matches(char::from(0))
            .trim()
            .to_string()),
        Ok(Err(e)) => Err(e.into()),
        Err(_) => Err(NetError::Timeout),
    }
}

async fn command(sock: &Mutex<UdpSocket>, cmd: &str, timeout: Duration) -> Result<String> {
    let s = sock.lock().await;
    drain(&s).await;
    send_wait(&s, cmd.as_bytes(), timeout).await
}

async fn poll_status(sock: &Mutex<UdpSocket>, paused_hint: &AtomicBool) -> Result<PrinterStatus> {
    let m4000 = command(sock, "M4000", REPLY_TIMEOUT).await?;
    let m27 = command(sock, "M27", REPLY_TIMEOUT).await.unwrap_or_default();
    let (d, _z) = parse_m4000(&m4000);
    let progress = parse_m27(&m27).or(d.map(|(a, b, _)| (a, b)));
    let paused = d.is_some_and(|x| x.2) || paused_hint.load(Ordering::Relaxed);
    // "Not SD printing." also contains "SD printing"
    let printing = (m27.contains("SD printing") && !m27.contains("Not SD printing"))
        || progress.is_some_and(|(a, b)| b > 0 && a < b);
    let state = match (printing, paused) {
        (true, true) => MachineState::Paused,
        (true, false) => MachineState::Printing,
        _ => MachineState::Idle,
    };
    Ok(PrinterStatus {
        state,
        detail: Some("legacy WiFi".into()),
        // byte progress is reported as permille "layers" so the UI shows a percentage
        current_layer: progress
            .filter(|p| p.1 > 0)
            .map(|(a, b)| ((a as f64 / b as f64) * 1000.0) as u32),
        total_layers: progress.filter(|p| p.1 > 0).map(|_| 1000),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_layout() {
        let p = data_packet(&[1, 2, 3], 0x0102_0304);
        assert_eq!(p[..3], [1, 2, 3]);
        assert_eq!(p[3..7], [4, 3, 2, 1]);
        assert_eq!(p[7], [1u8, 2, 3, 4, 3, 2, 1].iter().fold(0, |a, b| a ^ b));
        assert_eq!(p[8], 0x83);
    }

    #[test]
    fn replies() {
        assert_eq!(parse_m27("SD printing byte 1200/34000"), Some((1200, 34000)));
        assert_eq!(parse_m27("Not SD printing."), None);
        let (d, z) = parse_m4000("ok B:0/0 X:0.000 Y:0.000 Z:12.500 F:256/256 D:500/1000/1");
        assert_eq!(d, Some((500, 1000, true)));
        assert_eq!(z, Some(12.5));
        assert_eq!(sanitize_name("Mäx test (1).ctb"), "M_x_test__1_.ctb");
    }

    #[tokio::test]
    async fn upload_against_fake_printer() {
        // a minimal fake mainboard acknowledging every packet
        let srv = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = srv.local_addr().unwrap();
        let received = Arc::new(Mutex::new(Vec::<u8>::new()));
        let rec2 = received.clone();
        tokio::spawn(async move {
            let mut b = vec![0u8; 4096];
            loop {
                let (n, from) = srv.recv_from(&mut b).await.unwrap();
                let msg = &b[..n];
                let reply: &[u8] = if msg.ends_with(&[0x83]) && n > 6 {
                    rec2.lock().await.extend_from_slice(&msg[..n - 6]);
                    b"ok"
                } else {
                    b"ok N:0"
                };
                srv.send_to(reply, from).await.unwrap();
            }
        });
        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        sock.connect(addr).await.unwrap();
        let (tx, rx) = watch::channel(PrinterStatus::default());
        let client = ChituClient {
            ip: addr.ip(),
            sock: Arc::new(Mutex::new(sock)),
            state: rx,
            state_tx: tx,
            poll: tokio::spawn(async {}),
            paused_hint: Arc::new(AtomicBool::new(false)),
        };
        let dir = std::env::temp_dir().join(format!("openresin-chitu-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.ctb");
        let data: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &data).unwrap();
        client
            .upload(&path, "f.ctb", &|_, _| {}, &AtomicBool::new(false))
            .await
            .unwrap();
        assert_eq!(*received.lock().await, data);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
