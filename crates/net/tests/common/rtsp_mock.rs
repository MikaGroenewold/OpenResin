//! Fake printer camera: RTSP on 127.0.0.1 streaming real H.264 over RTP/UDP
//! (or TCP interleaved), with the server quirks documented in sdcp.md.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::task::JoinHandle;

use super::h264::{base64, encode_sequence, nal_type, packetize, rtp_packet, Packetization};
use super::http::{header, read_request};

#[derive(Debug, Clone)]
pub struct RtspConfig {
    pub mode: Packetization,
    /// SPS/PPS in the SDP (`sprop-parameter-sets`).
    pub sprop_in_sdp: bool,
    /// SPS/PPS sent in-band before each IDR frame.
    pub inband_params: bool,
    /// Refuse UDP transport (461) so the client must use TCP interleaving.
    pub tcp_only: bool,
    /// Set the RTP marker bit on the last packet of each frame.
    pub marker: bool,
    pub mtu: usize,
    pub content_base: bool,
    /// Malformed header lines like the printer firmware (`Data :...`).
    pub quirky_headers: bool,
    pub frame_interval: Duration,
    pub frames: usize,
    pub session_timeout: Option<u64>,
    /// Close the first connection after sending this many frames.
    pub close_after_frames: Option<usize>,
    pub describe_status: u16,
    pub sdp_override: Option<String>,
    /// First RTP sequence number (close to 65535 to exercise wrap-around).
    pub first_seq: u16,
}

impl Default for RtspConfig {
    fn default() -> Self {
        RtspConfig {
            mode: Packetization::RawAnnexB,
            sprop_in_sdp: false,
            inband_params: true,
            tcp_only: false,
            marker: true,
            mtu: 1000,
            content_base: true,
            quirky_headers: true,
            frame_interval: Duration::from_millis(80),
            frames: 10,
            session_timeout: Some(60),
            close_after_frames: None,
            describe_status: 200,
            sdp_override: None,
            first_seq: 65_530,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RtspRequest {
    pub conn: usize,
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
}

impl RtspRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        header(&self.headers, name)
    }
}

#[derive(Debug, Default)]
pub struct RtspLog {
    pub requests: Vec<RtspRequest>,
    pub connections: usize,
    pub frames_sent: usize,
    /// Packets received on the server RTP port (client NAT punching).
    pub punches: usize,
    pub client_ports: Vec<(u16, u16)>,
    pub teardowns: Vec<String>,
    pub active_streams: usize,
}

impl RtspLog {
    pub fn methods(&self) -> Vec<String> {
        self.requests.iter().map(|r| r.method.clone()).collect()
    }
}

pub struct RtspMock {
    pub addr: SocketAddr,
    /// `rtsp://127.0.0.1:<port>/video`
    pub url: String,
    pub log: Arc<Mutex<RtspLog>>,
    pub sps: Vec<u8>,
    pub pps: Vec<u8>,
    /// The encoded access units being streamed (NAL units, SPS/PPS included).
    pub aus: Vec<Vec<Vec<u8>>>,
    task: JoinHandle<()>,
}

impl Drop for RtspMock {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub const SESSION_BASE: u32 = 0x1234_5670;

impl RtspMock {
    pub async fn start(cfg: RtspConfig) -> Self {
        let frames = cfg.frames;
        let aus = tokio::task::spawn_blocking(move || encode_sequence(frames))
            .await
            .unwrap();
        let sps = aus[0].iter().find(|n| nal_type(n) == 7).unwrap().clone();
        let pps = aus[0].iter().find(|n| nal_type(n) == 8).unwrap().clone();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("rtsp://{addr}/video");
        let log = Arc::new(Mutex::new(RtspLog::default()));
        let shared = Arc::new(Shared {
            cfg,
            aus: aus.clone(),
            sps: sps.clone(),
            pps: pps.clone(),
            url: url.clone(),
            log: log.clone(),
        });
        let task = tokio::spawn(async move {
            let mut conn = 0;
            while let Ok((sock, _)) = listener.accept().await {
                shared.log.lock().unwrap().connections += 1;
                tokio::spawn(serve(sock, conn, shared.clone()));
                conn += 1;
            }
        });
        RtspMock {
            addr,
            url,
            log,
            sps,
            pps,
            aus,
            task,
        }
    }

    pub fn log(&self) -> std::sync::MutexGuard<'_, RtspLog> {
        self.log.lock().unwrap()
    }

    pub fn session_id(conn: usize) -> String {
        format!("{:08X}", SESSION_BASE + conn as u32)
    }
}

struct Shared {
    cfg: RtspConfig,
    aus: Vec<Vec<Vec<u8>>>,
    sps: Vec<u8>,
    pps: Vec<u8>,
    url: String,
    log: Arc<Mutex<RtspLog>>,
}

enum Target {
    Udp(Arc<UdpSocket>, SocketAddr),
    Tcp(Arc<tokio::sync::Mutex<OwnedWriteHalf>>, u8),
}

async fn reply(
    wr: &tokio::sync::Mutex<OwnedWriteHalf>,
    sh: &Shared,
    status: &str,
    cseq: &str,
    extra: &[(&str, String)],
    body: &[u8],
) -> std::io::Result<()> {
    let date = if sh.cfg.quirky_headers {
        "Data :Sun, Oct 18 2026 10:00:00 GMT"
    } else {
        "Date: Sun, Oct 18 2026 10:00:00 GMT"
    };
    let mut r = format!("RTSP/1.0 {status}\r\nCSeq: {cseq}\r\n{date}\r\nServer: MockCam/1.0\r\n");
    for (k, v) in extra {
        r.push_str(&format!("{k}: {v}\r\n"));
    }
    if !body.is_empty() {
        r.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    r.push_str("\r\n");
    let mut w = wr.lock().await;
    w.write_all(r.as_bytes()).await?;
    w.write_all(body).await?;
    w.flush().await
}

fn sdp(sh: &Shared) -> String {
    if let Some(s) = &sh.cfg.sdp_override {
        return s.clone();
    }
    let sprop = if sh.cfg.sprop_in_sdp {
        format!(";sprop-parameter-sets={},{}", base64(&sh.sps), base64(&sh.pps))
    } else {
        String::new()
    };
    format!(
        "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=Mock camera\r\nt=0 0\r\na=control:*\r\n\
         m=audio 0 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\na=control:audio\r\n\
         m=video 0 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\n\
         a=fmtp:96 packetization-mode=1;profile-level-id=42C01E{sprop}\r\na=control:track1\r\n"
    )
}

async fn serve(sock: TcpStream, conn: usize, sh: Arc<Shared>) {
    let (mut rd, wr) = sock.into_split();
    let wr = Arc::new(tokio::sync::Mutex::new(wr));
    let session = RtspMock::session_id(conn);
    let rtp = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let server_port = rtp.local_addr().unwrap().port();
    let punch_task = {
        let rtp = rtp.clone();
        let log = sh.log.clone();
        tokio::spawn(async move {
            let mut b = [0u8; 2048];
            loop {
                if rtp.recv_from(&mut b).await.is_ok() {
                    log.lock().unwrap().punches += 1;
                }
            }
        })
    };
    let mut target: Option<Target> = None;
    let mut stream: Option<JoinHandle<()>> = None;
    let mut buf = Vec::new();
    while let Ok(Some(req)) = read_request(&mut rd, &mut buf).await {
        let cseq = req.header("CSeq").unwrap_or("0").to_string();
        sh.log.lock().unwrap().requests.push(RtspRequest {
            conn,
            method: req.method.clone(),
            url: req.path.clone(),
            headers: req.headers.clone(),
        });
        let sess_hdr = match sh.cfg.session_timeout {
            Some(t) => format!("{session};timeout={t}"),
            None => session.clone(),
        };
        let r = match req.method.as_str() {
            "OPTIONS" => {
                reply(
                    &wr,
                    &sh,
                    "200 OK",
                    &cseq,
                    &[(
                        "Public",
                        "OPTIONS, DESCRIBE, SETUP, TEARDOWN, PLAY, GET_PARAMETER".into(),
                    )],
                    b"",
                )
                .await
            }
            "DESCRIBE" => {
                if sh.cfg.describe_status != 200 {
                    reply(&wr, &sh, &format!("{} Error", sh.cfg.describe_status), &cseq, &[], b"").await
                } else {
                    let mut extra = vec![("Content-Type", "application/sdp".to_string())];
                    if sh.cfg.content_base {
                        extra.push(("Content-Base", format!("{}/", sh.url)));
                    }
                    reply(&wr, &sh, "200 OK", &cseq, &extra, sdp(&sh).as_bytes()).await
                }
            }
            "SETUP" => {
                let t = req.header("Transport").unwrap_or("").to_string();
                if t.contains("interleaved") {
                    target = Some(Target::Tcp(wr.clone(), 0));
                    reply(
                        &wr,
                        &sh,
                        "200 OK",
                        &cseq,
                        &[
                            ("Transport", "RTP/AVP/TCP;unicast;interleaved=0-1".into()),
                            ("Session", sess_hdr),
                        ],
                        b"",
                    )
                    .await
                } else if sh.cfg.tcp_only {
                    reply(&wr, &sh, "461 Unsupported Transport", &cseq, &[], b"").await
                } else {
                    let ports = t
                        .split(';')
                        .find_map(|p| p.trim().strip_prefix("client_port="))
                        .and_then(|p| p.split_once('-'))
                        .and_then(|(a, b)| Some((a.parse::<u16>().ok()?, b.parse::<u16>().ok()?)));
                    match ports {
                        Some((a, b)) => {
                            sh.log.lock().unwrap().client_ports.push((a, b));
                            target = Some(Target::Udp(rtp.clone(), SocketAddr::from(([127, 0, 0, 1], a))));
                            let tr = format!(
                                "RTP/AVP;unicast;client_port={a}-{b};server_port={server_port}-{};ssrc=11223344",
                                server_port + 1
                            );
                            reply(
                                &wr,
                                &sh,
                                "200 OK",
                                &cseq,
                                &[("Transport", tr), ("Session", sess_hdr)],
                                b"",
                            )
                            .await
                        }
                        None => reply(&wr, &sh, "461 Unsupported Transport", &cseq, &[], b"").await,
                    }
                }
            }
            "PLAY" => {
                let r = reply(
                    &wr,
                    &sh,
                    "200 OK",
                    &cseq,
                    &[("Session", session.clone()), ("Range", "npt=0.000-".into())],
                    b"",
                )
                .await;
                if let Some(t) = target.take() {
                    if let Some(s) = stream.take() {
                        s.abort();
                    }
                    stream = Some(tokio::spawn(send_stream(sh.clone(), t, wr.clone(), conn)));
                }
                r
            }
            "TEARDOWN" => {
                if let Some(s) = stream.take() {
                    s.abort();
                }
                sh.log
                    .lock()
                    .unwrap()
                    .teardowns
                    .push(req.header("Session").unwrap_or("").to_string());
                reply(&wr, &sh, "200 OK", &cseq, &[("Session", session.clone())], b"").await
            }
            "GET_PARAMETER" => reply(&wr, &sh, "200 OK", &cseq, &[("Session", session.clone())], b"").await,
            _ => reply(&wr, &sh, "501 Not Implemented", &cseq, &[], b"").await,
        };
        if r.is_err() {
            break;
        }
    }
    if let Some(s) = stream.take() {
        s.abort();
    }
    punch_task.abort();
}

/// Counts running streams; decremented when the stream task ends or is aborted.
struct Active(Arc<Mutex<RtspLog>>);

impl Drop for Active {
    fn drop(&mut self) {
        if let Ok(mut l) = self.0.lock() {
            l.active_streams -= 1;
        }
    }
}

async fn send_stream(sh: Arc<Shared>, target: Target, ctrl: Arc<tokio::sync::Mutex<OwnedWriteHalf>>, conn: usize) {
    sh.log.lock().unwrap().active_streams += 1;
    let _active = Active(sh.log.clone());
    let mut seq = sh.cfg.first_seq;
    let mut frame: u32 = 0;
    let mut sent_frames = 0usize;
    loop {
        for au in &sh.aus {
            let nals: Vec<Vec<u8>> = au
                .iter()
                .filter(|n| sh.cfg.inband_params || !matches!(nal_type(n), 7 | 8))
                .cloned()
                .collect();
            let payloads = packetize(&nals, sh.cfg.mode, sh.cfg.mtu);
            let ts = frame.wrapping_mul(9000);
            for (i, p) in payloads.iter().enumerate() {
                let marker = sh.cfg.marker && i + 1 == payloads.len();
                let pkt = rtp_packet(seq, ts, 0x1122_3344, marker, p);
                seq = seq.wrapping_add(1);
                let ok = match &target {
                    Target::Udp(s, to) => s.send_to(&pkt, to).await.is_ok(),
                    Target::Tcp(w, ch) => {
                        let mut w = w.lock().await;
                        let mut framed = vec![b'$', *ch];
                        framed.extend_from_slice(&(pkt.len() as u16).to_be_bytes());
                        framed.extend_from_slice(&pkt);
                        w.write_all(&framed).await.is_ok()
                    }
                };
                if !ok {
                    return;
                }
                // pace bursts so small loopback socket buffers do not overflow
                if i % 8 == 7 {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            }
            frame += 1;
            sent_frames += 1;
            sh.log.lock().unwrap().frames_sent += 1;
            if conn == 0 && sh.cfg.close_after_frames.is_some_and(|n| sent_frames >= n) {
                // the camera goes away: close the control connection
                let _ = ctrl.lock().await.shutdown().await;
                return;
            }
            tokio::time::sleep(sh.cfg.frame_interval).await;
        }
    }
}
