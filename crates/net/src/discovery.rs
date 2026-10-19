//! LAN discovery: UDP broadcast of `M99999` on port 3000.
//!
//! SDCP mainboards answer with a JSON document, older Chitu WiFi boards with a
//! single text line of `KEY:VALUE` tokens.

use std::collections::BTreeMap;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::net::UdpSocket;
use tokio::time::Instant;

use crate::Result;

pub const DISCOVERY_PORT: u16 = 3000;
pub const DISCOVERY_MESSAGE: &[u8] = b"M99999";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Protocol {
    Sdcp,
    ChituUdp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiscoveredPrinter {
    pub ip: IpAddr,
    pub protocol: Protocol,
    pub name: String,
    pub model: Option<String>,
    pub brand: Option<String>,
    /// SDCP brand identifier (`Id`), echoed in requests.
    pub id: Option<String>,
    pub mainboard_id: Option<String>,
    pub protocol_version: Option<String>,
    pub firmware_version: Option<String>,
    /// Remaining raw fields for diagnostics.
    pub extra: BTreeMap<String, String>,
}

impl fmt::Display for DiscoveredPrinter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:<15} {:<10} {:<24} {}",
            self.ip,
            match self.protocol {
                Protocol::Sdcp => format!("SDCP {}", self.protocol_version.as_deref().unwrap_or("")),
                Protocol::ChituUdp => "Chitu UDP".to_string(),
            },
            self.name,
            self.model.as_deref().unwrap_or("")
        )
    }
}

/// Parses a discovery reply from `from`.
pub fn parse_reply(data: &[u8], from: IpAddr) -> Option<DiscoveredPrinter> {
    let text = String::from_utf8_lossy(data);
    let text = text.trim_matches(|c: char| c.is_whitespace() || c == '\0');
    if text.starts_with('{') {
        let v: Value = serde_json::from_str(text).ok()?;
        let d = v.get("Data").unwrap_or(&v);
        // older SDCP firmwares nest the device fields in `Data.Attributes`
        let d = d.get("Attributes").filter(|a| a.is_object()).unwrap_or(d);
        let s = |k: &str| d.get(k).and_then(|x| x.as_str()).map(str::to_string);
        if s("MainboardID").is_none() && s("Name").is_none() && s("MachineName").is_none() {
            return None; // some other JSON speaker, not a printer
        }
        let ip = s("MainboardIP").and_then(|i| i.parse().ok()).unwrap_or(from);
        let mut extra = BTreeMap::new();
        if let Some(obj) = d.as_object() {
            for (k, val) in obj {
                if !matches!(
                    k.as_str(),
                    "Name"
                        | "MachineName"
                        | "BrandName"
                        | "MainboardIP"
                        | "MainboardID"
                        | "ProtocolVersion"
                        | "FirmwareVersion"
                ) {
                    extra.insert(k.clone(), val.to_string());
                }
            }
        }
        return Some(DiscoveredPrinter {
            ip,
            protocol: Protocol::Sdcp,
            name: s("Name")
                .or_else(|| s("MachineName"))
                .unwrap_or_else(|| "Printer".into()),
            model: s("MachineName"),
            brand: s("BrandName"),
            id: v.get("Id").and_then(|x| x.as_str()).map(str::to_string),
            mainboard_id: s("MainboardID"),
            protocol_version: s("ProtocolVersion"),
            firmware_version: s("FirmwareVersion"),
            extra,
        });
    }
    // Legacy: "ok MAC:00:e0:4c:... IP:192.168.1.10 VER:V1.4.1 ID:xx NAME:CBD"
    let body = text.strip_prefix("ok").unwrap_or(text).trim();
    if body.is_empty() {
        return None;
    }
    let mut fields = BTreeMap::new();
    let mut key: Option<String> = None;
    for tok in body.split_whitespace() {
        match tok.split_once(':') {
            Some((k, v)) if k.chars().all(|c| c.is_ascii_uppercase() || c == '_') && !k.is_empty() => {
                fields.insert(k.to_string(), v.to_string());
                key = Some(k.to_string());
            }
            _ => {
                // value continuation (names with spaces)
                if let Some(k) = &key {
                    let e = fields.entry(k.clone()).or_default();
                    e.push(' ');
                    e.push_str(tok);
                }
            }
        }
    }
    if !["MAC", "IP", "NAME", "ID"].iter().any(|k| fields.contains_key(*k)) {
        return None;
    }
    let ip = fields.get("IP").and_then(|i| i.parse().ok()).unwrap_or(from);
    Some(DiscoveredPrinter {
        ip,
        protocol: Protocol::ChituUdp,
        name: fields.get("NAME").cloned().unwrap_or_else(|| "Chitu printer".into()),
        model: fields.get("MODEL").cloned(),
        brand: None,
        id: fields.get("ID").cloned(),
        mainboard_id: fields.get("ID").cloned(),
        protocol_version: None,
        firmware_version: fields.get("VER").cloned(),
        extra: fields,
    })
}

fn broadcast_targets() -> Vec<SocketAddr> {
    let mut out = vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), DISCOVERY_PORT)];
    if let Ok(ifs) = if_addrs::get_if_addrs() {
        for i in ifs {
            if let if_addrs::IfAddr::V4(v4) = i.addr {
                if v4.ip.is_loopback() {
                    continue;
                }
                let b = v4.broadcast.unwrap_or_else(|| {
                    let ip = u32::from(v4.ip);
                    let mask = u32::from(v4.netmask);
                    Ipv4Addr::from(ip | !mask)
                });
                let a = SocketAddr::new(IpAddr::V4(b), DISCOVERY_PORT);
                if !out.contains(&a) {
                    out.push(a);
                }
            }
        }
    }
    out
}

/// Broadcasts a discovery request and collects replies for `timeout`.
pub async fn discover(timeout: Duration) -> Result<Vec<DiscoveredPrinter>> {
    discover_on(&broadcast_targets(), timeout).await
}

/// Sends the discovery request to explicit `targets` (broadcast or unicast
/// addresses, e.g. of other subnets) and collects replies for `timeout`.
pub async fn discover_on(targets: &[SocketAddr], timeout: Duration) -> Result<Vec<DiscoveredPrinter>> {
    let local = if !targets.is_empty() && targets.iter().all(|t| t.ip().is_loopback()) {
        Ipv4Addr::LOCALHOST
    } else {
        Ipv4Addr::UNSPECIFIED
    };
    let sock = UdpSocket::bind((local, 0)).await?;
    sock.set_broadcast(true)?;
    collect(&sock, targets, timeout, false).await
}

/// Sends a discovery request directly to one host (useful across subnets/VPNs).
pub async fn discover_host(ip: IpAddr, timeout: Duration) -> Result<Option<DiscoveredPrinter>> {
    discover_addr(SocketAddr::new(ip, DISCOVERY_PORT), timeout).await
}

/// Like [`discover_host`] for a non-standard port; returns on the first reply.
pub async fn discover_addr(addr: SocketAddr, timeout: Duration) -> Result<Option<DiscoveredPrinter>> {
    let sock = UdpSocket::bind((crate::bind_addr_for(addr.ip()), 0)).await?;
    let found = collect(&sock, &[addr], timeout, true).await?;
    Ok(found.into_iter().next())
}

async fn collect(
    sock: &UdpSocket,
    targets: &[SocketAddr],
    timeout: Duration,
    first_only: bool,
) -> Result<Vec<DiscoveredPrinter>> {
    let deadline = Instant::now() + timeout;
    let mut found: Vec<DiscoveredPrinter> = Vec::new();
    let mut next_send = Instant::now();
    let mut buf = vec![0u8; 4096];
    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        if now >= next_send {
            for t in targets {
                // individual interfaces may refuse broadcasts; that is fine
                let _ = sock.send_to(DISCOVERY_MESSAGE, t).await;
            }
            next_send = now + Duration::from_millis(1000);
        }
        let wait = deadline.min(next_send).saturating_duration_since(Instant::now());
        match tokio::time::timeout(wait, sock.recv_from(&mut buf)).await {
            Ok(Ok((n, from))) => {
                if &buf[..n] == DISCOVERY_MESSAGE {
                    continue; // our own broadcast looped back
                }
                if let Some(p) = parse_reply(&buf[..n], from.ip()) {
                    if !found.iter().any(|f| f.ip == p.ip) {
                        found.push(p);
                    }
                    if first_only {
                        break;
                    }
                }
            }
            Ok(Err(e)) => {
                // Windows reports ICMP port unreachable as a recv error; ignore
                log::debug!("discovery recv error: {e}");
            }
            Err(_) => {}
        }
    }
    found.sort_by(|a, b| a.ip.cmp(&b.ip));
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sdcp_reply() {
        let r = br#"{"Id":"979d4C788A4a78bC777A870F1A02867A","Data":{"Name":"Saturn","MachineName":"ELEGOO Saturn 4 Ultra","BrandName":"ELEGOO","MainboardIP":"192.168.1.42","MainboardID":"000000000001d354","ProtocolVersion":"V3.0.0","FirmwareVersion":"V1.2.3"}}"#;
        let p = parse_reply(r, "10.0.0.1".parse().unwrap()).unwrap();
        assert_eq!(p.protocol, Protocol::Sdcp);
        assert_eq!(p.ip.to_string(), "192.168.1.42");
        assert_eq!(p.mainboard_id.as_deref(), Some("000000000001d354"));
        assert_eq!(p.model.as_deref(), Some("ELEGOO Saturn 4 Ultra"));
    }

    #[test]
    fn parse_legacy_reply() {
        let r = b"ok MAC:00:e0:4c:68:01:02 IP:192.168.1.50 VER:V1.4.3 ID:2a,3b,4c NAME:CBD Printer";
        let p = parse_reply(r, "192.168.1.50".parse().unwrap()).unwrap();
        assert_eq!(p.protocol, Protocol::ChituUdp);
        assert_eq!(p.name, "CBD Printer");
        assert_eq!(p.firmware_version.as_deref(), Some("V1.4.3"));
    }
}
