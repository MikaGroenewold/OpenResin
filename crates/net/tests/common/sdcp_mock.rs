//! Fake SDCP V3 printer: WebSocket control channel and HTTP file server on one
//! TCP port (like port 3030 on the real mainboard) plus a UDP discovery
//! responder. All on 127.0.0.1.

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::{self, protocol::Role, Message};
use tokio_tungstenite::WebSocketStream;

use openresin_net::discovery::{parse_reply, DiscoveredPrinter};
use openresin_net::sdcp::SdcpClient;

use super::http::{header, parse_multipart, parse_range, percent_decode, read_body, read_head, respond};
use super::md5_hex;

pub const MAINBOARD_ID: &str = "000000000001d354";
pub const BRAND_ID: &str = "979d4C788A4a78bC777A870F1A02867A";

/// What the fake printer does with one WebSocket request.
pub enum Action {
    /// Answer with this `Data.Data` object.
    Reply(Value),
    /// Send this complete message instead of a proper response.
    ReplyRaw(Value),
    /// Never answer.
    NoReply,
    /// Drop the connection without answering (no close frame).
    Close,
    /// Answer after a delay (other requests are served meanwhile).
    Delay(Duration, Value),
}

/// Custom behaviour; returning `None` falls back to the default printer.
pub type Handler = Arc<dyn Fn(&mut MockState, u32, &Value) -> Option<Action> + Send + Sync>;

#[derive(Debug, Clone)]
pub struct Chunk {
    pub fields: Vec<String>,
    pub md5: String,
    pub check: String,
    pub offset: u64,
    pub uuid: String,
    pub total: u64,
    pub filename: Option<String>,
    pub content_type: Option<String>,
    pub len: usize,
}

#[derive(Debug, Clone)]
pub struct Completed {
    pub name: String,
    pub uuid: String,
    pub md5_ok: bool,
    pub len: usize,
}

#[derive(Debug, Clone)]
pub struct HttpLog {
    pub method: String,
    pub path: String,
    pub range: Option<String>,
    pub status: u16,
}

#[derive(Debug, Clone, Default)]
pub struct PullFetch {
    pub url: String,
    pub status: u16,
    pub data: Vec<u8>,
    pub md5_ok: bool,
    pub range_status: u16,
    pub content_range: Option<String>,
    pub range_data: Vec<u8>,
    pub wrong_token_status: u16,
}

pub struct MockState {
    pub protocol_version: String,
    /// Every JSON request received, in order.
    pub requests: Vec<Value>,
    /// Non-JSON text frames received (e.g. `ping`).
    pub texts: Vec<String>,
    pub status: Value,
    pub attributes: Value,
    /// Internal storage, keyed by path below `/local/`.
    pub files: BTreeMap<String, Vec<u8>>,
    /// Folders (full paths such as `/local/sub`).
    pub folders: Vec<String>,
    /// USB storage; `None` = no stick inserted.
    pub usb: Option<BTreeMap<String, Vec<u8>>>,
    pub history: Vec<String>,
    pub video_url: String,
    pub chunks: Vec<Chunk>,
    pub partial: HashMap<String, Vec<u8>>,
    pub completed: Vec<Completed>,
    pub http_log: Vec<HttpLog>,
    /// Number of upload POSTs seen (including dropped ones).
    pub posts: usize,
    /// Upload POST attempts (0-based) whose connection is dropped unanswered.
    pub drop_posts: Vec<usize>,
    /// (chunk index, HTTP status, body) to answer instead of accepting.
    pub reject_chunk: Option<(usize, u16, String)>,
    pub ignore_range: bool,
    pub ws_connections: usize,
    pub handler: Option<Handler>,
    pub pull_delay: Duration,
    pub pull_fetches: Vec<PullFetch>,
    /// FileTransferInfo.Status pushed after a pull upload (2 = done, 3 = failed).
    pub pull_result: i64,
    pub discovery_requests: usize,
    outbox: Vec<Value>,
}

fn now_s() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

pub fn idle_status() -> Value {
    json!({
        "CurrentStatus": [0],
        "PreviousStatus": 0,
        "TempOfUVLED": 28.5,
        "TempOfBox": 24.0,
        "TempTargetBox": 0,
        "TimeLapseStatus": 0,
        "PrintScreen": 0,
        "ReleaseFilm": 0,
        "PrintInfo": {
            "Status": 0, "CurrentLayer": 0, "TotalLayer": 0, "CurrentTicks": 0,
            "TotalTicks": 0, "Filename": "", "ErrorNumber": 0, "TaskId": ""
        },
        "FileTransferInfo": {"Status": 0, "DownloadOffset": 0, "CheckOffset": 0, "FileTotalSize": 0, "Filename": ""}
    })
}

pub fn default_attributes(protocol_version: &str) -> Value {
    json!({
        "Name": "Mock Mars",
        "MachineName": "ELEGOO Mars 5 Ultra",
        "BrandName": "ELEGOO",
        "ProtocolVersion": protocol_version,
        "FirmwareVersion": "V1.5.0",
        "Resolution": "8520x4320",
        "XYZsize": "153.36x77.76x165",
        "MainboardIP": "127.0.0.1",
        "MainboardID": MAINBOARD_ID,
        "NumberOfVideoStreamConnected": 0,
        "MaximumVideoStreamAllowed": 2,
        "NetworkStatus": "wlan",
        "UsbDiskStatus": 0,
        "Capabilities": ["FILE_TRANSFER", "PRINT_CONTROL", "VIDEO_STREAM"],
        "SupportFileType": ["GOO", "CTB"],
        "CameraStatus": 1,
        "RemainingMemory": 5_000_000_000u64
    })
}

impl MockState {
    fn new() -> Self {
        MockState {
            protocol_version: "V3.0.0".into(),
            requests: Vec::new(),
            texts: Vec::new(),
            status: idle_status(),
            attributes: default_attributes("V3.0.0"),
            files: BTreeMap::new(),
            folders: Vec::new(),
            usb: None,
            history: vec!["task-3".into(), "task-2".into(), "task-1".into()],
            video_url: "rtsp://${ipaddr}:554/video".into(),
            chunks: Vec::new(),
            partial: HashMap::new(),
            completed: Vec::new(),
            http_log: Vec::new(),
            posts: 0,
            drop_posts: Vec::new(),
            reject_chunk: None,
            ignore_range: false,
            ws_connections: 0,
            handler: None,
            pull_delay: Duration::ZERO,
            pull_fetches: Vec::new(),
            pull_result: 2,
            discovery_requests: 0,
            outbox: Vec::new(),
        }
    }

    /// Queues a status push (sent before the response to the current request).
    pub fn queue_status(&mut self) {
        self.outbox.push(status_message(&self.status));
    }

    pub fn queue_attributes(&mut self) {
        self.outbox.push(attributes_message(&self.attributes));
    }

    /// `Data.Data` of every request with command `cmd`.
    pub fn data_of(&self, cmd: u32) -> Vec<Value> {
        self.requests
            .iter()
            .filter(|r| r.pointer("/Data/Cmd").and_then(Value::as_u64) == Some(cmd as u64))
            .map(|r| r.pointer("/Data/Data").cloned().unwrap_or(Value::Null))
            .collect()
    }

    pub fn commands(&self) -> Vec<u32> {
        self.requests
            .iter()
            .filter_map(|r| r.pointer("/Data/Cmd").and_then(Value::as_u64).map(|c| c as u32))
            .collect()
    }
}

pub fn status_message(status: &Value) -> Value {
    json!({
        "Status": status,
        "MainboardID": MAINBOARD_ID,
        "TimeStamp": now_s(),
        "Topic": format!("sdcp/status/{MAINBOARD_ID}")
    })
}

pub fn attributes_message(attrs: &Value) -> Value {
    json!({
        "Attributes": attrs,
        "MainboardID": MAINBOARD_ID,
        "TimeStamp": now_s(),
        "Topic": format!("sdcp/attributes/{MAINBOARD_ID}")
    })
}

pub fn response_message(req: &Value, data: Value) -> Value {
    json!({
        "Id": BRAND_ID,
        "Data": {
            "Cmd": req.pointer("/Data/Cmd").cloned().unwrap_or(Value::Null),
            "Data": data,
            "RequestID": req.pointer("/Data/RequestID").cloned().unwrap_or(Value::Null),
            "MainboardID": MAINBOARD_ID,
            "TimeStamp": now_s()
        },
        "Topic": format!("sdcp/response/{MAINBOARD_ID}")
    })
}

struct PullJob {
    url: String,
    md5: String,
    filename: String,
    size: u64,
}

fn strip_local(name: &str) -> &str {
    name.strip_prefix("/local/").unwrap_or(name)
}

/// The behaviour of a well-behaved printer.
fn default_action(st: &mut MockState, cmd: u32, data: &Value) -> (Action, Option<PullJob>) {
    let ack = |a: i64| Action::Reply(json!({"Ack": a}));
    let a = match cmd {
        0 => {
            st.queue_status();
            ack(0)
        }
        1 => {
            st.queue_attributes();
            ack(0)
        }
        128 => {
            let f = data.get("Filename").and_then(Value::as_str).unwrap_or("").to_string();
            let busy = st.status.pointer("/CurrentStatus/0").and_then(Value::as_i64) == Some(1);
            if busy {
                ack(1)
            } else if !st.files.contains_key(strip_local(&f)) {
                ack(2)
            } else {
                st.status["CurrentStatus"] = json!([1]);
                st.status["PrintInfo"] = json!({
                    "Status": 3, "CurrentLayer": 0, "TotalLayer": 100, "CurrentTicks": 0,
                    "TotalTicks": 3_600_000, "Filename": f, "ErrorNumber": 0, "TaskId": "task-4"
                });
                st.queue_status();
                ack(0)
            }
        }
        129 => {
            st.status["PrintInfo"]["Status"] = json!(6);
            st.queue_status();
            ack(0)
        }
        131 => {
            st.status["PrintInfo"]["Status"] = json!(3);
            st.queue_status();
            ack(0)
        }
        130 => {
            st.status["CurrentStatus"] = json!([0]);
            st.status["PrintInfo"]["Status"] = json!(8);
            st.queue_status();
            ack(0)
        }
        192 => {
            st.attributes["Name"] = data.get("Name").cloned().unwrap_or(Value::Null);
            st.queue_attributes();
            ack(0)
        }
        255 => {
            let uuid = data.get("Uuid").and_then(Value::as_str).unwrap_or("");
            if st.partial.remove(uuid).is_some() {
                ack(0)
            } else {
                ack(1)
            }
        }
        256 => {
            let job = PullJob {
                url: data.get("URL").and_then(Value::as_str).unwrap_or("").to_string(),
                md5: data.get("MD5").and_then(Value::as_str).unwrap_or("").to_string(),
                filename: data.get("Filename").and_then(Value::as_str).unwrap_or("").to_string(),
                size: data.get("FileSize").and_then(Value::as_u64).unwrap_or(0),
            };
            return (ack(0), Some(job));
        }
        258 => {
            let url = data
                .get("Url")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim_end_matches('/')
                .to_string();
            let mut list = Vec::new();
            if url == "/usb" || url.starts_with("/usb/") {
                let Some(usb) = &st.usb else { return (ack(-1), None) };
                for name in usb.keys() {
                    list.push(json!({"name": format!("/usb/{name}"), "type": 1}));
                }
            } else if url == "/local" || url.starts_with("/local/") {
                for f in &st.folders {
                    if f.rsplit_once('/').map(|(parent, _)| parent) == Some(url.as_str()) {
                        list.push(json!({"name": f, "type": 0}));
                    }
                }
                for name in st.files.keys() {
                    let full = format!("/local/{name}");
                    if full.rsplit_once('/').map(|(parent, _)| parent) == Some(url.as_str()) {
                        list.push(json!({"name": full, "type": 1}));
                    }
                }
            } else {
                return (ack(-1), None);
            }
            Action::Reply(json!({"Ack": 0, "FileList": list}))
        }
        259 => {
            let mut err = Vec::new();
            for f in data
                .get("FileList")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                let f = f.as_str().unwrap_or("").to_string();
                if st.files.remove(strip_local(&f)).is_none() {
                    err.push(json!(f));
                }
            }
            for d in data
                .get("FolderList")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                let d = d.as_str().unwrap_or("").to_string();
                let before = st.folders.len();
                st.folders.retain(|x| *x != d);
                if st.folders.len() == before {
                    err.push(json!(d));
                }
            }
            Action::Reply(json!({"Ack": 0, "ErrData": err}))
        }
        320 => Action::Reply(json!({"Ack": 0, "HistoryData": st.history})),
        321 => {
            let ids = data.get("Id").and_then(Value::as_array).cloned().unwrap_or_default();
            let list: Vec<Value> = ids
                .iter()
                .map(|id| {
                    let id = id.as_str().unwrap_or("");
                    json!({
                        "TaskId": id,
                        "TaskName": format!("/local/{id}.goo"),
                        "BeginTime": 1_700_000_000u64,
                        "EndTime": 1_700_003_600u64,
                        "TaskStatus": 1,
                        "AlreadyPrintLayer": 100,
                        "SliceInformation": {"total_layer_numbers": 100}
                    })
                })
                .collect();
            Action::Reply(json!({"Ack": 0, "HistoryDetailList": list}))
        }
        386 => {
            if data.get("Enable").and_then(Value::as_i64) == Some(1) {
                Action::Reply(json!({"Ack": 0, "VideoUrl": st.video_url}))
            } else {
                ack(0)
            }
        }
        387 => {
            st.status["TimeLapseStatus"] = data.get("Enable").cloned().unwrap_or(json!(0));
            st.queue_status();
            ack(0)
        }
        _ => ack(0),
    };
    (a, None)
}

#[derive(Clone)]
enum Out {
    Msg(Message),
    Close,
}

pub struct SdcpMock {
    /// WebSocket + HTTP server (port 3030 on real printers).
    pub addr: SocketAddr,
    /// Discovery responder (port 3000 on real printers).
    pub udp_addr: SocketAddr,
    pub st: Arc<Mutex<MockState>>,
    out: broadcast::Sender<Out>,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for SdcpMock {
    fn drop(&mut self) {
        let _ = self.out.send(Out::Close);
        for t in &self.tasks {
            t.abort();
        }
    }
}

impl SdcpMock {
    pub async fn start() -> Self {
        Self::start_with(|_| {}).await
    }

    pub async fn start_with(configure: impl FnOnce(&mut MockState)) -> Self {
        let mut state = MockState::new();
        configure(&mut state);
        let pv = state.protocol_version.clone();
        state.attributes["ProtocolVersion"] = json!(pv);
        let st = Arc::new(Mutex::new(state));
        let (out, _) = broadcast::channel(256);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let udp_addr = udp.local_addr().unwrap();

        let tcp_task = {
            let st = st.clone();
            let out = out.clone();
            tokio::spawn(async move {
                while let Ok((sock, _)) = listener.accept().await {
                    tokio::spawn(serve_tcp(sock, st.clone(), out.clone()));
                }
            })
        };
        let udp_task = {
            let st = st.clone();
            tokio::spawn(async move {
                let mut b = [0u8; 2048];
                loop {
                    let Ok((n, from)) = udp.recv_from(&mut b).await else {
                        continue;
                    };
                    if &b[..n] == b"M99999" {
                        let reply = {
                            let mut s = st.lock().unwrap();
                            s.discovery_requests += 1;
                            discovery_json(&s.attributes)
                        };
                        let _ = udp.send_to(reply.to_string().as_bytes(), from).await;
                    }
                }
            })
        };
        SdcpMock {
            addr,
            udp_addr,
            st,
            out,
            tasks: vec![tcp_task, udp_task],
        }
    }

    pub fn state(&self) -> std::sync::MutexGuard<'_, MockState> {
        self.st.lock().unwrap()
    }

    pub fn set_handler(&self, h: impl Fn(&mut MockState, u32, &Value) -> Option<Action> + Send + Sync + 'static) {
        self.state().handler = Some(Arc::new(h));
    }

    /// The discovery reply this printer sends.
    pub fn discovery_reply(&self) -> Vec<u8> {
        discovery_json(&self.state().attributes).to_string().into_bytes()
    }

    /// What discovery reports for this printer.
    pub fn printer(&self) -> DiscoveredPrinter {
        let p = parse_reply(&self.discovery_reply(), self.addr.ip()).expect("discovery reply parses");
        assert!(p.ip.is_loopback());
        p
    }

    pub async fn client(&self) -> SdcpClient {
        SdcpClient::connect_with_port(&self.printer(), self.addr.port())
            .await
            .expect("connect to mock printer")
    }

    /// Modifies the status and pushes it to every connected client.
    pub fn push_status(&self, f: impl FnOnce(&mut Value)) {
        let msg = {
            let mut s = self.state();
            f(&mut s.status);
            status_message(&s.status)
        };
        self.push_json(&msg);
    }

    pub fn push_attributes(&self, f: impl FnOnce(&mut Value)) {
        let msg = {
            let mut s = self.state();
            f(&mut s.attributes);
            attributes_message(&s.attributes)
        };
        self.push_json(&msg);
    }

    pub fn push_json(&self, v: &Value) {
        let _ = self.out.send(Out::Msg(Message::Text(v.to_string())));
    }

    pub fn push_raw(&self, m: Message) {
        let _ = self.out.send(Out::Msg(m));
    }

    /// Drops every WebSocket connection without a close frame.
    pub fn close_all(&self) {
        let _ = self.out.send(Out::Close);
    }
}

fn discovery_json(attrs: &Value) -> Value {
    json!({
        "Id": BRAND_ID,
        "Data": {
            "Name": attrs["Name"],
            "MachineName": attrs["MachineName"],
            "BrandName": attrs["BrandName"],
            "MainboardIP": attrs["MainboardIP"],
            "MainboardID": attrs["MainboardID"],
            "ProtocolVersion": attrs["ProtocolVersion"],
            "FirmwareVersion": attrs["FirmwareVersion"]
        }
    })
}

async fn serve_tcp(mut sock: TcpStream, st: Arc<Mutex<MockState>>, out: broadcast::Sender<Out>) {
    let mut buf = Vec::new();
    loop {
        let Ok(Some((method, path, headers))) = read_head(&mut sock, &mut buf).await else {
            return;
        };
        if header(&headers, "Upgrade").is_some_and(|u| u.eq_ignore_ascii_case("websocket")) {
            if path != "/websocket" {
                let _ = respond(&mut sock, "404 Not Found", &[], b"").await;
                return;
            }
            let key = header(&headers, "Sec-WebSocket-Key").unwrap_or("").to_string();
            let accept = tungstenite::handshake::derive_accept_key(key.as_bytes());
            let resp = format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
            );
            if sock.write_all(resp.as_bytes()).await.is_err() {
                return;
            }
            assert!(buf.is_empty(), "client sent data before the handshake finished");
            let ws = WebSocketStream::from_raw_socket(sock, Role::Server, None).await;
            serve_ws(ws, st, out).await;
            return;
        }
        let Ok(body) = read_body(&mut sock, &mut buf, &headers).await else {
            return;
        };
        let range = header(&headers, "Range").map(str::to_string);
        if method == "POST" && path == "/uploadFile/upload" {
            let ct = header(&headers, "Content-Type").unwrap_or("").to_string();
            match handle_upload(&st, &ct, &body) {
                None => return, // drop the connection
                Some((status, json)) => {
                    st.lock().unwrap().http_log.push(HttpLog {
                        method,
                        path,
                        range,
                        status: code(&status),
                    });
                    if respond(
                        &mut sock,
                        &status,
                        &[("Content-Type", "application/json".into())],
                        json.as_bytes(),
                    )
                    .await
                    .is_err()
                    {
                        return;
                    }
                }
            }
        } else if method == "GET" && path.starts_with("/media/mmcblk0p3/") {
            let name = percent_decode(&path["/media/mmcblk0p3/".len()..]);
            let (data, ignore_range) = {
                let s = st.lock().unwrap();
                (s.files.get(&name).cloned(), s.ignore_range)
            };
            let (status, hdrs, body): (String, Vec<(&str, String)>, Vec<u8>) = match data {
                None => ("404 Not Found".into(), vec![], vec![]),
                Some(d) => match range.as_deref().filter(|_| !ignore_range) {
                    None => ("200 OK".into(), vec![("Accept-Ranges", "bytes".into())], d),
                    Some(r) => match parse_range(r, d.len() as u64) {
                        Ok((a, b)) => (
                            "206 Partial Content".into(),
                            vec![("Content-Range", format!("bytes {a}-{b}/{}", d.len()))],
                            d[a as usize..=b as usize].to_vec(),
                        ),
                        Err(()) => (
                            "416 Range Not Satisfiable".into(),
                            vec![("Content-Range", format!("bytes */{}", d.len()))],
                            vec![],
                        ),
                    },
                },
            };
            st.lock().unwrap().http_log.push(HttpLog {
                method,
                path,
                range,
                status: code(&status),
            });
            let mut hdrs = hdrs;
            hdrs.push(("Content-Type", "application/octet-stream".into()));
            if respond(&mut sock, &status, &hdrs, &body).await.is_err() {
                return;
            }
        } else {
            st.lock().unwrap().http_log.push(HttpLog {
                method,
                path,
                range,
                status: 404,
            });
            if respond(&mut sock, "404 Not Found", &[], b"").await.is_err() {
                return;
            }
        }
    }
}

fn code(status: &str) -> u16 {
    status
        .split_whitespace()
        .next()
        .and_then(|c| c.parse().ok())
        .unwrap_or(0)
}

/// Returns `None` to drop the connection, else (status line, JSON body).
fn handle_upload(st: &Mutex<MockState>, content_type: &str, body: &[u8]) -> Option<(String, String)> {
    let mut s = st.lock().unwrap();
    let attempt = s.posts;
    s.posts += 1;
    if s.drop_posts.contains(&attempt) {
        return None;
    }
    let parts = parse_multipart(content_type, body);
    let text = |n: &str| {
        parts
            .iter()
            .find(|p| p.name == n)
            .map(|p| String::from_utf8_lossy(&p.data).to_string())
            .unwrap_or_default()
    };
    let file = parts.iter().find(|p| p.name == "File");
    let chunk = Chunk {
        fields: parts.iter().map(|p| p.name.clone()).collect(),
        md5: text("S-File-MD5"),
        check: text("Check"),
        offset: text("Offset").parse().unwrap_or(u64::MAX),
        uuid: text("Uuid"),
        total: text("TotalSize").parse().unwrap_or(0),
        filename: file.and_then(|f| f.filename.clone()),
        content_type: file.and_then(|f| f.content_type.clone()),
        len: file.map(|f| f.data.len()).unwrap_or(0),
    };
    let index = s.chunks.len();
    s.chunks.push(chunk.clone());
    if let Some((i, status, resp)) = s.reject_chunk.clone() {
        if i == index {
            let line = format!("{status} {}", if status == 200 { "OK" } else { "Error" });
            return Some((line, resp));
        }
    }
    let Some(file) = file else {
        return Some((
            "200 OK".into(),
            json!({"success": false, "messages": [{"field": "File", "message": "missing"}]}).to_string(),
        ));
    };
    let partial = s.partial.entry(chunk.uuid.clone()).or_default();
    if chunk.offset > partial.len() as u64 {
        return Some((
            "200 OK".into(),
            json!({"success": false, "messages": [{"field": "common_field", "message": "-2"}]}).to_string(),
        ));
    }
    partial.truncate(chunk.offset as usize);
    partial.extend_from_slice(&file.data);
    if partial.len() as u64 >= chunk.total {
        let data = s.partial.remove(&chunk.uuid).unwrap();
        let md5_ok = md5_hex(&data) == chunk.md5;
        let name = chunk.filename.clone().unwrap_or_default();
        s.completed.push(Completed {
            name: name.clone(),
            uuid: chunk.uuid.clone(),
            md5_ok,
            len: data.len(),
        });
        if md5_ok {
            s.files.insert(name, data);
        }
    }
    Some(("200 OK".into(), json!({"success": true}).to_string()))
}

async fn serve_ws(ws: WebSocketStream<TcpStream>, st: Arc<Mutex<MockState>>, out: broadcast::Sender<Out>) {
    let (mut sink, mut stream) = ws.split();
    let mut rx = out.subscribe();
    let (late_tx, mut late_rx) = mpsc::unbounded_channel::<Message>();
    st.lock().unwrap().ws_connections += 1;
    loop {
        tokio::select! {
            m = stream.next() => {
                let text = match m {
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(Message::Binary(b))) => String::from_utf8_lossy(&b).into_owned(),
                    Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) | Some(Ok(Message::Frame(_))) => continue,
                    _ => break,
                };
                let Ok(req) = serde_json::from_str::<Value>(&text) else {
                    st.lock().unwrap().texts.push(text.clone());
                    if text == "ping" && sink.send(Message::Text("pong".into())).await.is_err() {
                        break;
                    }
                    continue;
                };
                let cmd = req.pointer("/Data/Cmd").and_then(Value::as_u64).unwrap_or(u64::MAX) as u32;
                let data = req.pointer("/Data/Data").cloned().unwrap_or(Value::Null);
                let (pushes, action, job) = {
                    let mut s = st.lock().unwrap();
                    s.requests.push(req.clone());
                    let custom = s.handler.clone().and_then(|h| h(&mut s, cmd, &data));
                    let (action, job) = match custom {
                        Some(a) => (a, None),
                        None => default_action(&mut s, cmd, &data),
                    };
                    (std::mem::take(&mut s.outbox), action, job)
                };
                let mut failed = false;
                for p in pushes {
                    failed |= sink.send(Message::Text(p.to_string())).await.is_err();
                }
                let reply = match action {
                    Action::Reply(d) => Some(response_message(&req, d)),
                    Action::ReplyRaw(v) => Some(v),
                    Action::NoReply => None,
                    Action::Close => break,
                    Action::Delay(d, v) => {
                        let tx = late_tx.clone();
                        let msg = response_message(&req, v);
                        tokio::spawn(async move {
                            tokio::time::sleep(d).await;
                            let _ = tx.send(Message::Text(msg.to_string()));
                        });
                        None
                    }
                };
                if let Some(r) = reply {
                    failed |= sink.send(Message::Text(r.to_string())).await.is_err();
                }
                if let Some(job) = job {
                    tokio::spawn(run_pull(job, st.clone(), out.clone()));
                }
                if failed {
                    break;
                }
            }
            o = rx.recv() => match o {
                Ok(Out::Msg(m)) => if sink.send(m).await.is_err() { break },
                Ok(Out::Close) | Err(broadcast::error::RecvError::Closed) => break,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
            },
            Some(m) = late_rx.recv() => if sink.send(m).await.is_err() { break },
        }
    }
}

/// Printer side of the pull upload (command 256): fetch the file from the URL
/// the client gave us, then report the outcome in a status push.
async fn run_pull(job: PullJob, st: Arc<Mutex<MockState>>, out: broadcast::Sender<Out>) {
    let delay = st.lock().unwrap().pull_delay;
    tokio::time::sleep(delay).await;
    let url: reqwest::Url = job.url.parse().expect("pull URL parses");
    let host_ok = url
        .host_str()
        .and_then(|h| h.parse::<std::net::IpAddr>().ok())
        .is_some_and(|ip| ip.is_loopback());
    assert!(
        host_ok,
        "client asked the printer to fetch from a non-loopback URL: {}",
        job.url
    );
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut fetch = PullFetch {
        url: job.url.clone(),
        ..Default::default()
    };
    if let Ok(r) = http.get(url.clone()).send().await {
        fetch.status = r.status().as_u16();
        fetch.data = r.bytes().await.map(|b| b.to_vec()).unwrap_or_default();
    }
    fetch.md5_ok = md5_hex(&fetch.data) == job.md5 && fetch.data.len() as u64 == job.size;
    if let Ok(r) = http.get(url.clone()).header("Range", "bytes=10-").send().await {
        fetch.range_status = r.status().as_u16();
        fetch.content_range = r
            .headers()
            .get("Content-Range")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        fetch.range_data = r.bytes().await.map(|b| b.to_vec()).unwrap_or_default();
    }
    let mut bad = url.clone();
    bad.set_path("/0000/not-the-token");
    if let Ok(r) = http.get(bad).send().await {
        fetch.wrong_token_status = r.status().as_u16();
    }
    let msg = {
        let mut s = st.lock().unwrap();
        let result = if fetch.md5_ok { s.pull_result } else { 3 };
        if result == 2 {
            s.files.insert(job.filename.clone(), fetch.data.clone());
        }
        s.status["FileTransferInfo"] = json!({
            "Status": result,
            "DownloadOffset": fetch.data.len(),
            "CheckOffset": fetch.data.len(),
            "FileTotalSize": job.size,
            "Filename": job.filename
        });
        s.pull_fetches.push(fetch);
        status_message(&s.status)
    };
    let _ = out.send(Out::Msg(Message::Text(msg.to_string())));
}
