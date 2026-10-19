//! Printer networking for OpenResin.
//!
//! Everything in this crate talks to devices on the local network only. There
//! is no cloud relay, no account, no telemetry and no update check.

pub mod camera;
pub mod chitu;
pub mod discovery;
pub mod printer;
pub mod rtsp;
pub mod sdcp;
pub mod status;

pub use discovery::{discover, discover_host, DiscoveredPrinter, Protocol};
pub use printer::{DeviceState, PrinterClient};
pub use status::{FileEntry, MachineState, PrinterStatus};

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// Boxed: the WebSocket error is large and would bloat every `Result`.
    #[error("websocket error: {0}")]
    WebSocket(Box<tokio_tungstenite::tungstenite::Error>),
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("timed out waiting for the printer")]
    Timeout,
    #[error("printer rejected the request: {0}")]
    Rejected(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("not supported by this printer: {0}")]
    Unsupported(String),
    #[error("connection closed")]
    Closed,
    #[error("cancelled")]
    Cancelled,
}

impl From<tokio_tungstenite::tungstenite::Error> for NetError {
    fn from(e: tokio_tungstenite::tungstenite::Error) -> Self {
        NetError::WebSocket(Box::new(e))
    }
}

pub type Result<T, E = NetError> = std::result::Result<T, E>;

/// Local address for sockets that talk to `peer`: loopback peers (local
/// proxies, tests) stay on loopback, everything else uses the wildcard address.
pub(crate) fn bind_addr_for(peer: std::net::IpAddr) -> std::net::IpAddr {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    match peer {
        IpAddr::V4(p) if p.is_loopback() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(p) if p.is_loopback() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
    }
}

/// Progress callback for uploads: `(bytes_sent, total_bytes)`.
pub type ProgressFn = dyn Fn(u64, u64) + Send + Sync;
