//! SDCP ("Smart Device Control Protocol") client.
//!
//! JSON messages over `ws://<ip>:3030/websocket`, file upload via
//! `POST http://<ip>:3030/uploadFile/upload` in 1 MiB chunks (protocol V3), or –
//! for older mainboards – by letting the printer download the file from a
//! short-lived HTTP server on this machine (command 256).

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use md5::{Digest, Md5};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

use crate::discovery::{discover_host, DiscoveredPrinter, Protocol};
use crate::status::{FileEntry, MachineState, PrinterStatus};
use crate::{NetError, ProgressFn, Result};

pub const WS_PORT: u16 = 3030;
pub const CHUNK_SIZE: usize = 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const PING_INTERVAL: Duration = Duration::from_secs(20);

pub mod cmd {
    pub const STATUS: u32 = 0;
    pub const ATTRIBUTES: u32 = 1;
    pub const START_PRINT: u32 = 128;
    pub const PAUSE: u32 = 129;
    pub const STOP: u32 = 130;
    pub const RESUME: u32 = 131;
    pub const STOP_FEEDING: u32 = 132;
    pub const SKIP_PREHEAT: u32 = 133;
    pub const RENAME: u32 = 192;
    pub const TERMINATE_TRANSFER: u32 = 255;
    pub const DOWNLOAD_FILE: u32 = 256;
    pub const FILE_LIST: u32 = 258;
    pub const DELETE_FILES: u32 = 259;
    pub const HISTORY: u32 = 320;
    pub const TASK_DETAILS: u32 = 321;
    pub const VIDEO_STREAM: u32 = 386;
    pub const TIME_LAPSE: u32 = 387;
}

/// Events pushed by the printer.
#[derive(Debug, Clone)]
pub enum SdcpEvent {
    Status(PrinterStatus),
    Attributes(Value),
    Error(Value),
    Notice(Value),
    Disconnected,
}

#[derive(Debug, Clone, Default)]
pub struct SdcpState {
    pub status: PrinterStatus,
    pub raw_status: Option<Value>,
    pub attributes: Option<Value>,
}

type Pending = Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>;

pub struct SdcpClient {
    pub ip: IpAddr,
    /// Port of the WebSocket/HTTP server ([`WS_PORT`] on real printers).
    pub port: u16,
    pub id: String,
    pub mainboard_id: String,
    pub protocol_version: String,
    out: mpsc::UnboundedSender<Message>,
    pending: Pending,
    state: watch::Receiver<SdcpState>,
    events: broadcast::Sender<SdcpEvent>,
    /// Set by the reader task once the WebSocket is gone.
    closed: Arc<AtomicBool>,
    request_timeout: Duration,
    tasks: Vec<JoinHandle<()>>,
    http: reqwest::Client,
}

impl Drop for SdcpClient {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

fn now_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn request_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..16].to_string()
}

impl SdcpClient {
    /// Connects to a printer found by discovery.
    pub async fn connect(p: &DiscoveredPrinter) -> Result<Self> {
        Self::connect_with_port(p, WS_PORT).await
    }

    /// Like [`connect`](Self::connect) for a server on a non-standard port.
    pub async fn connect_with_port(p: &DiscoveredPrinter, port: u16) -> Result<Self> {
        if p.protocol != Protocol::Sdcp {
            return Err(NetError::Unsupported("printer does not speak SDCP".into()));
        }
        let mainboard_id = p
            .mainboard_id
            .clone()
            .ok_or_else(|| NetError::Protocol("discovery reply lacks MainboardID".into()))?;
        let url = format!("ws://{}/websocket", SocketAddr::new(p.ip, port));
        let (ws, _) = tokio::time::timeout(Duration::from_secs(8), tokio_tungstenite::connect_async(url))
            .await
            .map_err(|_| NetError::Timeout)??;
        let (mut sink, mut stream) = ws.split();
        let (out, mut out_rx) = mpsc::unbounded_channel::<Message>();
        let pending: Pending = Arc::default();
        let (state_tx, state) = watch::channel(SdcpState::default());
        let (events, _) = broadcast::channel(64);
        let closed = Arc::new(AtomicBool::new(false));

        let writer = tokio::spawn(async move {
            let mut ping = tokio::time::interval(PING_INTERVAL);
            ping.tick().await;
            loop {
                tokio::select! {
                    msg = out_rx.recv() => match msg {
                        Some(m) => if sink.send(m).await.is_err() { break },
                        None => break,
                    },
                    _ = ping.tick() => {
                        if sink.send(Message::Text("ping".into())).await.is_err() { break }
                    }
                }
            }
        });
        let reader = {
            let pending = pending.clone();
            let events = events.clone();
            let closed = closed.clone();
            tokio::spawn(async move {
                while let Some(msg) = stream.next().await {
                    let text = match msg {
                        Ok(Message::Text(t)) => t.to_string(),
                        Ok(Message::Binary(b)) => String::from_utf8_lossy(&b).into_owned(),
                        Ok(Message::Close(_)) | Err(_) => break,
                        Ok(_) => continue,
                    };
                    if text.trim() == "pong" {
                        continue;
                    }
                    let Ok(v) = serde_json::from_str::<Value>(&text) else {
                        log::debug!("sdcp: non-JSON message: {text}");
                        continue;
                    };
                    handle_message(v, &pending, &state_tx, &events);
                }
                // fail outstanding requests now instead of letting them time out
                closed.store(true, Ordering::SeqCst);
                pending.lock().unwrap().clear();
                let _ = events.send(SdcpEvent::Disconnected);
                state_tx.send_modify(|s| s.status.state = MachineState::Offline);
            })
        };

        let client = SdcpClient {
            ip: p.ip,
            port,
            id: p.id.clone().unwrap_or_default(),
            mainboard_id,
            protocol_version: p.protocol_version.clone().unwrap_or_default(),
            out,
            pending,
            state,
            events,
            closed,
            request_timeout: REQUEST_TIMEOUT,
            tasks: vec![writer, reader],
            http: reqwest::Client::builder()
                .no_proxy()
                .connect_timeout(Duration::from_secs(8))
                .timeout(Duration::from_secs(120))
                .build()?,
        };
        // ask for an initial snapshot; ignore failures from minimal firmwares
        let _ = client.request(cmd::ATTRIBUTES, json!({})).await;
        let _ = client.request(cmd::STATUS, json!({})).await;
        Ok(client)
    }

    /// Discovers the printer at `ip` and connects to it.
    pub async fn connect_ip(ip: IpAddr) -> Result<Self> {
        let mut p = discover_host(ip, Duration::from_secs(3))
            .await?
            .ok_or(NetError::Timeout)?;
        // the address that answered, not the one the reply claims (NAT, VPN)
        p.ip = ip;
        Self::connect(&p).await
    }

    /// How long [`request`](Self::request) waits for an answer (default 10 s).
    pub fn set_request_timeout(&mut self, timeout: Duration) {
        self.request_timeout = timeout;
    }

    pub fn state(&self) -> SdcpState {
        self.state.borrow().clone()
    }

    pub fn watch_state(&self) -> watch::Receiver<SdcpState> {
        self.state.clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SdcpEvent> {
        self.events.subscribe()
    }

    fn is_v3(&self) -> bool {
        let v = self.protocol_version.trim_start_matches(['V', 'v']);
        v.split('.').next().and_then(|m| m.parse::<u32>().ok()).unwrap_or(3) >= 3
    }

    /// Sends a request and waits for the matching response's `Data.Data` object.
    pub async fn request(&self, command: u32, data: Value) -> Result<Value> {
        let rid = request_id();
        let msg = json!({
            "Id": self.id,
            "Data": {
                "Cmd": command,
                "Data": data,
                "RequestID": rid,
                "MainboardID": self.mainboard_id,
                "TimeStamp": now_s(),
                "From": 0
            },
            "Topic": format!("sdcp/request/{}", self.mainboard_id),
        });
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(rid.clone(), tx);
        if self.closed.load(Ordering::SeqCst) || self.out.send(Message::Text(msg.to_string())).is_err() {
            self.pending.lock().unwrap().remove(&rid);
            return Err(NetError::Closed);
        }
        let resp = match tokio::time::timeout(self.request_timeout, rx).await {
            Ok(Ok(v)) => v,
            Ok(Err(_)) => return Err(NetError::Closed),
            Err(_) => {
                self.pending.lock().unwrap().remove(&rid);
                return Err(NetError::Timeout);
            }
        };
        let inner = resp.pointer("/Data/Data").cloned().unwrap_or(Value::Null);
        let ack = inner.get("Ack").and_then(Value::as_i64).unwrap_or(0);
        if ack != 0 {
            return Err(NetError::Rejected(ack_message(command, ack)));
        }
        Ok(inner)
    }

    pub async fn refresh(&self) -> Result<()> {
        self.request(cmd::STATUS, json!({})).await.map(|_| ())
    }

    pub async fn start_print(&self, filename: &str, start_layer: u32) -> Result<()> {
        self.request(
            cmd::START_PRINT,
            json!({"Filename": filename, "StartLayer": start_layer}),
        )
        .await
        .map(|_| ())
    }

    pub async fn pause(&self) -> Result<()> {
        self.request(cmd::PAUSE, json!({})).await.map(|_| ())
    }

    pub async fn resume(&self) -> Result<()> {
        self.request(cmd::RESUME, json!({})).await.map(|_| ())
    }

    pub async fn stop(&self) -> Result<()> {
        self.request(cmd::STOP, json!({})).await.map(|_| ())
    }

    pub async fn rename(&self, name: &str) -> Result<()> {
        self.request(cmd::RENAME, json!({"Name": name})).await.map(|_| ())
    }

    pub async fn list_files(&self, dir: &str) -> Result<Vec<FileEntry>> {
        let v = self.request(cmd::FILE_LIST, json!({"Url": dir})).await?;
        let list = v.get("FileList").and_then(Value::as_array).cloned().unwrap_or_default();
        Ok(list
            .iter()
            .map(|f| FileEntry {
                path: f.get("name").and_then(Value::as_str).unwrap_or_default().to_string(),
                is_dir: f.get("type").and_then(Value::as_i64) == Some(0),
                size: f.get("FileSize").or_else(|| f.get("size")).and_then(Value::as_u64),
            })
            .collect())
    }

    pub async fn delete(&self, files: &[String], folders: &[String]) -> Result<Vec<String>> {
        let v = self
            .request(cmd::DELETE_FILES, json!({"FileList": files, "FolderList": folders}))
            .await?;
        Ok(v.get("ErrData")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default())
    }

    pub async fn history(&self) -> Result<Vec<String>> {
        let v = self.request(cmd::HISTORY, json!({})).await?;
        Ok(v.get("HistoryData")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default())
    }

    pub async fn task_details(&self, ids: &[String]) -> Result<Value> {
        let v = self.request(cmd::TASK_DETAILS, json!({"Id": ids})).await?;
        Ok(v.get("HistoryDetailList").cloned().unwrap_or(Value::Null))
    }

    /// Enables the camera stream and returns its URL (RTSP or HTTP/MJPEG).
    pub async fn enable_video(&self, enable: bool) -> Result<Option<String>> {
        let v = self.request(cmd::VIDEO_STREAM, json!({"Enable": enable as u8})).await?;
        Ok(v.get("VideoUrl")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(|u| normalize_video_url(u, self.ip)))
    }

    pub async fn set_time_lapse(&self, enable: bool) -> Result<()> {
        self.request(cmd::TIME_LAPSE, json!({"Enable": enable as u8}))
            .await
            .map(|_| ())
    }

    /// HTTP URL of a file on the printer's internal storage (`/local/...`).
    ///
    /// Observed on ELEGOO SDCP V3 firmware: the web server on port 3030 serves
    /// the internal storage below `/media/mmcblk0p3/` and supports byte ranges.
    pub fn file_url(&self, path: &str) -> Option<String> {
        let rest = path
            .strip_prefix("/local/")
            .or_else(|| (!path.starts_with('/')).then_some(path))?;
        let enc: String = rest
            .bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => (b as char).to_string(),
                _ => format!("%{b:02X}"),
            })
            .collect();
        Some(format!(
            "http://{}/media/mmcblk0p3/{enc}",
            SocketAddr::new(self.ip, self.port)
        ))
    }

    /// Reads `len` bytes at `offset` of a stored file (HTTP range request).
    pub async fn read_range(&self, path: &str, offset: u64, len: u64) -> Result<Vec<u8>> {
        let url = self
            .file_url(path)
            .ok_or_else(|| NetError::Unsupported("files on this storage cannot be read".into()))?;
        if len == 0 {
            return Ok(Vec::new());
        }
        let r = self
            .http
            .get(&url)
            .header("Range", format!("bytes={offset}-{}", offset + len - 1))
            .send()
            .await?
            .error_for_status()?;
        let partial = r.status() == reqwest::StatusCode::PARTIAL_CONTENT;
        let body = r.bytes().await?;
        // a server may ignore the range and send the whole file (200)
        let start = if partial { 0 } else { (offset as usize).min(body.len()) };
        let end = start.saturating_add(len as usize).min(body.len());
        Ok(body[start..end].to_vec())
    }

    /// Downloads a stored file to `dest`.
    pub async fn download(&self, path: &str, dest: &Path, progress: &ProgressFn, cancel: &AtomicBool) -> Result<u64> {
        let url = self
            .file_url(path)
            .ok_or_else(|| NetError::Unsupported("files on this storage cannot be downloaded".into()))?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(8))
            .build()?;
        let resp = client.get(&url).send().await?.error_for_status()?;
        let total = resp.content_length().unwrap_or(0);
        let tmp = dest.with_extension("part");
        let mut f = tokio::fs::File::create(&tmp).await?;
        let mut done = 0u64;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            if cancel.load(Ordering::Relaxed) {
                drop(f);
                let _ = tokio::fs::remove_file(&tmp).await;
                return Err(NetError::Cancelled);
            }
            let chunk = chunk?;
            f.write_all(&chunk).await?;
            done += chunk.len() as u64;
            progress(done, total.max(done));
        }
        f.flush().await?;
        drop(f);
        tokio::fs::rename(&tmp, dest).await?;
        Ok(done)
    }

    /// Uploads a local file to the printer's storage as `remote_name`.
    pub async fn upload(
        &self,
        path: &Path,
        remote_name: &str,
        progress: &ProgressFn,
        cancel: &AtomicBool,
    ) -> Result<()> {
        if self.is_v3() {
            self.upload_http(path, remote_name, progress, cancel).await
        } else {
            self.upload_pull(path, remote_name, progress, cancel).await
        }
    }

    async fn upload_http(
        &self,
        path: &Path,
        remote_name: &str,
        progress: &ProgressFn,
        cancel: &AtomicBool,
    ) -> Result<()> {
        let total = tokio::fs::metadata(path).await?.len();
        if total == 0 {
            // the loop below would send nothing and report success
            return Err(NetError::Unsupported("cannot upload an empty file".into()));
        }
        let md5 = file_md5(path).await?;
        let uuid = uuid::Uuid::new_v4().simple().to_string();
        let url = format!("http://{}/uploadFile/upload", SocketAddr::new(self.ip, self.port));
        let mut f = tokio::fs::File::open(path).await?;
        let mut offset = 0u64;
        let mut buf = vec![0u8; CHUNK_SIZE];
        progress(0, total);
        while offset < total {
            if cancel.load(Ordering::Relaxed) {
                let _ = self
                    .request(cmd::TERMINATE_TRANSFER, json!({"Uuid": uuid, "FileName": remote_name}))
                    .await;
                return Err(NetError::Cancelled);
            }
            let n = read_full(&mut f, &mut buf).await?;
            if n == 0 {
                break;
            }
            let make_form = || -> Result<reqwest::multipart::Form> {
                let part = reqwest::multipart::Part::bytes(buf[..n].to_vec())
                    .file_name(remote_name.to_string())
                    .mime_str("application/octet-stream")?;
                Ok(reqwest::multipart::Form::new()
                    .text("S-File-MD5", md5.clone())
                    .text("Check", "1")
                    .text("Offset", offset.to_string())
                    .text("Uuid", uuid.clone())
                    .text("TotalSize", total.to_string())
                    .part("File", part))
            };
            let mut attempt = 0;
            loop {
                match self.http.post(&url).multipart(make_form()?).send().await {
                    Ok(r) => {
                        let v: Value = r.json().await.unwrap_or(Value::Null);
                        if v.get("success").and_then(Value::as_bool) == Some(true) {
                            break;
                        }
                        return Err(NetError::Rejected(upload_error(&v)));
                    }
                    Err(e) if attempt < 2 => {
                        log::warn!("upload chunk at {offset} failed ({e}), retrying");
                        attempt += 1;
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            offset += n as u64;
            progress(offset, total);
        }
        Ok(())
    }

    /// Older mainboards fetch the file themselves (command 256).
    async fn upload_pull(
        &self,
        path: &Path,
        remote_name: &str,
        progress: &ProgressFn,
        cancel: &AtomicBool,
    ) -> Result<()> {
        let total = tokio::fs::metadata(path).await?.len();
        let md5 = file_md5(path).await?;
        let local_ip = local_ip_towards(self.ip)?;
        let server = FileServer::start(local_ip, path.to_path_buf(), remote_name.to_string()).await?;
        let mut rx = self.state.clone();
        // only status pushed after the request can describe this transfer; the
        // current one may still show the outcome of a previous transfer
        rx.borrow_and_update();
        let mut fresh = false;
        self.request(
            cmd::DOWNLOAD_FILE,
            json!({
                "Check": 0,
                "CleanCache": 1,
                "Compress": 0,
                "FileSize": total,
                "Filename": remote_name,
                "MD5": md5,
                "URL": server.url,
            }),
        )
        .await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30 + total / 100_000);
        loop {
            if cancel.load(Ordering::Relaxed) {
                let _ = self
                    .request(cmd::TERMINATE_TRANSFER, json!({"Uuid": "", "FileName": remote_name}))
                    .await;
                return Err(NetError::Cancelled);
            }
            let served = server.served.load(Ordering::Relaxed);
            progress(served.min(total), total);
            let info = fresh
                .then(|| {
                    rx.borrow()
                        .raw_status
                        .as_ref()
                        .and_then(|s| status_object(s).get("FileTransferInfo").cloned())
                })
                .flatten();
            if let Some(t) = info {
                match t.get("Status").and_then(Value::as_i64) {
                    Some(2) => {
                        progress(total, total);
                        return Ok(());
                    }
                    Some(3) => return Err(NetError::Rejected("printer reported a failed transfer".into())),
                    _ => {}
                }
            }
            if tokio::time::Instant::now() > deadline {
                return Err(NetError::Timeout);
            }
            if let Ok(Ok(())) = tokio::time::timeout(Duration::from_millis(500), rx.changed()).await {
                fresh = true;
            }
        }
    }
}

async fn read_full(f: &mut tokio::fs::File, buf: &mut [u8]) -> Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        let r = f.read(&mut buf[n..]).await?;
        if r == 0 {
            break;
        }
        n += r;
    }
    Ok(n)
}

pub async fn file_md5(path: &Path) -> Result<String> {
    let mut f = tokio::fs::File::open(path).await?;
    let mut h = Md5::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}

/// Determines the local address used to reach `peer` (no packets are sent).
pub fn local_ip_towards(peer: IpAddr) -> Result<IpAddr> {
    if peer.is_loopback() {
        return Ok(peer);
    }
    let s = std::net::UdpSocket::bind(("0.0.0.0", 0))?;
    s.connect(SocketAddr::new(peer, 9))?;
    Ok(s.local_addr()?.ip())
}

fn normalize_video_url(u: &str, ip: IpAddr) -> String {
    let u = u.trim().replace("${ipaddr}", &ip.to_string());
    if u.contains("://") {
        u
    } else if u.contains(":554") || u.to_ascii_lowercase().contains("rtsp") {
        format!("rtsp://{u}")
    } else {
        format!("http://{u}")
    }
}

fn ack_message(command: u32, ack: i64) -> String {
    let text = match (command, ack) {
        (cmd::START_PRINT, 1) => "printer is busy",
        (cmd::START_PRINT, 2) => "file not found",
        (cmd::START_PRINT, 3) => "MD5 verification failed",
        (cmd::START_PRINT, 4) => "file read failed",
        (cmd::START_PRINT, 5) => "resolution does not match this printer",
        (cmd::START_PRINT, 6) => "unrecognised file format",
        (cmd::START_PRINT, 7) => "file was sliced for a different machine model",
        (cmd::VIDEO_STREAM, 1) => "too many camera streams are open",
        (cmd::VIDEO_STREAM, 2) => "printer has no camera",
        (cmd::TERMINATE_TRANSFER, 1) => "no transfer in progress",
        (cmd::TERMINATE_TRANSFER, 2) => "file is already being verified",
        (cmd::TERMINATE_TRANSFER, 3) => "file not found",
        _ => "",
    };
    if text.is_empty() {
        format!("command {command} failed with code {ack}")
    } else {
        format!("{text} (code {ack})")
    }
}

fn upload_error(v: &Value) -> String {
    let msgs = v.get("messages").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut parts = Vec::new();
    for m in msgs {
        let field = m.get("field").and_then(Value::as_str).unwrap_or("");
        let msg = m.get("message").map(|x| x.to_string()).unwrap_or_default();
        let explained = match msg.trim_matches('"') {
            "-1" => "illegal offset".to_string(),
            "-2" => "offset does not match the partial file".to_string(),
            "-3" => "printer cannot open the file".to_string(),
            "-4" => "unknown error".to_string(),
            other => other.to_string(),
        };
        parts.push(if field == "common_field" {
            explained
        } else {
            format!("{field}: {explained}")
        });
    }
    if parts.is_empty() {
        format!("upload failed: {v}")
    } else {
        parts.join(", ")
    }
}

fn handle_message(
    v: Value,
    pending: &Pending,
    state: &watch::Sender<SdcpState>,
    events: &broadcast::Sender<SdcpEvent>,
) {
    let topic = v.get("Topic").and_then(Value::as_str).unwrap_or("");
    if topic.starts_with("sdcp/response/") {
        if let Some(rid) = v.pointer("/Data/RequestID").and_then(Value::as_str) {
            if let Some(tx) = pending.lock().unwrap().remove(rid) {
                let _ = tx.send(v);
            }
        }
    } else if topic.starts_with("sdcp/status/") || v.get("Status").is_some() {
        let st = parse_status(&v);
        state.send_modify(|s| {
            s.status = PrinterStatus {
                free_storage: s.status.free_storage,
                camera_connected: s.status.camera_connected,
                ..st.clone()
            };
            s.raw_status = Some(v.clone());
        });
        let _ = events.send(SdcpEvent::Status(st));
    } else if topic.starts_with("sdcp/attributes/") || v.get("Attributes").is_some() {
        let attrs = v
            .get("Attributes")
            .or_else(|| v.pointer("/Data/Attributes"))
            .cloned()
            .unwrap_or(Value::Null);
        state.send_modify(|s| {
            s.status.free_storage = attrs.get("RemainingMemory").and_then(Value::as_u64);
            s.status.camera_connected = attrs.get("CameraStatus").and_then(Value::as_i64).map(|c| c == 1);
            s.attributes = Some(attrs.clone());
        });
        let _ = events.send(SdcpEvent::Attributes(attrs));
    } else if topic.starts_with("sdcp/error/") {
        let _ = events.send(SdcpEvent::Error(v));
    } else if topic.starts_with("sdcp/notice/") {
        let _ = events.send(SdcpEvent::Notice(v));
    }
}

/// The `Status` object of a status message (V3: top level, older: `Data.Status`).
fn status_object(v: &Value) -> &Value {
    v.get("Status").or_else(|| v.pointer("/Data/Status")).unwrap_or(v)
}

/// Maps an SDCP status message to [`PrinterStatus`].
pub fn parse_status(v: &Value) -> PrinterStatus {
    let s = status_object(v);
    let current = match s.get("CurrentStatus") {
        Some(Value::Array(a)) => a.first().and_then(Value::as_i64),
        Some(x) => x.as_i64(),
        None => None,
    };
    let pi = s.get("PrintInfo");
    let sub = pi.and_then(|p| p.get("Status")).and_then(Value::as_i64);
    let state = match current {
        Some(0) => match sub {
            Some(8) => MachineState::Stopped,
            Some(9) => MachineState::Completed,
            _ => MachineState::Idle,
        },
        Some(1) => match sub {
            Some(1) => MachineState::Homing,
            Some(5) => MachineState::Pausing,
            Some(6) => MachineState::Paused,
            Some(7) => MachineState::Stopping,
            Some(8) => MachineState::Stopped,
            Some(9) => MachineState::Completed,
            Some(10) => MachineState::FileChecking,
            _ => MachineState::Printing,
        },
        Some(2) => MachineState::FileTransfer,
        Some(3) => MachineState::ExposureTest,
        Some(4) => MachineState::SelfTest,
        _ => MachineState::Unknown,
    };
    let detail = sub.map(|x| {
        match x {
            0 => "idle",
            1 => "homing",
            2 => "descending",
            3 => "exposing",
            4 => "lifting",
            5 => "pausing",
            6 => "paused",
            7 => "stopping",
            8 => "stopped",
            9 => "complete",
            10 => "checking file",
            _ => "unknown",
        }
        .to_string()
    });
    let u = |k: &str| pi.and_then(|p| p.get(k)).and_then(Value::as_u64);
    let err = pi
        .and_then(|p| p.get("ErrorNumber"))
        .and_then(Value::as_i64)
        .filter(|e| *e != 0);
    PrinterStatus {
        state: if err.is_some() && state == MachineState::Idle {
            MachineState::Error
        } else {
            state
        },
        detail,
        filename: pi
            .and_then(|p| p.get("Filename"))
            .and_then(Value::as_str)
            .filter(|f| !f.is_empty())
            .map(str::to_string),
        current_layer: u("CurrentLayer").map(|x| x as u32),
        total_layers: u("TotalLayer").map(|x| x as u32),
        elapsed_s: u("CurrentTicks").map(|t| t / 1000),
        total_s: u("TotalTicks").map(|t| t / 1000),
        uv_led_temp: s.get("TempOfUVLED").and_then(Value::as_f64).map(|t| t as f32),
        box_temp: s.get("TempOfBox").and_then(Value::as_f64).map(|t| t as f32),
        error_code: err,
        free_storage: None,
        camera_connected: None,
        time_lapse: s.get("TimeLapseStatus").and_then(Value::as_i64).map(|v| v == 1),
    }
}

/// Minimal single-file HTTP server used for pull uploads.
struct FileServer {
    url: String,
    served: Arc<std::sync::atomic::AtomicU64>,
    task: JoinHandle<()>,
}

impl Drop for FileServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FileServer {
    async fn start(bind: IpAddr, path: std::path::PathBuf, name: String) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind(SocketAddr::new(bind, 0)).await?;
        let port = listener.local_addr()?.port();
        let token = uuid::Uuid::new_v4().simple().to_string();
        let route = format!("/{token}/{}", urlencode(&name));
        let url = format!("http://{bind}:{port}{route}");
        let served = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let served2 = served.clone();
        let task = tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let path = path.clone();
                let route = route.clone();
                let served = served2.clone();
                tokio::spawn(async move {
                    let mut req = vec![0u8; 4096];
                    let n = sock.read(&mut req).await.unwrap_or(0);
                    let head = String::from_utf8_lossy(&req[..n]).to_string();
                    let target = head.split_whitespace().nth(1).unwrap_or("");
                    if target != route {
                        let _ = sock
                            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                            .await;
                        return;
                    }
                    let Ok(mut f) = tokio::fs::File::open(&path).await else {
                        return;
                    };
                    let len = f.metadata().await.map(|m| m.len()).unwrap_or(0);
                    let start = head
                        .lines()
                        .find_map(|l| l.to_ascii_lowercase().strip_prefix("range: bytes=").map(str::to_string))
                        .and_then(|r| r.split('-').next().and_then(|s| s.trim().parse::<u64>().ok()))
                        .unwrap_or(0)
                        .min(len);
                    let hdr = if start > 0 {
                        format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{}/{len}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                            len - start,
                            len.saturating_sub(1)
                        )
                    } else {
                        format!("HTTP/1.1 200 OK\r\nContent-Length: {len}\r\nContent-Type: application/octet-stream\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n")
                    };
                    if sock.write_all(hdr.as_bytes()).await.is_err()
                        || f.seek(std::io::SeekFrom::Start(start)).await.is_err()
                    {
                        return;
                    }
                    let mut buf = vec![0u8; 64 * 1024];
                    let mut sent = start;
                    loop {
                        let r = match f.read(&mut buf).await {
                            Ok(0) | Err(_) => break,
                            Ok(r) => r,
                        };
                        if sock.write_all(&buf[..r]).await.is_err() {
                            break;
                        }
                        sent += r as u64;
                        served.fetch_max(sent, Ordering::Relaxed);
                    }
                });
            }
        });
        Ok(Self { url, served, task })
    }
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_mapping() {
        let v = json!({"Status": {"CurrentStatus": [1], "PrintInfo": {"Status": 6, "CurrentLayer": 50, "TotalLayer": 200, "CurrentTicks": 60000, "TotalTicks": 240000, "Filename": "a.goo", "ErrorNumber": 0}}, "Topic": "sdcp/status/x"});
        let s = parse_status(&v);
        assert_eq!(s.state, MachineState::Paused);
        assert_eq!(s.progress(), Some(0.25));
        assert_eq!(s.elapsed_s, Some(60));
        assert_eq!(s.filename.as_deref(), Some("a.goo"));
        let v = json!({"Status": {"CurrentStatus": 0, "PrintInfo": {"Status": 9}}});
        assert_eq!(parse_status(&v).state, MachineState::Completed);
    }

    #[test]
    fn video_urls() {
        let ip: IpAddr = "10.0.0.5".parse().unwrap();
        assert_eq!(
            normalize_video_url("10.0.0.5:554/video", ip),
            "rtsp://10.0.0.5:554/video"
        );
        assert_eq!(
            normalize_video_url("10.0.0.5:3031/video", ip),
            "http://10.0.0.5:3031/video"
        );
        assert_eq!(normalize_video_url("rtsp://${ipaddr}/live", ip), "rtsp://10.0.0.5/live");
    }
}
