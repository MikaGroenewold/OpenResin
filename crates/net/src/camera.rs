//! Printer camera access.
//!
//! Sources (RTSP/H.264 or HTTP/MJPEG) are converted into a stream of JPEG frames
//! and re-served on `127.0.0.1` as `multipart/x-mixed-replace`, which every
//! webview (WebView2, WebKitGTK, WKWebView) can show in a plain `<img>` tag.
//! The local URL contains a random token so other local software cannot guess it.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::StreamExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::{NetError, Result};

const MAX_FPS: f32 = 15.0;
const JPEG_QUALITY: u8 = 80;

#[derive(Debug, Clone, Default)]
pub struct Frame {
    pub jpeg: Bytes,
    pub width: u32,
    pub height: u32,
    pub seq: u64,
}

/// A running camera relay. Dropping it stops the source and the local server.
pub struct CameraRelay {
    /// `http://127.0.0.1:<port>/<token>/stream.mjpeg`
    pub stream_url: String,
    /// `http://127.0.0.1:<port>/<token>/snapshot.jpg`
    pub snapshot_url: String,
    frames: watch::Receiver<Option<Frame>>,
    status: watch::Receiver<String>,
    stop: watch::Sender<bool>,
    source_task: Option<JoinHandle<()>>,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for CameraRelay {
    fn drop(&mut self) {
        // the source task ends the RTSP session (TEARDOWN) on its own
        let _ = self.stop.send(true);
        for t in &self.tasks {
            t.abort();
        }
    }
}

impl CameraRelay {
    /// Starts relaying `source` (an `rtsp://` or `http://` MJPEG URL).
    pub async fn start(source: &str) -> Result<Self> {
        let (tx, frames) = watch::channel::<Option<Frame>>(None);
        let (status_tx, status) = watch::channel(String::from("connecting"));
        let (stop_tx, mut stop_rx) = watch::channel(false);
        let src = source.to_string();
        let source_task = tokio::spawn(async move {
            let mut backoff = Duration::from_secs(1);
            loop {
                let started = Instant::now();
                let r = if src.starts_with("rtsp://") {
                    run_rtsp(&src, &tx, &status_tx, &mut stop_rx).await
                } else {
                    tokio::select! {
                        r = run_mjpeg(&src, &tx, &status_tx) => r,
                        _ = stop_rx.wait_for(|s| *s) => Ok(()),
                    }
                };
                if *stop_rx.borrow() {
                    return;
                }
                let msg = match r {
                    Ok(()) => "stream ended".to_string(),
                    Err(e) => format!("camera error: {e}"),
                };
                log::warn!("{msg}");
                let _ = status_tx.send(msg);
                if started.elapsed() > Duration::from_secs(30) {
                    backoff = Duration::from_secs(1);
                }
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = stop_rx.wait_for(|s| *s) => return,
                }
                backoff = (backoff * 2).min(Duration::from_secs(15));
            }
        });

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let addr: SocketAddr = listener.local_addr()?;
        let token = uuid::Uuid::new_v4().simple().to_string();
        let server_frames = frames.clone();
        let tok = token.clone();
        let server_task = tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                tokio::spawn(serve_client(sock, tok.clone(), server_frames.clone()));
            }
        });
        Ok(Self {
            stream_url: format!("http://{addr}/{token}/stream.mjpeg"),
            snapshot_url: format!("http://{addr}/{token}/snapshot.jpg"),
            frames,
            status,
            stop: stop_tx,
            source_task: Some(source_task),
            tasks: vec![server_task],
        })
    }

    /// Latest JPEG frame, if any arrived yet.
    pub fn latest(&self) -> Option<Frame> {
        self.frames.borrow().clone()
    }

    /// Waits up to `timeout` for the first frame.
    pub async fn wait_frame(&self, timeout: Duration) -> Option<Frame> {
        let mut rx = self.frames.clone();
        let _ = tokio::time::timeout(timeout, rx.wait_for(|f| f.is_some())).await;
        self.latest()
    }

    pub fn status(&self) -> String {
        self.status.borrow().clone()
    }

    /// Stops the relay and waits until the camera session is closed.
    pub async fn shutdown(mut self) {
        let _ = self.stop.send(true);
        if let Some(t) = self.source_task.take() {
            let _ = tokio::time::timeout(Duration::from_secs(3), t).await;
        }
    }
}

/// Largest request head (request line + headers) the relay accepts.
const MAX_REQUEST_HEAD: usize = 8 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

enum Head {
    Complete(Vec<u8>),
    TooLarge,
    /// Timed out, closed or failed before the head was complete.
    Incomplete,
}

/// Reads up to the blank line that ends the request head; clients may send it
/// in several TCP segments.
async fn read_request_head(sock: &mut tokio::net::TcpStream) -> Head {
    let mut buf = Vec::with_capacity(1024);
    let read = async {
        let mut tmp = [0u8; 1024];
        loop {
            if let Some(end) = find(&buf, b"\r\n\r\n") {
                buf.truncate(end);
                return Head::Complete(std::mem::take(&mut buf));
            }
            if buf.len() > MAX_REQUEST_HEAD {
                return Head::TooLarge;
            }
            match sock.read(&mut tmp).await {
                Ok(0) | Err(_) => return Head::Incomplete,
                Ok(n) => buf.extend_from_slice(&tmp[..n]),
            }
        }
    };
    tokio::time::timeout(REQUEST_TIMEOUT, read)
        .await
        .unwrap_or(Head::Incomplete)
}

async fn serve_client(mut sock: tokio::net::TcpStream, token: String, mut frames: watch::Receiver<Option<Frame>>) {
    let head = match read_request_head(&mut sock).await {
        Head::Complete(h) => h,
        Head::TooLarge => {
            let _ = sock
                .write_all(
                    b"HTTP/1.1 431 Request Header Fields Too Large\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            let _ = sock.shutdown().await;
            // swallow the rest of the request: closing with unread data resets
            // the connection, which can discard the answer at the client
            let mut rest = [0u8; 4096];
            let _ = tokio::time::timeout(Duration::from_secs(1), async {
                while let Ok(1..) = sock.read(&mut rest).await {}
            })
            .await;
            return;
        }
        Head::Incomplete => return,
    };
    let req = String::from_utf8_lossy(&head);
    let path = req.split_whitespace().nth(1).unwrap_or("").to_string();
    let cors = "Access-Control-Allow-Origin: *\r\nCache-Control: no-cache, no-store\r\n";
    if path == format!("/{token}/snapshot.jpg") {
        let f = frames.borrow().clone();
        match f {
            Some(f) => {
                let hdr = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n{cors}Connection: close\r\n\r\n",
                    f.jpeg.len()
                );
                let _ = sock.write_all(hdr.as_bytes()).await;
                let _ = sock.write_all(&f.jpeg).await;
            }
            None => {
                let _ = sock
                    .write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await;
            }
        }
        return;
    }
    if path != format!("/{token}/stream.mjpeg") {
        let _ = sock
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await;
        return;
    }
    let hdr = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary=frame\r\n{cors}Connection: close\r\n\r\n"
    );
    if sock.write_all(hdr.as_bytes()).await.is_err() {
        return;
    }
    let mut last_seq = u64::MAX;
    loop {
        let f = frames.borrow_and_update().clone();
        if let Some(f) = f {
            if f.seq != last_seq {
                last_seq = f.seq;
                let part = format!(
                    "--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
                    f.jpeg.len()
                );
                if sock.write_all(part.as_bytes()).await.is_err()
                    || sock.write_all(&f.jpeg).await.is_err()
                    || sock.write_all(b"\r\n").await.is_err()
                {
                    return;
                }
            }
        }
        if frames.changed().await.is_err() {
            return;
        }
    }
}

// ------------------------------------------------------------------ MJPEG

async fn run_mjpeg(url: &str, tx: &watch::Sender<Option<Frame>>, status: &watch::Sender<String>) -> Result<()> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(8))
        .build()?;
    let resp = client.get(url).send().await?.error_for_status()?;
    let _ = status.send("streaming".into());
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::with_capacity(512 * 1024);
    let mut seq = last_seq(tx);
    let mut last = Instant::now() - Duration::from_secs(1);
    while let Some(chunk) = stream.next().await {
        buf.extend_from_slice(&chunk?);
        // extract complete JPEGs (SOI .. EOI)
        loop {
            let Some(start) = find(&buf, &[0xFF, 0xD8]) else {
                buf.clear();
                break;
            };
            let Some(end) = find(&buf[start + 2..], &[0xFF, 0xD9]).map(|e| start + 2 + e + 2) else {
                if start > 0 {
                    buf.drain(..start);
                }
                break;
            };
            let jpeg = Bytes::copy_from_slice(&buf[start..end]);
            buf.drain(..end);
            if last.elapsed().as_secs_f32() >= 1.0 / MAX_FPS {
                last = Instant::now();
                seq += 1;
                let (w, h) = jpeg_size(&jpeg).unwrap_or((0, 0));
                let _ = tx.send(Some(Frame {
                    jpeg,
                    width: w,
                    height: h,
                    seq,
                }));
            }
        }
        if buf.len() > 8 * 1024 * 1024 {
            buf.clear();
        }
    }
    Ok(())
}

/// Frame numbers keep counting across reconnects so viewers never mistake a
/// new frame for one they already have.
fn last_seq(tx: &watch::Sender<Option<Frame>>) -> u64 {
    tx.borrow().as_ref().map_or(0, |f| f.seq)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn jpeg_size(data: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2;
    while i + 9 < data.len() {
        if data[i] != 0xFF {
            return None;
        }
        let marker = data[i + 1];
        let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        if (0xC0..=0xC3).contains(&marker) {
            let h = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
            let w = u16::from_be_bytes([data[i + 7], data[i + 8]]) as u32;
            return Some((w, h));
        }
        i += 2 + len;
    }
    None
}

// ------------------------------------------------------------------ RTSP

async fn run_rtsp(
    url: &str,
    tx: &watch::Sender<Option<Frame>>,
    status: &watch::Sender<String>,
    stop: &mut watch::Receiver<bool>,
) -> Result<()> {
    let mut session = crate::rtsp::RtspSession::open(url).await?;
    let _ = status.send("streaming".into());
    let decoder = Arc::new(std::sync::Mutex::new(
        openh264::decoder::Decoder::new().map_err(|e| NetError::Protocol(format!("H.264 decoder: {e}")))?,
    ));
    let mut seq = last_seq(tx);
    let mut last = Instant::now() - Duration::from_secs(1);
    let result = loop {
        let au = tokio::select! {
            r = session.next_access_unit() => match r {
                Ok(au) => au,
                Err(e) => break Err(e),
            },
            _ = stop.wait_for(|s| *s) => break Ok(()),
        };
        let encode_now = last.elapsed().as_secs_f32() >= 1.0 / MAX_FPS;
        let dec = decoder.clone();
        // every access unit must be decoded; JPEG encoding happens at the target rate
        let out = tokio::task::spawn_blocking(move || -> Option<Frame> {
            use openh264::formats::YUVSource;
            let mut d = dec.lock().ok()?;
            let yuv = d.decode(&au).ok()??;
            if !encode_now {
                return None;
            }
            let (w, h) = yuv.dimensions();
            let mut rgb = vec![0u8; w * h * 3];
            yuv.write_rgb8(&mut rgb);
            let mut jpeg = Vec::with_capacity(w * h / 4);
            let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, JPEG_QUALITY);
            use image::ImageEncoder;
            enc.write_image(&rgb, w as u32, h as u32, image::ExtendedColorType::Rgb8)
                .ok()?;
            Some(Frame {
                jpeg: Bytes::from(jpeg),
                width: w as u32,
                height: h as u32,
                seq: 0,
            })
        })
        .await
        .ok()
        .flatten();
        if let Some(mut f) = out {
            last = Instant::now();
            seq += 1;
            f.seq = seq;
            let _ = tx.send(Some(f));
        }
    };
    session.teardown().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mjpeg_relay_serves_frames() {
        // a fake MJPEG camera
        let cam = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let cam_addr = cam.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = cam.accept().await.unwrap();
            let mut b = [0u8; 1024];
            let _ = s.read(&mut b).await;
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary=x\r\n\r\n")
                .await
                .unwrap();
            let img = image::RgbImage::from_pixel(8, 6, image::Rgb([200, 10, 10]));
            let mut jpeg = Vec::new();
            image::codecs::jpeg::JpegEncoder::new(&mut jpeg)
                .encode_image(&img)
                .unwrap();
            for _ in 0..50 {
                let part = "--x\r\nContent-Type: image/jpeg\r\n\r\n";
                if s.write_all(part.as_bytes()).await.is_err() || s.write_all(&jpeg).await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(80)).await;
            }
        });
        let relay = CameraRelay::start(&format!("http://{cam_addr}/video")).await.unwrap();
        let f = relay.wait_frame(Duration::from_secs(5)).await.expect("frame");
        assert_eq!((f.width, f.height), (8, 6));
        let snap = reqwest::get(&relay.snapshot_url).await.unwrap();
        assert_eq!(snap.status(), 200);
        assert_eq!(snap.bytes().await.unwrap()[..2], [0xFF, 0xD8]);
        let bad = reqwest::get(relay.snapshot_url.replace("/snapshot.jpg", "x/snapshot.jpg"))
            .await
            .unwrap();
        assert_eq!(bad.status(), 404);
    }
}
