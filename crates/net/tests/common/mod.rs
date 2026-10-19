//! In-process fake printers and helpers shared by the integration tests.
//!
//! Everything binds to 127.0.0.1 on ephemeral ports; nothing here talks to the
//! real network.
#![allow(dead_code)]

pub mod chitu_mock;
pub mod h264;
pub mod http;
pub mod rtsp_mock;
pub mod sdcp_mock;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use md5::{Digest, Md5};

/// Fails the test instead of hanging when `fut` takes longer than `secs`.
pub async fn within<T>(secs: f32, what: &str, fut: impl Future<Output = T>) -> T {
    match tokio::time::timeout(Duration::from_secs_f32(secs), fut).await {
        Ok(v) => v,
        Err(_) => panic!("timed out after {secs}s: {what}"),
    }
}

/// Deterministic pseudo random bytes (xorshift64*).
pub fn random_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut out = Vec::with_capacity(len + 8);
    while out.len() < len {
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        out.extend_from_slice(&x.wrapping_mul(0x2545_F491_4F6C_DD1D).to_le_bytes());
    }
    out.truncate(len);
    out
}

pub fn md5_hex(data: &[u8]) -> String {
    format!("{:x}", Md5::digest(data))
}

/// A unique temporary directory, removed on drop.
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "openresin-net-test-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn file(&self, name: &str, data: &[u8]) -> PathBuf {
        let p = self.0.join(name);
        std::fs::write(&p, data).unwrap();
        p
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Records upload/download progress callbacks.
#[derive(Clone, Default)]
pub struct ProgressLog(pub Arc<Mutex<Vec<(u64, u64)>>>);

impl ProgressLog {
    pub fn callback(&self) -> impl Fn(u64, u64) + Send + Sync + 'static {
        let log = self.0.clone();
        move |a, b| log.lock().unwrap().push((a, b))
    }

    pub fn calls(&self) -> Vec<(u64, u64)> {
        self.0.lock().unwrap().clone()
    }

    /// Asserts that progress never goes backwards, the total is constant and
    /// the last call reports completion.
    pub fn assert_complete(&self, total: u64) {
        let calls = self.calls();
        assert!(!calls.is_empty(), "no progress reported");
        for w in calls.windows(2) {
            assert!(w[1].0 >= w[0].0, "progress went backwards: {calls:?}");
        }
        assert!(calls.iter().all(|c| c.1 == total), "total changed: {calls:?}");
        assert!(calls.iter().all(|c| c.0 <= c.1), "sent > total: {calls:?}");
        assert_eq!(calls.last().unwrap().0, total, "did not end at 100%: {calls:?}");
    }
}

/// A local UDP port that nothing listens on (bound once, then released).
pub fn free_udp_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A local TCP port that nothing listens on (bound once, then released).
pub fn free_tcp_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
