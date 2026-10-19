//! Fake legacy Chitu WiFi mainboard: G-code over UDP on 127.0.0.1.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::net::UdpSocket;
use tokio::task::JoinHandle;

#[derive(Debug, Clone, PartialEq)]
pub struct Packet {
    pub offset: u32,
    pub len: usize,
    pub checksum_ok: bool,
}

#[derive(Default)]
pub struct ChituState {
    /// Text commands in arrival order (data packets excluded).
    pub commands: Vec<String>,
    pub packets: Vec<Packet>,
    pub files: BTreeMap<String, Vec<u8>>,
    /// Upload in progress (name, data) between M28 and M29.
    pub upload: Option<(String, Vec<u8>)>,
    pub printing: Option<String>,
    pub paused: bool,
    pub done: u64,
    pub total: u64,
    /// Never answer anything.
    pub silent: bool,
    /// Ignore the first data packet at these offsets (simulated loss).
    pub lose_offsets: Vec<u32>,
    /// Answer `resend <offset>` once for packets at these offsets.
    pub resend_offsets: Vec<u32>,
    /// Answer `resend <other>` for the packet at this offset.
    pub resend_other: Option<(u32, u32)>,
    /// Send the M20 listing as one datagram per line.
    pub m20_split: bool,
}

pub struct ChituMock {
    pub addr: SocketAddr,
    pub st: Arc<Mutex<ChituState>>,
    task: JoinHandle<()>,
}

impl Drop for ChituMock {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Text commands minus the status polling the client does on its own.
pub fn control_commands(cmds: &[String]) -> Vec<String> {
    cmds.iter()
        .filter(|c| !matches!(c.as_str(), "M4000" | "M27" | "M99999"))
        .cloned()
        .collect()
}

impl ChituMock {
    pub async fn start() -> Self {
        Self::start_with(|_| {}).await
    }

    pub async fn start_with(configure: impl FnOnce(&mut ChituState)) -> Self {
        let mut s = ChituState::default();
        configure(&mut s);
        let st = Arc::new(Mutex::new(s));
        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = sock.local_addr().unwrap();
        let st2 = st.clone();
        let task = tokio::spawn(async move {
            let mut b = vec![0u8; 8192];
            loop {
                // Windows reports ICMP "port unreachable" from earlier sends as recv errors
                let Ok((n, from)) = sock.recv_from(&mut b).await else {
                    continue;
                };
                let replies = handle(&st2, &b[..n]);
                for r in replies {
                    let _ = sock.send_to(r.as_bytes(), from).await;
                }
            }
        });
        ChituMock { addr, st, task }
    }

    pub fn state(&self) -> std::sync::MutexGuard<'_, ChituState> {
        self.st.lock().unwrap()
    }
}

fn handle(st: &Mutex<ChituState>, msg: &[u8]) -> Vec<String> {
    let mut s = st.lock().unwrap();
    if s.silent {
        return Vec::new();
    }
    // binary upload packet: payload | offset u32 LE | xor | 0x83
    if s.upload.is_some() && msg.len() > 6 && msg.ends_with(&[0x83]) {
        let n = msg.len();
        let payload = &msg[..n - 6];
        let offset = u32::from_le_bytes(msg[n - 6..n - 2].try_into().unwrap());
        let xor = msg[..n - 2].iter().fold(0u8, |a, b| a ^ b);
        let checksum_ok = xor == msg[n - 2];
        s.packets.push(Packet {
            offset,
            len: payload.len(),
            checksum_ok,
        });
        if let Some(pos) = s.lose_offsets.iter().position(|o| *o == offset) {
            s.lose_offsets.remove(pos);
            return Vec::new();
        }
        if let Some(pos) = s.resend_offsets.iter().position(|o| *o == offset) {
            s.resend_offsets.remove(pos);
            return vec![format!("resend {offset}")];
        }
        if let Some((at, other)) = s.resend_other {
            if at == offset {
                return vec![format!("resend {other}")];
            }
        }
        if !checksum_ok {
            return vec![format!("resend {offset}")];
        }
        let (_, data) = s.upload.as_mut().unwrap();
        let off = offset as usize;
        if data.len() < off + payload.len() {
            data.resize(off + payload.len(), 0);
        }
        data[off..off + payload.len()].copy_from_slice(payload);
        return vec![format!("ok {}", off + payload.len())];
    }
    let text = String::from_utf8_lossy(msg).trim().to_string();
    s.commands.push(text.clone());
    let (cmd, arg) = text
        .split_once(' ')
        .map(|(c, a)| (c, a.trim()))
        .unwrap_or((text.as_str(), ""));
    let reply = match cmd {
        "M99999" => "ok MAC:00:e0:4c:68:01:02 IP:127.0.0.1 VER:V1.4.3 ID:2a,3b,4c NAME:Mock Chitu".to_string(),
        "M4000" => format!(
            "ok B:0/0 X:0.000 Y:0.000 Z:12.500 F:256/256 D:{}/{}/{}",
            s.done, s.total, s.paused as u8
        ),
        "M27" => {
            if s.printing.is_some() {
                format!("SD printing byte {}/{}", s.done, s.total)
            } else {
                "Not SD printing.".to_string()
            }
        }
        "M20" => {
            let mut lines = vec!["Begin file list".to_string()];
            for (name, data) in &s.files {
                lines.push(format!("{name} {}", data.len()));
            }
            lines.push("End file list".to_string());
            lines.push("ok L:0".to_string());
            if s.m20_split {
                return lines;
            }
            lines.join("\r\n")
        }
        "M28" => {
            s.upload = Some((arg.to_string(), Vec::new()));
            "ok".to_string()
        }
        "M29" => {
            if let Some((name, data)) = s.upload.take() {
                s.files.insert(name, data);
            }
            "ok".to_string()
        }
        "M30" => {
            if s.files.remove(arg).is_some() {
                "ok".to_string()
            } else {
                format!("Deletion failed, File:{arg}.")
            }
        }
        "M6030" => {
            let name = arg.trim_matches('\'').trim_start_matches(':').to_string();
            match s.files.get(&name).map(|d| d.len() as u64) {
                Some(len) => {
                    s.printing = Some(name);
                    s.paused = false;
                    s.done = 0;
                    s.total = len;
                    "ok".to_string()
                }
                None => "Error: file not found".to_string(),
            }
        }
        "M25" => {
            s.paused = true;
            "ok".to_string()
        }
        "M24" => {
            s.paused = false;
            "ok".to_string()
        }
        "M33" => {
            s.printing = None;
            s.paused = false;
            s.done = 0;
            s.total = 0;
            "ok".to_string()
        }
        _ => "ok".to_string(),
    };
    vec![reply]
}
