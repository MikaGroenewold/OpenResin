//! Minimal, lenient RTSP client for H.264 cameras.
//!
//! Printer firmwares ship small RTSP servers that do not always follow the
//! RFCs to the letter (e.g. malformed header names), so this client parses
//! responses leniently. It supports RTP over UDP and TCP interleaving and
//! depacketizes H.264 (RFC 6184: single NAL units, STAP-A and FU-A) into
//! Annex B access units.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};

use crate::{NetError, Result};

const USER_AGENT: &str = "OpenResin";
const IO_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

enum Transport {
    Udp { rtp: UdpSocket, _rtcp: UdpSocket },
    Interleaved { channel: u8 },
}

/// An established RTSP session delivering H.264 access units.
pub struct RtspSession {
    ctrl: TcpStream,
    buf: Vec<u8>,
    url: String,
    session: String,
    cseq: u32,
    transport: Transport,
    /// SPS/PPS from the SDP, Annex B encoded.
    pub parameter_sets: Vec<u8>,
    depack: Depacketizer,
    keepalive_every: Duration,
    last_keepalive: tokio::time::Instant,
}

fn b64(s: &str) -> Option<Vec<u8>> {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        if c == b'=' {
            break;
        }
        let v = T.iter().position(|&x| x == c)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

fn parse_url(url: &str) -> Result<(String, u16)> {
    let rest = url
        .strip_prefix("rtsp://")
        .ok_or_else(|| NetError::Protocol(format!("not an rtsp URL: {url}")))?;
    let hostport = rest.split('/').next().unwrap_or("");
    let hostport = hostport.rsplit('@').next().unwrap_or(hostport);
    match hostport.rsplit_once(':') {
        Some((h, p)) if !h.contains(']') || h.ends_with(']') => {
            Ok((h.trim_matches(['[', ']']).to_string(), p.parse().unwrap_or(554)))
        }
        _ => Ok((hostport.trim_matches(['[', ']']).to_string(), 554)),
    }
}

impl RtspSession {
    /// Connects, negotiates the first H.264 video track and starts playback.
    pub async fn open(url: &str) -> Result<Self> {
        let (host, port) = parse_url(url)?;
        let ctrl = tokio::time::timeout(IO_TIMEOUT, TcpStream::connect((host.as_str(), port)))
            .await
            .map_err(|_| NetError::Timeout)??;
        ctrl.set_nodelay(true)?;
        let mut s = RtspSession {
            ctrl,
            buf: Vec::with_capacity(64 * 1024),
            url: url.to_string(),
            session: String::new(),
            cseq: 0,
            transport: Transport::Interleaved { channel: 0 },
            parameter_sets: Vec::new(),
            depack: Depacketizer::default(),
            keepalive_every: Duration::from_secs(25),
            last_keepalive: tokio::time::Instant::now(),
        };
        let desc = s.request("DESCRIBE", url, &[("Accept", "application/sdp")]).await?;
        if desc.status != 200 {
            return Err(NetError::Protocol(format!("RTSP DESCRIBE returned {}", desc.status)));
        }
        let base = desc
            .header("Content-Base")
            .or_else(|| desc.header("Content-Location"))
            .unwrap_or(url)
            .to_string();
        let sdp = String::from_utf8_lossy(&desc.body).to_string();
        let (control, sprop) = parse_sdp(&sdp)?;
        s.parameter_sets = sprop;
        let track_url = if control.starts_with("rtsp://") {
            control
        } else if control == "*" || control.is_empty() {
            base.clone()
        } else {
            format!("{}/{}", base.trim_end_matches('/'), control)
        };

        // try UDP first, then TCP interleaving
        let peer_ip: IpAddr = s.ctrl.peer_addr()?.ip();
        let (rtp, rtcp) = bind_rtp_pair(crate::bind_addr_for(peer_ip)).await?;
        let rtp_port = rtp.local_addr()?.port();
        let rtcp_port = rtcp.local_addr()?.port();
        // Spelled exactly like FFmpeg/live555 clients: some printer firmwares parse
        // this header positionally and misread other (valid) spellings.
        let udp_transport = format!("RTP/AVP/UDP;unicast;client_port={rtp_port}-{rtcp_port}");
        let force_tcp = std::env::var_os("OPENRESIN_RTSP_TCP").is_some();
        let setup = if force_tcp {
            Response {
                status: 0,
                headers: Vec::new(),
                body: Vec::new(),
            }
        } else {
            s.request("SETUP", &track_url, &[("Transport", &udp_transport)]).await?
        };
        if setup.status == 200 && setup.header("Transport").is_some_and(|t| !t.contains("interleaved")) {
            s.transport = Transport::Udp { rtp, _rtcp: rtcp };
            // punch through local firewalls/NAT by sending a packet to the server port
            if let Some(sp) = setup
                .header("Transport")
                .and_then(|t| t.split(';').find_map(|p| p.trim().strip_prefix("server_port=")))
                .and_then(|p| p.split('-').next()?.parse::<u16>().ok())
            {
                if let Transport::Udp { rtp, .. } = &s.transport {
                    let _ = rtp
                        .send_to(&[0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], SocketAddr::new(peer_ip, sp))
                        .await;
                }
            }
            s.set_session(&setup);
        } else {
            drop((rtp, rtcp));
            let tcp = s
                .request(
                    "SETUP",
                    &track_url,
                    &[("Transport", "RTP/AVP/TCP;unicast;interleaved=0-1")],
                )
                .await?;
            if tcp.status != 200 {
                return Err(NetError::Protocol(format!(
                    "RTSP SETUP returned {} / {}",
                    setup.status, tcp.status
                )));
            }
            let channel = tcp
                .header("Transport")
                .and_then(|t| t.split(';').find_map(|p| p.trim().strip_prefix("interleaved=")))
                .and_then(|p| p.split('-').next()?.parse::<u8>().ok())
                .unwrap_or(0);
            s.transport = Transport::Interleaved { channel };
            s.set_session(&tcp);
        }
        let session = s.session.clone();
        let play = s
            .request("PLAY", &base, &[("Session", &session), ("Range", "npt=0.000-")])
            .await?;
        if play.status != 200 {
            return Err(NetError::Protocol(format!("RTSP PLAY returned {}", play.status)));
        }
        Ok(s)
    }

    fn set_session(&mut self, r: &Response) {
        if let Some(sess) = r.header("Session") {
            let mut parts = sess.split(';');
            self.session = parts.next().unwrap_or("").trim().to_string();
            for p in parts {
                if let Some(t) = p.trim().strip_prefix("timeout=").and_then(|t| t.parse::<u64>().ok()) {
                    self.keepalive_every = Duration::from_secs((t / 2).clamp(5, 60));
                }
            }
        }
    }

    async fn send_request(&mut self, method: &str, url: &str, headers: &[(&str, &str)]) -> Result<()> {
        self.cseq += 1;
        let mut req = format!(
            "{method} {url} RTSP/1.0\r\nCSeq: {}\r\nUser-Agent: {USER_AGENT}\r\n",
            self.cseq
        );
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("\r\n");
        self.ctrl.write_all(req.as_bytes()).await?;
        Ok(())
    }

    async fn request(&mut self, method: &str, url: &str, headers: &[(&str, &str)]) -> Result<Response> {
        self.send_request(method, url, headers).await?;
        loop {
            match tokio::time::timeout(IO_TIMEOUT, self.read_item())
                .await
                .map_err(|_| NetError::Timeout)??
            {
                Item::Response(r) => {
                    if std::env::var_os("OPENRESIN_RTSP_DEBUG").is_some() {
                        eprintln!("RTSP {method} -> {} {:?}", r.status, r.headers);
                    }
                    return Ok(r);
                }
                Item::Interleaved(..) => continue,
            }
        }
    }

    async fn fill(&mut self) -> Result<()> {
        let mut tmp = [0u8; 16 * 1024];
        let n = self.ctrl.read(&mut tmp).await?;
        if n == 0 {
            return Err(NetError::Closed);
        }
        self.buf.extend_from_slice(&tmp[..n]);
        Ok(())
    }

    /// Reads either an RTSP response or an interleaved binary packet.
    async fn read_item(&mut self) -> Result<Item> {
        loop {
            if self.buf.first() == Some(&b'$') {
                if self.buf.len() >= 4 {
                    let ch = self.buf[1];
                    let len = u16::from_be_bytes([self.buf[2], self.buf[3]]) as usize;
                    if self.buf.len() >= 4 + len {
                        let pkt = self.buf[4..4 + len].to_vec();
                        self.buf.drain(..4 + len);
                        return Ok(Item::Interleaved(ch, pkt));
                    }
                }
            } else if let Some(end) = find(&self.buf, b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&self.buf[..end]).to_string();
                let mut lines = head.lines();
                let status_line = lines.next().unwrap_or("");
                let status = status_line
                    .split_whitespace()
                    .nth(1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                // lenient header parsing: split at the first ':' and trim both sides
                let headers: Vec<(String, String)> = lines
                    .filter_map(|l| l.split_once(':'))
                    .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                    .collect();
                let len = headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("Content-Length"))
                    .and_then(|(_, v)| v.parse::<usize>().ok())
                    .unwrap_or(0);
                if self.buf.len() >= end + 4 + len {
                    let body = self.buf[end + 4..end + 4 + len].to_vec();
                    self.buf.drain(..end + 4 + len);
                    if !status_line.starts_with("RTSP/") {
                        // a server request (e.g. GET_PARAMETER) or garbage: ignore
                        continue;
                    }
                    return Ok(Item::Response(Response { status, headers, body }));
                }
            } else if !self.buf.is_empty() && !self.buf.starts_with(b"RTSP") && !self.buf.starts_with(b"$") {
                // resynchronise on the next '$' or 'RTSP'
                let next = self
                    .buf
                    .iter()
                    .position(|&b| b == b'$')
                    .into_iter()
                    .chain(find(&self.buf, b"RTSP"))
                    .min()
                    .unwrap_or(self.buf.len());
                self.buf.drain(..next.max(1));
                continue;
            }
            self.fill().await?;
        }
    }

    /// Returns the next complete access unit (Annex B), including parameter
    /// sets in front of IDR frames.
    pub async fn next_access_unit(&mut self) -> Result<Vec<u8>> {
        let mut pkt = vec![0u8; 65536];
        loop {
            if self.last_keepalive.elapsed() > self.keepalive_every {
                self.last_keepalive = tokio::time::Instant::now();
                let session = self.session.clone();
                let url = self.url.clone();
                self.send_request("GET_PARAMETER", &url, &[("Session", &session)])
                    .await?;
            }
            let data: Vec<u8> = match &self.transport {
                Transport::Udp { rtp, .. } => {
                    let mut cbuf = [0u8; 2048];
                    tokio::select! {
                        r = rtp.recv_from(&mut pkt) => {
                            let (n, _) = r?;
                            pkt[..n].to_vec()
                        }
                        // drain control-channel responses (keepalive replies)
                        r = self.ctrl.read(&mut cbuf) => {
                            if r? == 0 {
                                return Err(NetError::Closed);
                            }
                            continue;
                        }
                        _ = tokio::time::sleep(Duration::from_secs(5)) => {
                            return Err(NetError::Timeout);
                        }
                    }
                }
                Transport::Interleaved { channel } => {
                    let channel = *channel;
                    match tokio::time::timeout(Duration::from_secs(10), self.read_item())
                        .await
                        .map_err(|_| NetError::Timeout)??
                    {
                        Item::Interleaved(ch, p) if ch == channel => p,
                        _ => continue,
                    }
                }
            };
            let debug = std::env::var_os("OPENRESIN_RTSP_DEBUG").is_some();
            if debug {
                eprintln!(
                    "RTP {} bytes m={} seq={} ts={} nal={:#04x} fu={:#04x} head={:02x?}",
                    data.len(),
                    data[1] >> 7,
                    u16::from_be_bytes([data[2], data[3]]),
                    u32::from_be_bytes([data[4], data[5], data[6], data[7]]),
                    data.get(12).copied().unwrap_or(0),
                    data.get(13).copied().unwrap_or(0),
                    &data[14.min(data.len())..30.min(data.len())]
                );
            }
            if let Some(mut au) = self.depack.push(&data) {
                if debug {
                    eprintln!("AU {} bytes idr={} sps={}", au.data.len(), au.idr, au.has_sps);
                }
                if au.idr && !self.parameter_sets.is_empty() && !au.has_sps {
                    let mut with = self.parameter_sets.clone();
                    with.append(&mut au.data);
                    return Ok(with);
                }
                if au.has_sps {
                    // remember in-band parameter sets for later IDR frames
                    self.parameter_sets = extract_parameter_sets(&au.data);
                }
                return Ok(au.data);
            }
        }
    }

    /// Ends the session (frees the viewer slot on the camera).
    pub async fn teardown(mut self) {
        let session = self.session.clone();
        let url = self.url.clone();
        let _ = self.send_request("TEARDOWN", &url, &[("Session", &session)]).await;
        let _ = tokio::time::timeout(Duration::from_millis(500), self.read_item()).await;
    }
}

enum Item {
    Response(Response),
    Interleaved(u8, Vec<u8>),
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Binds an even/odd UDP port pair for RTP/RTCP.
///
/// Ports are taken from 5000..30000 like FFmpeg does: some printer firmwares
/// store the client port in a signed 16-bit integer and silently fail to send
/// to ports above 32767.
async fn bind_rtp_pair(local: IpAddr) -> Result<(UdpSocket, UdpSocket)> {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    for i in 0..200u32 {
        let port = 5000 + ((seed.wrapping_add(i.wrapping_mul(7919)) % 12_500) * 2) as u16;
        let Ok(rtp) = UdpSocket::bind((local, port)).await else {
            continue;
        };
        if let Ok(rtcp) = UdpSocket::bind((local, port + 1)).await {
            return Ok((rtp, rtcp));
        }
    }
    Err(NetError::Protocol("no free UDP port pair for RTP".into()))
}

/// Finds the H.264 video track: returns (control attribute, Annex B parameter sets).
fn parse_sdp(sdp: &str) -> Result<(String, Vec<u8>)> {
    let mut in_video = false;
    let mut control = String::new();
    let mut params = Vec::new();
    let mut found = false;
    let mut is_h264 = false;
    for line in sdp.lines() {
        let line = line.trim();
        if let Some(m) = line.strip_prefix("m=") {
            if found && is_h264 {
                break;
            }
            in_video = m.starts_with("video");
            found = in_video;
            is_h264 = false;
            control.clear();
            params.clear();
            continue;
        }
        if !in_video {
            continue;
        }
        if let Some(c) = line.strip_prefix("a=control:") {
            control = c.trim().to_string();
        } else if let Some(r) = line.strip_prefix("a=rtpmap:") {
            is_h264 = r.to_ascii_uppercase().contains("H264");
        } else if let Some(f) = line.strip_prefix("a=fmtp:") {
            if let Some(sp) = f
                .split(';')
                .find_map(|p| p.trim().strip_prefix("sprop-parameter-sets="))
            {
                for ps in sp.split(',') {
                    if let Some(nal) = b64(ps.trim()) {
                        params.extend_from_slice(&[0, 0, 0, 1]);
                        params.extend_from_slice(&nal);
                    }
                }
            }
        }
    }
    if !found {
        return Err(NetError::Unsupported("camera SDP has no video track".into()));
    }
    if !is_h264 {
        return Err(NetError::Unsupported("camera video is not H.264".into()));
    }
    Ok((control, params))
}

fn extract_parameter_sets(annexb: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for nal in split_annexb(annexb) {
        if let Some(&h) = nal.first() {
            if matches!(h & 0x1F, 7 | 8) {
                out.extend_from_slice(&[0, 0, 0, 1]);
                out.extend_from_slice(nal);
            }
        }
    }
    out
}

fn split_annexb(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut start = None;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            if let Some(s) = start {
                let mut e = i;
                while e > s && data[e - 1] == 0 {
                    e -= 1;
                }
                out.push(&data[s..e]);
            }
            i += 3;
            start = Some(i);
        } else {
            i += 1;
        }
    }
    if let Some(s) = start {
        out.push(&data[s..]);
    }
    out
}

pub struct AccessUnit {
    pub data: Vec<u8>,
    pub idr: bool,
    pub has_sps: bool,
}

/// RFC 6184 depacketizer producing Annex B access units.
#[derive(Default)]
pub struct Depacketizer {
    current: Vec<u8>,
    timestamp: Option<u32>,
    last_seq: Option<u16>,
    broken: bool,
    fu_open: bool,
    raw_fu: bool,
    idr: bool,
    has_sps: bool,
}

impl Depacketizer {
    fn add_nal(&mut self, nal: &[u8]) {
        if nal.is_empty() {
            return;
        }
        match nal[0] & 0x1F {
            5 => self.idr = true,
            7 => self.has_sps = true,
            _ => {}
        }
        self.current.extend_from_slice(&[0, 0, 0, 1]);
        self.current.extend_from_slice(nal);
    }

    fn take(&mut self) -> Option<AccessUnit> {
        if self.raw_fu {
            // flags were not visible in the FU headers; scan the NAL units
            for nal in split_annexb(&self.current) {
                match nal.first().map(|h| h & 0x1F) {
                    Some(5) => self.idr = true,
                    Some(7) => self.has_sps = true,
                    _ => {}
                }
            }
        }
        let au = (!self.current.is_empty() && !self.broken).then(|| AccessUnit {
            data: std::mem::take(&mut self.current),
            idr: self.idr,
            has_sps: self.has_sps,
        });
        self.raw_fu = false;
        self.current.clear();
        self.broken = false;
        self.fu_open = false;
        self.idr = false;
        self.has_sps = false;
        au
    }

    /// Feeds one RTP packet; returns an access unit when one is complete.
    pub fn push(&mut self, pkt: &[u8]) -> Option<AccessUnit> {
        if pkt.len() < 12 || pkt[0] >> 6 != 2 {
            return None;
        }
        let padding = pkt[0] & 0x20 != 0;
        let ext = pkt[0] & 0x10 != 0;
        let cc = (pkt[0] & 0x0F) as usize;
        let marker = pkt[1] & 0x80 != 0;
        let seq = u16::from_be_bytes([pkt[2], pkt[3]]);
        let ts = u32::from_be_bytes([pkt[4], pkt[5], pkt[6], pkt[7]]);
        let mut off = 12 + cc * 4;
        if ext {
            if pkt.len() < off + 4 {
                return None;
            }
            let words = u16::from_be_bytes([pkt[off + 2], pkt[off + 3]]) as usize;
            off += 4 + words * 4;
        }
        let mut end = pkt.len();
        if padding {
            end = end.saturating_sub(*pkt.last()? as usize);
        }
        if off >= end {
            return None;
        }
        let payload = &pkt[off..end];

        let lost = self.last_seq.is_some_and(|last| seq != last.wrapping_add(1));
        let mut finished = None;
        if self.timestamp.is_some_and(|t| t != ts) && !self.current.is_empty() {
            // new frame started without a marker on the previous one; after a
            // gap its last packets (with the marker) may be what was lost
            self.broken |= lost;
            finished = self.take();
        }
        if lost {
            self.broken = true; // packet loss inside this access unit
        }
        self.last_seq = Some(seq);
        self.timestamp = Some(ts);

        // Quirk: some cameras send raw Annex B data (starting with a start code)
        // instead of RFC 6184 payloads.
        if payload.starts_with(&[0, 0, 1]) || payload.starts_with(&[0, 0, 0, 1]) {
            self.raw_fu = true;
            self.current.extend_from_slice(payload);
            return if marker {
                finished.or_else(|| self.take())
            } else {
                finished
            };
        }
        let nal_type = payload[0] & 0x1F;
        match nal_type {
            1..=23 => self.add_nal(payload),
            24 => {
                // STAP-A
                let mut i = 1;
                while i + 2 <= payload.len() {
                    let l = u16::from_be_bytes([payload[i], payload[i + 1]]) as usize;
                    i += 2;
                    if i + l > payload.len() {
                        break;
                    }
                    let nal = payload[i..i + l].to_vec();
                    self.add_nal(&nal);
                    i += l;
                }
            }
            28 => {
                // FU-A
                if payload.len() < 2 {
                    return finished;
                }
                let start = payload[1] & 0x80 != 0;
                let header = (payload[0] & 0xE0) | (payload[1] & 0x1F);
                let body = &payload[2..];
                if start {
                    // Quirk seen in printer cameras: the "fragments" carry a raw
                    // Annex B byte stream (with start codes and real NAL headers)
                    // and the FU header's type is 0. Pass such data through as is.
                    let raw = header & 0x1F == 0 && (body.starts_with(&[0, 0, 1]) || body.starts_with(&[0, 0, 0, 1]));
                    self.raw_fu |= raw;
                    if !raw {
                        match header & 0x1F {
                            5 => self.idr = true,
                            7 => self.has_sps = true,
                            _ => {}
                        }
                        self.current.extend_from_slice(&[0, 0, 0, 1, header]);
                    }
                    self.fu_open = true;
                } else if !self.fu_open {
                    self.broken = true;
                }
                self.current.extend_from_slice(body);
            }
            _ => {}
        }
        if marker {
            return finished.or_else(|| self.take());
        }
        finished
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rtp(seq: u16, ts: u32, marker: bool, payload: &[u8]) -> Vec<u8> {
        let mut p = vec![0x80, if marker { 0x80 | 96 } else { 96 }];
        p.extend_from_slice(&seq.to_be_bytes());
        p.extend_from_slice(&ts.to_be_bytes());
        p.extend_from_slice(&[0, 0, 0, 1]);
        p.extend_from_slice(payload);
        p
    }

    #[test]
    fn fu_a_and_stap_a() {
        let mut d = Depacketizer::default();
        // STAP-A with SPS + PPS
        let stap = [24, 0, 2, 0x67, 0xAA, 0, 1, 0x68];
        assert!(d.push(&rtp(1, 100, false, &stap)).is_none());
        // FU-A IDR in two fragments
        assert!(d.push(&rtp(2, 100, false, &[0x7C, 0x85, 1, 2])).is_none());
        let au = d.push(&rtp(3, 100, true, &[0x7C, 0x45, 3])).unwrap();
        assert!(au.idr && au.has_sps);
        assert_eq!(
            au.data,
            vec![0, 0, 0, 1, 0x67, 0xAA, 0, 0, 0, 1, 0x68, 0, 0, 0, 1, 0x65, 1, 2, 3]
        );
        // packet loss drops the frame
        assert!(d.push(&rtp(5, 200, true, &[0x41, 9])).is_none());
        let au = d.push(&rtp(6, 300, true, &[0x41, 7])).unwrap();
        assert_eq!(au.data, vec![0, 0, 0, 1, 0x41, 7]);
    }

    #[test]
    fn raw_annexb_in_fu_quirk() {
        let mut d = Depacketizer::default();
        // plain Annex B packets (parameter sets) followed by fake FU fragments
        assert!(d.push(&rtp(0, 10, false, &[0, 0, 0, 1, 0x68, 0xEE])).is_none());
        // fake FU-A (type 0) carrying "00 00 01 67 .. 00 00 01 65 .."
        assert!(d
            .push(&rtp(1, 10, false, &[0x1c, 0x80, 0, 0, 1, 0x67, 0x42, 0, 0, 1, 0x65]))
            .is_none());
        assert!(d.push(&rtp(2, 10, false, &[0x1c, 0x40, 0x88, 0x99])).is_none());
        let au = d.push(&rtp(3, 20, false, &[0x1c, 0x80, 0, 0, 1, 0x41, 1])).unwrap();
        assert!(au.idr && au.has_sps);
        assert_eq!(
            au.data,
            vec![0, 0, 0, 1, 0x68, 0xEE, 0, 0, 1, 0x67, 0x42, 0, 0, 1, 0x65, 0x88, 0x99]
        );
    }

    #[test]
    fn sdp_and_base64() {
        let sdp = "v=0\r\nm=audio 0 RTP/AVP 0\r\na=control:track0\r\nm=video 0 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\na=fmtp:96 packetization-mode=1;sprop-parameter-sets=Z0IAH5WoFAFuQA==,aM48gA==\r\na=control:track1\r\n";
        let (c, p) = parse_sdp(sdp).unwrap();
        assert_eq!(c, "track1");
        assert_eq!(&p[..5], &[0, 0, 0, 1, 0x67]);
        assert_eq!(b64("aM48gA==").unwrap(), vec![0x68, 0xCE, 0x3C, 0x80]);
        assert_eq!(
            parse_url("rtsp://10.0.0.2:554/video").unwrap(),
            ("10.0.0.2".into(), 554)
        );
        assert_eq!(parse_url("rtsp://cam/x").unwrap(), ("cam".into(), 554));
    }
}
