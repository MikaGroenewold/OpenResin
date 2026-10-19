//! Network subcommands: discovery, status, file transfer, print control, camera.

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use openresin_net::camera::CameraRelay;
use openresin_net::PrinterClient;

#[derive(Subcommand)]
pub enum NetCmd {
    /// Discover printers on the local network (UDP broadcast).
    Discover {
        /// Seconds to wait for replies.
        #[arg(long, default_value_t = 3)]
        timeout: u64,
        /// Print raw JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show a printer's status and attributes (read-only).
    Status {
        ip: IpAddr,
        /// Keep printing status updates.
        #[arg(long)]
        watch: bool,
    },
    /// List files stored on the printer.
    Files {
        ip: IpAddr,
        #[arg(default_value = "/local")]
        dir: String,
    },
    /// Download a file stored on the printer.
    Download {
        ip: IpAddr,
        /// Path as listed by `files`, e.g. /local/model.goo
        path: String,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Upload a sliced file to the printer.
    Upload {
        ip: IpAddr,
        file: PathBuf,
        /// Name on the printer (defaults to the local file name).
        #[arg(long)]
        name: Option<String>,
        /// Start printing after the upload.
        #[arg(long)]
        print: bool,
    },
    /// Start printing a file that is already on the printer.
    Print { ip: IpAddr, name: String },
    /// Pause the current print.
    Pause { ip: IpAddr },
    /// Resume a paused print.
    Resume { ip: IpAddr },
    /// Stop the current print.
    Stop { ip: IpAddr },
    /// Camera access: save a snapshot and/or relay the stream locally.
    Camera {
        ip: IpAddr,
        /// Save one JPEG frame to this path.
        #[arg(long)]
        snapshot: Option<PathBuf>,
        /// Keep relaying and print the local MJPEG URL until Ctrl+C.
        #[arg(long)]
        serve: bool,
        /// Use this stream URL instead of asking the printer (e.g. rtsp://ip:554/video).
        #[arg(long)]
        url: Option<String>,
    },
}

async fn connect(ip: IpAddr) -> Result<PrinterClient> {
    PrinterClient::connect(ip)
        .await
        .with_context(|| format!("connecting to {ip}"))
}

pub async fn run(cmd: NetCmd) -> Result<()> {
    match cmd {
        NetCmd::Discover { timeout, json } => {
            let found = openresin_net::discover(Duration::from_secs(timeout)).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&found)?);
            } else if found.is_empty() {
                println!("no printers answered");
            } else {
                for p in found {
                    println!("{p}");
                }
            }
        }
        NetCmd::Status { ip, watch } => {
            let c = connect(ip).await?;
            tokio::time::sleep(Duration::from_millis(500)).await;
            let s = c.state();
            println!("protocol: {:?}", c.protocol());
            println!("{}", serde_json::to_string_pretty(&s.status)?);
            if let Some(a) = &s.attributes {
                println!("attributes: {}", serde_json::to_string_pretty(a)?);
            }
            if watch {
                let mut rx = c.watch();
                loop {
                    tokio::select! {
                        r = rx.changed() => { if r.is_err() { break } }
                        _ = tokio::signal::ctrl_c() => break,
                    }
                    let st = rx.borrow().status.clone();
                    println!(
                        "{:?} {} {}/{} {}",
                        st.state,
                        st.detail.unwrap_or_default(),
                        st.current_layer.unwrap_or(0),
                        st.total_layers.unwrap_or(0),
                        st.filename.unwrap_or_default()
                    );
                }
            }
        }
        NetCmd::Files { ip, dir } => {
            let c = connect(ip).await?;
            for f in c.list_files(&dir).await? {
                println!("{}{}", f.path, if f.is_dir { "/" } else { "" });
            }
        }
        NetCmd::Upload { ip, file, name, print } => {
            // fail before the (slow) connection attempt
            if !file.is_file() {
                bail!("{} does not exist or is not a file", file.display());
            }
            let c = connect(ip).await?;
            let name = c.remote_name(
                &name.unwrap_or_else(|| file.file_name().and_then(|n| n.to_str()).unwrap_or("job").to_string()),
            );
            if c.state().status.state.is_busy() && print {
                bail!("printer is busy; refusing to start a print");
            }
            c.upload(
                &file,
                &name,
                &|done, total| eprint!("\ruploading {:>5.1}%", done as f64 * 100.0 / total.max(1) as f64),
                &AtomicBool::new(false),
            )
            .await?;
            eprintln!("\ruploaded {name}            ");
            if print {
                c.start_print(&name).await?;
                println!("print started");
            }
        }
        NetCmd::Download { ip, path, output } => {
            let c = connect(ip).await?;
            let n = c
                .download(
                    &path,
                    &output,
                    &|done, total| eprint!("\rdownloading {:>5.1}%", done as f64 * 100.0 / total.max(1) as f64),
                    &AtomicBool::new(false),
                )
                .await?;
            eprintln!();
            println!("saved {n} bytes to {}", output.display());
        }
        NetCmd::Print { ip, name } => connect(ip).await?.start_print(&name).await?,
        NetCmd::Pause { ip } => connect(ip).await?.pause().await?,
        NetCmd::Resume { ip } => connect(ip).await?.resume().await?,
        NetCmd::Stop { ip } => connect(ip).await?.stop().await?,
        NetCmd::Camera {
            ip,
            snapshot,
            serve,
            url,
        } => {
            let (url, client) = match url {
                Some(u) => (u, None),
                None => {
                    let c = connect(ip).await?;
                    let u = c.enable_video(true).await?.context("printer returned no camera URL")?;
                    (u, Some(c))
                }
            };
            eprintln!("camera source: {url}");
            let relay = CameraRelay::start(&url).await?;
            if let Some(path) = snapshot {
                let f = relay
                    .wait_frame(Duration::from_secs(15))
                    .await
                    .with_context(|| format!("no frame received ({})", relay.status()))?;
                std::fs::write(&path, &f.jpeg)?;
                println!("saved {}x{} snapshot to {}", f.width, f.height, path.display());
            }
            if serve {
                println!("MJPEG stream: {}", relay.stream_url);
                println!("snapshot:     {}", relay.snapshot_url);
                tokio::signal::ctrl_c().await?;
            }
            relay.shutdown().await;
            if let Some(c) = client {
                let _ = c.enable_video(false).await;
            }
        }
    }
    Ok(())
}
