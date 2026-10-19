//! Shows the camera of an Elegoo (SDCP) printer in a window.
//!
//! Finds the printer by UDP broadcast (or takes its IP), asks it to enable the
//! camera and displays the stream. The stream is also re-served as MJPEG on
//! 127.0.0.1, so it can be opened in a browser or VLC at the same time.
//!
//! Keys: `S` saves a JPEG snapshot to the current directory, `Esc` quits.

use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use minifb::{Key, KeyRepeat, ScaleMode, Window, WindowOptions};
use openresin_net::camera::CameraRelay;
use openresin_net::sdcp::SdcpClient;
use openresin_net::{NetError, Protocol};

#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// Printer IP address. Without it the first SDCP printer that answers discovery is used.
    ip: Option<IpAddr>,
    /// Use this stream URL directly instead of asking the printer (e.g. http://ip:3031/video).
    #[arg(long)]
    url: Option<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let rt = tokio::runtime::Runtime::new()?;
    let (client, relay, label) = rt.block_on(start(args.ip, args.url))?;
    eprintln!("also available as MJPEG: {}", relay.stream_url);

    // Ctrl+C in the console should still close the camera session cleanly
    let interrupted = Arc::new(AtomicBool::new(false));
    let flag = interrupted.clone();
    rt.spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            flag.store(true, Ordering::Relaxed);
        }
    });

    let r = show(&relay, &label, &interrupted);
    rt.block_on(async {
        relay.shutdown().await;
        if let Some(c) = client {
            let _ = c.enable_video(false).await;
        }
    });
    r
}

async fn start(ip: Option<IpAddr>, url: Option<String>) -> Result<(Option<SdcpClient>, CameraRelay, String)> {
    if let Some(url) = url {
        let relay = CameraRelay::start(&url).await?;
        return Ok((None, relay, url));
    }
    let printer = match ip {
        Some(ip) => openresin_net::discover_host(ip, Duration::from_secs(3))
            .await?
            .with_context(|| format!("no printer answered at {ip}"))?,
        None => {
            eprintln!("searching for printers...");
            let found = openresin_net::discover(Duration::from_secs(3)).await?;
            found
                .into_iter()
                .find(|p| p.protocol == Protocol::Sdcp)
                .context("no SDCP printer answered; pass the printer's IP address")?
        }
    };
    eprintln!("printer: {printer}");
    let client = SdcpClient::connect(&printer)
        .await
        .with_context(|| format!("connecting to {}", printer.ip))?;
    let (url, client) = match client.enable_video(true).await {
        Ok(url) => (url.context("printer returned no camera URL")?, Some(client)),
        // e.g. ChituManager holds the only stream slot; the RTSP server still serves extra viewers
        Err(NetError::Rejected(why)) => {
            let url = format!("rtsp://{}:554/video", printer.ip);
            eprintln!("printer refused to enable the camera ({why}), trying {url} directly");
            (url, None)
        }
        Err(e) => return Err(e.into()),
    };
    eprintln!("camera source: {url}");
    let relay = CameraRelay::start(&url).await?;
    // `client` is only kept when we enabled the camera, so we only disable what we turned on
    Ok((client, relay, format!("{} ({})", printer.name, printer.ip)))
}

fn show(relay: &CameraRelay, label: &str, interrupted: &AtomicBool) -> Result<()> {
    let (mut w, mut h) = (960, 540);
    let mut buf = vec![0u32; w * h];
    let mut window = Window::new(
        &format!("{label} - connecting"),
        w,
        h,
        WindowOptions {
            resize: true,
            scale_mode: ScaleMode::AspectRatioStretch,
            ..WindowOptions::default()
        },
    )
    .map_err(|e| anyhow!("opening window: {e}"))?;
    window.set_target_fps(60);

    let mut last_seq = 0;
    let mut last_status = String::new();
    while window.is_open() && !window.is_key_down(Key::Escape) && !interrupted.load(Ordering::Relaxed) {
        let frame = relay.latest();
        if let Some(f) = frame.as_ref().filter(|f| f.seq != last_seq) {
            last_seq = f.seq;
            match image::load_from_memory_with_format(&f.jpeg, image::ImageFormat::Jpeg) {
                Ok(img) => {
                    let rgb = img.into_rgb8();
                    (w, h) = (rgb.width() as usize, rgb.height() as usize);
                    buf.clear();
                    buf.extend(rgb.pixels().map(|p| u32::from_be_bytes([0, p[0], p[1], p[2]])));
                }
                Err(e) => eprintln!("bad frame: {e}"),
            }
        }

        let status = relay.status();
        if status != last_status {
            eprintln!("{status}");
            window.set_title(&format!("{label} - {status}"));
            last_status = status;
        }

        if window.is_key_pressed(Key::S, KeyRepeat::No) {
            if let Some(f) = &frame {
                let secs = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let path = format!("printer-cam-{secs}.jpg");
                match std::fs::write(&path, &f.jpeg) {
                    Ok(()) => eprintln!("saved {path}"),
                    Err(e) => eprintln!("saving {path}: {e}"),
                }
            }
        }

        window
            .update_with_buffer(&buf, w, h)
            .map_err(|e| anyhow!("drawing frame: {e}"))?;
    }
    Ok(())
}
