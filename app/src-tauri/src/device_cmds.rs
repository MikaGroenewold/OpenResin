//! Network printer commands: discovery, status, files, upload, control, camera.

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use openresin_net::camera::CameraRelay;
use openresin_net::{DiscoveredPrinter, FileEntry, PrinterClient, PrinterStatus, Protocol};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime, State};

use crate::state::{AppState, SavedDevice};

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn parse_ip(ip: &str) -> CmdResult<IpAddr> {
    ip.trim()
        .parse()
        .map_err(|_| format!("'{ip}' is not a valid IP address"))
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceStatusEvent {
    pub ip: String,
    pub status: PrinterStatus,
    pub attributes: Option<serde_json::Value>,
    pub connected: bool,
    pub protocol: Option<Protocol>,
    pub camera: bool,
    /// Stored files can be downloaded and previewed.
    pub downloads: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct UploadProgress {
    pub ip: String,
    pub sent: u64,
    pub total: u64,
    pub done: bool,
    pub error: Option<String>,
}

#[tauri::command]
pub async fn discover_printers(timeout_ms: Option<u64>) -> CmdResult<Vec<DiscoveredPrinter>> {
    openresin_net::discover(Duration::from_millis(timeout_ms.unwrap_or(2500).clamp(500, 15000)))
        .await
        .map_err(err)
}

#[tauri::command]
pub fn saved_devices(state: State<'_, AppState>) -> Vec<SavedDevice> {
    state.config.lock().unwrap().devices.clone()
}

#[tauri::command]
pub fn save_device(state: State<'_, AppState>, mut device: SavedDevice) -> CmdResult<Vec<SavedDevice>> {
    parse_ip(&device.ip)?;
    // store the address the way it is connected to (and compared) later
    device.ip = device.ip.trim().to_string();
    let mut cfg = state.config.lock().unwrap();
    cfg.devices.retain(|d| d.ip != device.ip);
    cfg.devices.push(device);
    cfg.save();
    Ok(cfg.devices.clone())
}

#[tauri::command]
pub async fn forget_device(state: State<'_, AppState>, ip: String) -> CmdResult<Vec<SavedDevice>> {
    disconnect_printer(state.clone(), ip.clone()).await?;
    let mut cfg = state.config.lock().unwrap();
    cfg.devices.retain(|d| d.ip != ip);
    cfg.save();
    Ok(cfg.devices.clone())
}

async fn client(state: &AppState, ip: &str) -> CmdResult<Arc<PrinterClient>> {
    state
        .devices
        .lock()
        .await
        .get(ip)
        .cloned()
        .ok_or_else(|| format!("not connected to {ip}"))
}

fn event(ip: &str, c: &PrinterClient, connected: bool) -> DeviceStatusEvent {
    let s = c.state();
    DeviceStatusEvent {
        ip: ip.to_string(),
        status: s.status,
        attributes: s.attributes,
        connected,
        protocol: Some(c.protocol()),
        camera: c.supports_camera(),
        downloads: c.can_download("/local/x"),
    }
}

/// Connects (or reuses the connection) and starts forwarding status events.
#[tauri::command]
pub async fn connect_printer<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    ip: String,
) -> CmdResult<DeviceStatusEvent> {
    if let Ok(c) = client(&state, &ip).await {
        return Ok(event(&ip, &c, true));
    }
    let addr = parse_ip(&ip)?;
    let c = Arc::new(PrinterClient::connect(addr).await.map_err(|e| format!("{ip}: {e}"))?);
    let mut rx = c.watch();
    let app2 = app.clone();
    let ip2 = ip.clone();
    let protocol = c.protocol();
    let camera = c.supports_camera();
    let downloads = c.can_download("/local/x");
    tauri::async_runtime::spawn(async move {
        loop {
            let changed = rx.changed().await;
            let s = rx.borrow().clone();
            let connected = changed.is_ok() && s.status.state != openresin_net::MachineState::Offline;
            let _ = app2.emit(
                "device-status",
                DeviceStatusEvent {
                    ip: ip2.clone(),
                    status: s.status,
                    attributes: s.attributes,
                    connected,
                    protocol: Some(protocol),
                    camera,
                    downloads,
                },
            );
            if !connected {
                break;
            }
        }
    });
    // keep the status fresh even on firmwares that only push on change
    let poll = Arc::downgrade(&c);
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let Some(c) = poll.upgrade() else { break };
            if c.refresh().await.is_err() {
                break;
            }
        }
    });
    let ev = event(&ip, &c, true);
    state.devices.lock().await.insert(ip.clone(), c);
    Ok(ev)
}

#[tauri::command]
pub async fn disconnect_printer(state: State<'_, AppState>, ip: String) -> CmdResult<()> {
    if let Some(cam) = state.cameras.lock().await.remove(&ip) {
        cam.shutdown().await;
    }
    state.devices.lock().await.remove(&ip);
    Ok(())
}

#[tauri::command]
pub async fn printer_files(state: State<'_, AppState>, ip: String, dir: Option<String>) -> CmdResult<Vec<FileEntry>> {
    let c = client(&state, &ip).await?;
    c.list_files(dir.as_deref().unwrap_or("/local")).await.map_err(err)
}

#[tauri::command]
pub async fn printer_delete(state: State<'_, AppState>, ip: String, files: Vec<String>) -> CmdResult<Vec<String>> {
    let c = client(&state, &ip).await?;
    c.delete(&files).await.map_err(err)
}

#[tauri::command]
pub async fn printer_history(state: State<'_, AppState>, ip: String) -> CmdResult<serde_json::Value> {
    let c = client(&state, &ip).await?;
    c.history(20).await.map_err(err)
}

#[tauri::command]
pub async fn printer_command(
    state: State<'_, AppState>,
    ip: String,
    command: String,
    file: Option<String>,
) -> CmdResult<()> {
    let c = client(&state, &ip).await?;
    match command.as_str() {
        "pause" => c.pause().await,
        "resume" => c.resume().await,
        "stop" => c.stop().await,
        "start" => {
            let f = file.ok_or("no file given")?;
            if c.state().status.state.is_busy() {
                return Err("the printer is busy".into());
            }
            c.start_print(&f).await
        }
        "refresh" => c.refresh().await,
        other => return Err(format!("unknown command {other}")),
    }
    .map_err(err)
}

/// Uploads the last sliced file (or `path`) and optionally starts printing.
#[tauri::command]
pub async fn upload_to_printer<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    ip: String,
    path: Option<String>,
    name: Option<String>,
    start: bool,
) -> CmdResult<String> {
    let c = client(&state, &ip).await?;
    let (local, default_name) = match path {
        Some(p) => {
            let pb = PathBuf::from(&p);
            let n = pb.file_name().and_then(|n| n.to_str()).unwrap_or("job").to_string();
            (pb, n)
        }
        None => {
            let slice = state.slice.lock().unwrap();
            let s = slice.as_ref().ok_or("nothing sliced yet")?;
            (s.path.clone(), s.job_name.clone())
        }
    };
    let remote = c.remote_name(&name.unwrap_or(default_name));
    if start && c.state().status.state.is_busy() {
        return Err("the printer is busy; not starting a new print".into());
    }
    let cancel = Arc::new(AtomicBool::new(false));
    state.uploads.lock().unwrap().insert(ip.clone(), cancel.clone());
    let app2 = app.clone();
    let ip2 = ip.clone();
    let progress = move |sent: u64, total: u64| {
        let _ = app2.emit(
            "upload-progress",
            UploadProgress {
                ip: ip2.clone(),
                sent,
                total,
                done: false,
                error: None,
            },
        );
    };
    let result = c.upload(&local, &remote, &progress, &cancel).await;
    state.uploads.lock().unwrap().remove(&ip);
    let total = std::fs::metadata(&local).map(|m| m.len()).unwrap_or(0);
    let _ = app.emit(
        "upload-progress",
        UploadProgress {
            ip: ip.clone(),
            sent: if result.is_ok() { total } else { 0 },
            total,
            done: true,
            error: result.as_ref().err().map(|e| e.to_string()),
        },
    );
    result.map_err(err)?;
    if start {
        // SDCP V3 expects the bare file name for files on local storage
        c.start_print(&remote).await.map_err(err)?;
    }
    Ok(remote)
}

#[tauri::command]
pub async fn printer_set_time_lapse(state: State<'_, AppState>, ip: String, enable: bool) -> CmdResult<()> {
    client(&state, &ip).await?.set_time_lapse(enable).await.map_err(err)
}

#[tauri::command]
pub async fn printer_rename(state: State<'_, AppState>, ip: String, name: String) -> CmdResult<()> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("the name must not be empty".into());
    }
    client(&state, &ip).await?.rename(&name).await.map_err(err)?;
    let mut cfg = state.config.lock().unwrap();
    for d in cfg.devices.iter_mut().filter(|d| d.ip == ip) {
        d.name = name.clone();
    }
    cfg.save();
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct DownloadProgress {
    pub ip: String,
    pub path: String,
    pub done: u64,
    pub total: u64,
}

/// Downloads a file stored on the printer.
#[tauri::command]
pub async fn printer_download<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    ip: String,
    path: String,
    dest: String,
) -> CmdResult<u64> {
    let c = client(&state, &ip).await?;
    let cancel = Arc::new(AtomicBool::new(false));
    state
        .downloads
        .lock()
        .unwrap()
        .insert(format!("{ip}|{path}"), cancel.clone());
    let (app2, ip2, path2) = (app.clone(), ip.clone(), path.clone());
    let progress = move |done: u64, total: u64| {
        let _ = app2.emit(
            "download-progress",
            DownloadProgress {
                ip: ip2.clone(),
                path: path2.clone(),
                done,
                total,
            },
        );
    };
    let r = c.download(&path, &PathBuf::from(dest), &progress, &cancel).await;
    state.downloads.lock().unwrap().remove(&format!("{ip}|{path}"));
    r.map_err(err)
}

#[tauri::command]
pub fn cancel_download(state: State<'_, AppState>, ip: String, path: String) {
    if let Some(c) = state.downloads.lock().unwrap().get(&format!("{ip}|{path}")) {
        c.store(true, Ordering::Relaxed);
    }
}

/// Preview image (PNG) of a file stored on the printer, read from the file
/// header with HTTP range requests. Errors when no preview is available.
#[tauri::command]
pub async fn printer_thumbnail(
    state: State<'_, AppState>,
    ip: String,
    path: String,
) -> CmdResult<tauri::ipc::Response> {
    let key = format!("{ip}|{path}");
    if let Some(cached) = state.remote_thumbs.lock().unwrap().get(&key).cloned() {
        return cached
            .map(tauri::ipc::Response::new)
            .ok_or_else(|| "no preview".to_string());
    }
    let c = client(&state, &ip).await?;
    let png = remote_thumbnail(&c, &path).await;
    state.remote_thumbs.lock().unwrap().insert(key, png.clone());
    png.map(tauri::ipc::Response::new)
        .ok_or_else(|| "no preview".to_string())
}

async fn remote_thumbnail(c: &PrinterClient, path: &str) -> Option<Vec<u8>> {
    use openresin_formats::{ctb, goo, Thumbnail};
    let lower = path.to_ascii_lowercase();
    if !c.can_download(path) {
        return None;
    }
    if lower.ends_with(".goo") {
        let head = c.read_range(path, 0, goo::HEADER_SIZE as u64).await.ok()?;
        let h = goo::GooHeader::decode(&head).ok()?;
        if h.big_preview.iter().all(|&p| p == 0) {
            return None;
        }
        return Thumbnail::from_rgb565(goo::BIG_PREVIEW, goo::BIG_PREVIEW, &h.big_preview)
            .to_png()
            .ok();
    }
    if lower.ends_with(".ctb") {
        let head = c.read_range(path, 0, 112).await.ok()?;
        let magic = u32::from_le_bytes(head.get(0..4)?.try_into().ok()?);
        if magic != ctb::MAGIC_CTB && magic != ctb::MAGIC_CTB_V4 {
            return None; // encrypted container: no readable preview
        }
        let off = u32::from_le_bytes(head.get(60..64)?.try_into().ok()?) as u64;
        let ph = c.read_range(path, off, 16).await.ok()?;
        let w = u32::from_le_bytes(ph.get(0..4)?.try_into().ok()?);
        let h = u32::from_le_bytes(ph.get(4..8)?.try_into().ok()?);
        let data_off = u32::from_le_bytes(ph.get(8..12)?.try_into().ok()?) as u64;
        let len = u32::from_le_bytes(ph.get(12..16)?.try_into().ok()?) as u64;
        if w == 0 || h == 0 || w > 2048 || h > 2048 || len > 16 << 20 {
            return None;
        }
        let data = c.read_range(path, data_off, len).await.ok()?;
        return ctb::decode_preview(w, h, &data).to_png().ok();
    }
    None
}

#[tauri::command]
pub fn cancel_upload(state: State<'_, AppState>, ip: String) {
    if let Some(c) = state.uploads.lock().unwrap().get(&ip) {
        c.store(true, Ordering::Relaxed);
    }
}

#[derive(Serialize)]
pub struct CameraUrls {
    pub stream_url: String,
    pub snapshot_url: String,
    pub source: String,
}

#[tauri::command]
pub async fn camera_start(state: State<'_, AppState>, ip: String, url: Option<String>) -> CmdResult<CameraUrls> {
    if let Some(cam) = state.cameras.lock().await.get(&ip) {
        return Ok(CameraUrls {
            stream_url: cam.stream_url.clone(),
            snapshot_url: cam.snapshot_url.clone(),
            source: String::new(),
        });
    }
    let source = match url {
        Some(u) if !u.trim().is_empty() => u,
        _ => {
            let c = client(&state, &ip).await?;
            if !c.supports_camera() {
                return Err("this printer has no network camera".into());
            }
            match c.enable_video(true).await {
                Ok(Some(u)) => u,
                Ok(None) => return Err("the printer did not return a camera URL".into()),
                // some firmwares refuse when all viewer slots are taken; the
                // standard RTSP endpoint usually still works
                Err(e) => {
                    log::warn!("enable video failed ({e}), falling back to the default RTSP URL");
                    format!("rtsp://{ip}:554/video")
                }
            }
        }
    };
    let relay = CameraRelay::start(&source).await.map_err(err)?;
    let urls = CameraUrls {
        stream_url: relay.stream_url.clone(),
        snapshot_url: relay.snapshot_url.clone(),
        source,
    };
    state.cameras.lock().await.insert(ip, relay);
    Ok(urls)
}

#[tauri::command]
pub async fn camera_stop(state: State<'_, AppState>, ip: String) -> CmdResult<()> {
    if let Some(cam) = state.cameras.lock().await.remove(&ip) {
        cam.shutdown().await;
    }
    if let Ok(c) = client(&state, &ip).await {
        let _ = c.enable_video(false).await;
    }
    Ok(())
}

#[tauri::command]
pub async fn camera_snapshot(state: State<'_, AppState>, ip: String, path: String) -> CmdResult<()> {
    let cams = state.cameras.lock().await;
    let cam = cams.get(&ip).ok_or("camera is not running")?;
    let f = cam.latest().ok_or("no camera frame received yet")?;
    std::fs::write(path, &f.jpeg).map_err(err)
}
