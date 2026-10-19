//! Scene, support, project and slicing commands.

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use openresin_core::geometry::Aabb;
use openresin_core::profile::{PrintSettings, PrinterProfile};
use openresin_core::scene::{ObjectId, Scene, SceneObject, Transform};
use openresin_core::support::{auto_points, SupportPoint, SupportSettings};
use openresin_core::{Mesh, Vec3};
use openresin_formats::{preview, slice_to_file, ExportJob, Format, SlicedInfo, Thumbnail};
use serde::Serialize;
use tauri::ipc::Response;
use tauri::{AppHandle, Emitter, Runtime, State};

use crate::state::{AppState, SliceOutput};

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(test, derive(serde::Deserialize, PartialEq))]
pub struct ObjectInfo {
    pub id: ObjectId,
    pub name: String,
    pub triangles: usize,
    pub transform: Transform,
    pub bounds: Aabb,
    pub support_points: usize,
    pub volume_mm3: f64,
    /// Changes whenever the object's mesh is replaced (repair, cut, …).
    pub mesh_key: u64,
    /// Material role: solid parts are added, `subtract` parts are cut away.
    pub subtract: bool,
    /// Number of cutout volumes (hollowing cavity, drain holes).
    pub cutouts: usize,
}

pub(crate) fn info(o: &SceneObject) -> ObjectInfo {
    let s = o.transform.scale;
    ObjectInfo {
        id: o.id,
        name: o.name.clone(),
        triangles: o.mesh.triangle_count(),
        transform: o.transform,
        bounds: o.world_bounds(),
        support_points: o.supports.len(),
        volume_mm3: o.mesh.signed_volume().abs() * (s.x * s.y * s.z).abs() as f64,
        mesh_key: std::sync::Arc::as_ptr(&o.mesh) as usize as u64,
        subtract: o.role == openresin_core::scene::ObjectRole::Subtract,
        cutouts: o.cutouts.len(),
    }
}

fn with_object<T>(state: &AppState, id: ObjectId, f: impl FnOnce(&mut Scene, ObjectId) -> T) -> CmdResult<T> {
    let mut scene = state.scene.lock().unwrap();
    if scene.get(id).is_none() {
        return Err(format!("object {id} not found"));
    }
    Ok(f(&mut scene, id))
}

/// Fails when `id` is not in the scene. Called before [`checkpoint`] so that a
/// rejected edit leaves the undo/redo history alone.
pub(crate) fn ensure_object(state: &AppState, id: ObjectId) -> CmdResult<()> {
    if state.scene.lock().unwrap().get(id).is_none() {
        return Err(format!("object {id} not found"));
    }
    Ok(())
}

/// Saves the current scene on the undo stack before a modification.
pub(crate) fn checkpoint(state: &AppState) {
    let snapshot = state.scene.lock().unwrap().clone();
    let mut h = state.history.lock().unwrap();
    h.undo.push(snapshot);
    if h.undo.len() > 50 {
        h.undo.remove(0);
    }
    h.redo.clear();
}

/// The plate is about to change: invalidate the last slice result.
pub(crate) fn invalidate_slice(state: &AppState) {
    *state.slice.lock().unwrap() = None;
}

// ------------------------------------------------------------------ printers & settings

#[derive(Serialize)]
pub struct ConfigView {
    pub printer: PrinterProfile,
    pub settings: PrintSettings,
    pub support_settings: SupportSettings,
    pub devices: Vec<crate::state::SavedDevice>,
    pub theme: String,
    pub formats: Vec<FormatView>,
}

#[derive(Serialize)]
pub struct FormatView {
    pub id: Format,
    pub extension: &'static str,
    pub description: &'static str,
}

/// Files passed on the command line ("open with", file associations).
#[tauri::command]
pub fn startup_files() -> Vec<String> {
    std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .filter(|a| std::path::Path::new(a).is_file())
        .collect()
}

/// Command line flags (e.g. `--printers` opens the printer manager).
#[tauri::command]
pub fn startup_flags() -> Vec<String> {
    std::env::args().skip(1).filter(|a| a.starts_with("--")).collect()
}

#[tauri::command]
pub fn list_printers(state: State<'_, AppState>) -> Vec<PrinterProfile> {
    state.printers()
}

#[tauri::command]
pub fn get_config(state: State<'_, AppState>) -> ConfigView {
    let printer = state.current_printer();
    let cfg = state.config.lock().unwrap();
    ConfigView {
        settings: cfg.settings_for(&printer.id),
        printer,
        support_settings: cfg.support_settings.clone(),
        devices: cfg.devices.clone(),
        theme: cfg.theme.clone(),
        formats: Format::ALL
            .iter()
            .map(|f| FormatView {
                id: *f,
                extension: f.extension(),
                description: f.description(),
            })
            .collect(),
    }
}

#[tauri::command]
pub fn set_printer(state: State<'_, AppState>, id: String) -> CmdResult<ConfigView> {
    if !state.printers().iter().any(|p| p.id == id) {
        return Err(format!("unknown printer {id}"));
    }
    {
        let mut cfg = state.config.lock().unwrap();
        cfg.printer_id = id;
        cfg.save();
    }
    invalidate_slice(&state);
    Ok(get_config(state))
}

#[tauri::command]
pub fn set_settings(state: State<'_, AppState>, settings: PrintSettings) -> CmdResult<()> {
    settings.validate()?;
    let mut cfg = state.config.lock().unwrap();
    let id = cfg.printer_id.clone();
    cfg.settings.insert(id, settings);
    cfg.save();
    drop(cfg);
    invalidate_slice(&state);
    Ok(())
}

#[tauri::command]
pub fn set_support_settings(state: State<'_, AppState>, settings: SupportSettings) {
    let mut cfg = state.config.lock().unwrap();
    cfg.support_settings = settings;
    cfg.save();
}

#[tauri::command]
pub fn save_custom_printer(state: State<'_, AppState>, printer: PrinterProfile) -> CmdResult<ConfigView> {
    if printer.resolution_x == 0 || printer.resolution_y == 0 || printer.size_x <= 0.0 || printer.size_y <= 0.0 {
        return Err("resolution and size must be positive".into());
    }
    {
        let mut cfg = state.config.lock().unwrap();
        cfg.custom_printers.retain(|p| p.id != printer.id);
        cfg.printer_id = printer.id.clone();
        cfg.custom_printers.push(printer);
        cfg.save();
    }
    invalidate_slice(&state);
    Ok(get_config(state))
}

#[tauri::command]
pub fn set_theme(state: State<'_, AppState>, theme: String) {
    let mut cfg = state.config.lock().unwrap();
    cfg.theme = theme;
    cfg.save();
}

// ------------------------------------------------------------------ scene

#[tauri::command]
pub async fn import_models(state: State<'_, AppState>, paths: Vec<String>) -> CmdResult<Vec<ObjectInfo>> {
    let loaded = tauri::async_runtime::spawn_blocking(move || {
        let mut out = Vec::new();
        for p in paths {
            let models = openresin_core::io::load_models(&PathBuf::from(&p)).map_err(err)?;
            out.extend(models);
        }
        Ok::<_, String>(out)
    })
    .await
    .map_err(err)??;
    checkpoint(&state);
    let printer = state.current_printer();
    let mut scene = state.scene.lock().unwrap();
    let mut ids = Vec::new();
    for m in loaded {
        ids.push(scene.add(m.name, m.mesh));
    }
    if scene.objects.len() > 1 {
        scene.arrange(&printer, 3.0);
    }
    drop(scene);
    invalidate_slice(&state);
    let scene = state.scene.lock().unwrap();
    Ok(ids.iter().filter_map(|id| scene.get(*id)).map(info).collect())
}

#[tauri::command]
pub fn scene_objects(state: State<'_, AppState>) -> Vec<ObjectInfo> {
    state.scene.lock().unwrap().objects.iter().map(info).collect()
}

/// Mesh geometry: `[u32 vertex count][u32 triangle count][f32 xyz…][u32 indices…]` (little endian).
#[tauri::command]
pub fn get_mesh(state: State<'_, AppState>, id: ObjectId) -> CmdResult<Response> {
    let scene = state.scene.lock().unwrap();
    let o = scene.get(id).ok_or("object not found")?;
    Ok(Response::new(encode_mesh(&o.mesh)))
}

pub fn encode_mesh(m: &Mesh) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + m.positions.len() * 12 + m.triangles.len() * 12);
    out.extend_from_slice(&(m.positions.len() as u32).to_le_bytes());
    out.extend_from_slice(&(m.triangles.len() as u32).to_le_bytes());
    for p in &m.positions {
        for v in [p.x, p.y, p.z] {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    for t in &m.triangles {
        for i in t {
            out.extend_from_slice(&i.to_le_bytes());
        }
    }
    out
}

#[tauri::command]
pub fn set_transform(
    state: State<'_, AppState>,
    id: ObjectId,
    transform: Transform,
    keep_on_plate: bool,
) -> CmdResult<ObjectInfo> {
    ensure_object(&state, id)?;
    checkpoint(&state);
    let r = with_object(&state, id, |scene, id| {
        let o = scene.get_mut(id).unwrap();
        let old = o.transform;
        let same_rotation = old.rotation.angle_between(transform.rotation) <= 1e-4;
        // mirroring in X/Y keeps supports valid (they mirror with the model);
        // any other rotation/scale change invalidates the vertical support structures
        let xy_mirror_only = same_rotation
            && (old.scale.abs() - transform.scale.abs()).length() <= 1e-6
            && old.scale.z.signum() == transform.scale.z.signum();
        let rotation_changed = !xy_mirror_only;
        o.transform = Transform {
            rotation: transform.rotation.normalize(),
            ..transform
        };
        if rotation_changed && !o.supports.is_empty() {
            // supports are vertical structures; they no longer fit a rotated model
            o.supports.clear();
        }
        if keep_on_plate {
            let lift = if o.supports.is_empty() {
                0.0
            } else {
                o.support_settings.lift_height
            };
            scene.drop_to(id, lift);
        }
        info(scene.get(id).unwrap())
    })?;
    invalidate_slice(&state);
    Ok(r)
}

#[tauri::command]
pub fn remove_objects(state: State<'_, AppState>, ids: Vec<ObjectId>) {
    checkpoint(&state);
    let mut scene = state.scene.lock().unwrap();
    for id in ids {
        scene.remove(id);
    }
    drop(scene);
    invalidate_slice(&state);
}

#[tauri::command]
pub fn duplicate_object(state: State<'_, AppState>, id: ObjectId, count: u32) -> CmdResult<Vec<ObjectInfo>> {
    ensure_object(&state, id)?;
    checkpoint(&state);
    let printer = state.current_printer();
    let mut scene = state.scene.lock().unwrap();
    let mut out = Vec::new();
    for _ in 0..count.clamp(1, 100) {
        let new_id = scene.duplicate(id).ok_or("object not found")?;
        out.push(new_id);
    }
    scene.arrange(&printer, 3.0);
    let r = out.iter().filter_map(|i| scene.get(*i)).map(info).collect();
    drop(scene);
    invalidate_slice(&state);
    Ok(r)
}

#[tauri::command]
pub fn arrange(state: State<'_, AppState>) -> (bool, Vec<ObjectInfo>) {
    checkpoint(&state);
    let printer = state.current_printer();
    let mut scene = state.scene.lock().unwrap();
    let fits = scene.arrange(&printer, 3.0);
    let r = scene.objects.iter().map(info).collect();
    drop(scene);
    invalidate_slice(&state);
    (fits, r)
}

#[tauri::command]
pub fn lay_flat(state: State<'_, AppState>, id: ObjectId, face: Option<u32>) -> CmdResult<ObjectInfo> {
    ensure_object(&state, id)?;
    checkpoint(&state);
    let r = with_object(&state, id, |scene, id| {
        let o = scene.get_mut(id).unwrap();
        let world = o.world_mesh();
        let q = match face {
            Some(f) if (f as usize) < world.triangle_count() => world.lay_flat_rotation(f as usize),
            _ => world.best_flat_rotation(),
        };
        o.transform.rotation = (q * o.transform.rotation).normalize();
        o.supports.clear();
        scene.drop_to(id, 0.0);
        info(scene.get(id).unwrap())
    })?;
    invalidate_slice(&state);
    Ok(r)
}

// ------------------------------------------------------------------ supports

#[tauri::command]
pub async fn auto_supports(
    state: State<'_, AppState>,
    ids: Vec<ObjectId>,
    settings: SupportSettings,
) -> CmdResult<Vec<ObjectInfo>> {
    // `settings` only applies to these objects. The user's defaults are saved
    // by `set_support_settings` alone: Magic Support passes derived settings
    // (denser, steeper), and storing them made every click compound the change.
    checkpoint(&state);
    let mut out = Vec::new();
    for id in ids {
        let world = {
            let mut scene = state.scene.lock().unwrap();
            if scene.get(id).is_none() {
                continue;
            }
            scene.drop_to(id, settings.lift_height);
            let o = scene.get_mut(id).unwrap();
            o.support_settings = settings.clone();
            o.world_mesh()
        };
        let s2 = settings.clone();
        let pts = tauri::async_runtime::spawn_blocking(move || auto_points(&world, &s2))
            .await
            .map_err(err)?;
        let mut scene = state.scene.lock().unwrap();
        if let Some(o) = scene.get_mut(id) {
            o.set_world_support_points(&pts);
            out.push(info(o));
        }
    }
    invalidate_slice(&state);
    Ok(out)
}

#[tauri::command]
pub fn get_support_points(state: State<'_, AppState>, id: ObjectId) -> CmdResult<Vec<SupportPoint>> {
    let scene = state.scene.lock().unwrap();
    Ok(scene.get(id).ok_or("object not found")?.world_support_points())
}

#[tauri::command]
pub fn set_support_points(
    state: State<'_, AppState>,
    id: ObjectId,
    points: Vec<SupportPoint>,
) -> CmdResult<ObjectInfo> {
    ensure_object(&state, id)?;
    checkpoint(&state);
    let r = with_object(&state, id, |scene, id| {
        let o = scene.get_mut(id).unwrap();
        let lift = o.support_settings.lift_height;
        let was_empty = o.supports.is_empty();
        o.set_world_support_points(&points);
        if was_empty && !points.is_empty() && o.elevation() < lift - 1e-3 {
            // first manual support: lift the model (points move with it)
            let dz = lift - o.elevation();
            o.transform.translation.z += dz;
        }
        info(scene.get(id).unwrap())
    })?;
    invalidate_slice(&state);
    Ok(r)
}

#[tauri::command]
pub fn clear_supports(state: State<'_, AppState>, id: ObjectId) -> CmdResult<ObjectInfo> {
    ensure_object(&state, id)?;
    checkpoint(&state);
    let r = with_object(&state, id, |scene, id| {
        scene.get_mut(id).unwrap().supports.clear();
        scene.drop_to(id, 0.0);
        info(scene.get(id).unwrap())
    })?;
    invalidate_slice(&state);
    Ok(r)
}

/// Support geometry in plate coordinates (same encoding as `get_mesh`).
#[tauri::command]
pub async fn get_support_mesh(state: State<'_, AppState>, id: ObjectId) -> CmdResult<Response> {
    let obj = state.scene.lock().unwrap().get(id).cloned().ok_or("object not found")?;
    let mesh = tauri::async_runtime::spawn_blocking(move || obj.support_mesh().mesh)
        .await
        .map_err(err)?;
    Ok(Response::new(encode_mesh(&mesh)))
}

/// Finds the closest surface point under a click ray (for manual supports).
#[tauri::command]
pub fn pick_surface(
    state: State<'_, AppState>,
    id: ObjectId,
    origin: [f32; 3],
    direction: [f32; 3],
) -> CmdResult<Option<SupportPoint>> {
    let scene = state.scene.lock().unwrap();
    let o = scene.get(id).ok_or("object not found")?;
    let world = o.world_mesh();
    let orig = Vec3::from(origin);
    let dir = Vec3::from(direction).normalize_or_zero();
    let mut best: Option<(f32, usize)> = None;
    for i in 0..world.triangle_count() {
        let [a, b, c] = world.tri(i);
        if let Some(t) = openresin_core::geometry::ray_triangle(orig, dir, a, b, c) {
            if best.is_none_or(|(bt, _)| t < bt) {
                best = Some((t, i));
            }
        }
    }
    Ok(best.map(|(t, i)| SupportPoint {
        position: orig + dir * t,
        normal: world.face_normal(i),
        size: o.support_settings.default_size,
    }))
}

// ------------------------------------------------------------------ project

#[tauri::command]
pub fn save_project(state: State<'_, AppState>, path: String) -> CmdResult<()> {
    let printer = state.current_printer();
    let settings = state.current_settings();
    let scene = state.scene.lock().unwrap();
    openresin_core::project::save(&PathBuf::from(path), &scene, Some(&printer.id), Some(&settings)).map_err(err)
}

#[tauri::command]
pub fn open_project(state: State<'_, AppState>, path: String) -> CmdResult<Vec<ObjectInfo>> {
    let p = openresin_core::project::load(&PathBuf::from(path)).map_err(err)?;
    checkpoint(&state);
    // `printers()` locks the config itself: look the printer up first
    let known_printer = p
        .printer_id
        .as_ref()
        .filter(|id| state.printers().iter().any(|x| &x.id == *id));
    {
        let mut cfg = state.config.lock().unwrap();
        if let Some(id) = known_printer {
            cfg.printer_id = id.clone();
        }
        if let Some(s) = p.settings {
            let id = cfg.printer_id.clone();
            cfg.settings.insert(id, s);
        }
        cfg.save();
    }
    let mut scene = state.scene.lock().unwrap();
    *scene = p.scene;
    let r = scene.objects.iter().map(info).collect();
    drop(scene);
    invalidate_slice(&state);
    Ok(r)
}

#[tauri::command]
pub fn new_project(state: State<'_, AppState>) {
    checkpoint(&state);
    *state.scene.lock().unwrap() = Scene::default();
    invalidate_slice(&state);
}

#[tauri::command]
pub fn export_stl(state: State<'_, AppState>, path: String, with_supports: bool) -> CmdResult<()> {
    let scene = state.scene.lock().unwrap();
    let mut mesh = Mesh::default();
    for o in &scene.objects {
        mesh.append(&o.world_mesh());
        if with_supports {
            mesh.append(&o.support_mesh().mesh);
        }
    }
    let f = std::fs::File::create(path).map_err(err)?;
    openresin_core::io::write_binary_stl(&mesh, std::io::BufWriter::new(f)).map_err(err)
}

// ------------------------------------------------------------------ slicing

#[derive(Debug, Clone, Serialize)]
pub struct SliceSummary {
    pub format: Format,
    pub extension: &'static str,
    pub layer_count: u32,
    pub print_time_s: f64,
    pub volume_ml: f64,
    pub weight_g: f64,
    pub cost: f64,
    pub currency: String,
    pub height_mm: f64,
    pub file_size: u64,
    pub duration_ms: u128,
    pub info: SlicedInfo,
    pub layer_areas: Vec<f64>,
    pub suggested_name: String,
    pub warnings: Vec<String>,
}

fn decode_data_url(s: &str) -> Option<Vec<u8>> {
    let b64 = s.split_once(',').map(|x| x.1).unwrap_or(s);
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(b64.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for c in b64.bytes() {
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

fn suggested_name(scene: &Scene, s: &PrintSettings, ext: &str) -> String {
    let base = scene
        .objects
        .first()
        .map(|o| o.name.clone())
        .unwrap_or_else(|| "plate".into());
    let base: String = base
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{base}_{:.3}_{:.1}.{ext}", s.layer_height, s.normal.exposure)
}

#[tauri::command]
pub async fn slice<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    format: Option<Format>,
    thumbnail: Option<String>,
) -> CmdResult<SliceSummary> {
    let printer = state.current_printer();
    let settings = state.current_settings();
    let format = format
        .or_else(|| printer.formats.iter().find_map(|f| Format::from_extension(f)))
        .unwrap_or(Format::Zip);
    let (meshes, warnings, name) = {
        let scene = state.scene.lock().unwrap();
        if scene.objects.is_empty() {
            return Err("the build plate is empty".into());
        }
        let mut warnings = Vec::new();
        let oob = scene.out_of_bounds(&printer);
        if !oob.is_empty() {
            return Err(format!(
                "{} object(s) are outside the printable area of {}",
                oob.len(),
                printer.display_name()
            ));
        }
        for o in scene
            .objects
            .iter()
            .filter(|o| o.role == openresin_core::scene::ObjectRole::Solid)
        {
            if o.supports.is_empty() && o.elevation() > 0.05 {
                warnings.push(format!("{} floats above the plate without supports", o.name));
            }
        }
        (
            scene.slice_volumes(),
            warnings,
            suggested_name(&scene, &settings, format.extension()),
        )
    };
    let thumb = thumbnail
        .and_then(|t| decode_data_url(&t))
        .and_then(|png| Thumbnail::from_png(&png).ok());
    // recreated in case it was removed (e.g. by `cleanup_work_dir`)
    std::fs::create_dir_all(&state.work_dir).map_err(err)?;
    let out_path = state.work_dir.join(&name);
    let cancel = state.cancel_slice.clone();
    cancel.store(false, Ordering::Relaxed);
    let app2 = app.clone();
    let printer2 = printer.clone();
    let settings2 = settings.clone();
    let path2 = out_path.clone();
    let name2 = name.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let (meshes, negatives) = meshes;
        let refs: Vec<&Mesh> = meshes.iter().collect();
        let neg_refs: Vec<&Mesh> = negatives.iter().collect();
        let thumb = thumb.unwrap_or_else(|| preview::render_meshes(&refs, 400, 400));
        let job = ExportJob {
            meshes: &refs,
            negatives: &neg_refs,
            printer: &printer2,
            settings: &settings2,
            format,
            thumbnail: Some(&thumb),
            job_name: &name2,
        };
        slice_to_file(
            &job,
            &path2,
            &|p| {
                let _ = app2.emit("slice-progress", p);
            },
            &cancel,
        )
    })
    .await
    .map_err(err)?
    .map_err(err)?;
    let file = openresin_formats::open(&res.path).map_err(err)?;
    let e = &res.estimate;
    let summary = SliceSummary {
        format,
        extension: format.extension(),
        layer_count: e.layer_count,
        print_time_s: e.print_time_s,
        volume_ml: e.volume_ml(),
        weight_g: e.weight_g,
        cost: e.cost,
        currency: e.currency.clone(),
        height_mm: e.height_mm,
        file_size: res.file_size,
        duration_ms: res.duration.as_millis(),
        info: file.info().clone(),
        layer_areas: res.layer_areas.clone(),
        suggested_name: name.clone(),
        warnings,
    };
    *state.slice.lock().unwrap() = Some(SliceOutput {
        path: res.path,
        file,
        job_name: name,
    });
    Ok(summary)
}

#[tauri::command]
pub fn cancel_slice(state: State<'_, AppState>) {
    state.cancel_slice.store(true, Ordering::Relaxed);
}

/// PNG of a sliced layer, downscaled to fit `max_size` (0 = full resolution).
#[tauri::command]
pub async fn get_layer_image(state: State<'_, AppState>, index: u32, max_size: u32) -> CmdResult<Response> {
    let raster = {
        let slice = state.slice.lock().unwrap();
        let s = slice.as_ref().ok_or("nothing sliced yet")?;
        s.file.layer(index).map_err(err)?
    };
    let png = tauri::async_runtime::spawn_blocking(move || -> Result<Vec<u8>, String> {
        let (w, h, px) = if max_size == 0 {
            (raster.width, raster.height, raster.to_gray())
        } else {
            raster.downscale(max_size, max_size)
        };
        let img = image::GrayImage::from_raw(w, h, px).ok_or("bad image size")?;
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).map_err(err)?;
        Ok(out.into_inner())
    })
    .await
    .map_err(err)??;
    Ok(Response::new(png))
}

#[tauri::command]
pub fn save_slice(state: State<'_, AppState>, path: String) -> CmdResult<()> {
    let slice = state.slice.lock().unwrap();
    let s = slice.as_ref().ok_or("nothing sliced yet")?;
    std::fs::copy(&s.path, &path).map_err(err)?;
    Ok(())
}

#[tauri::command]
pub fn open_sliced_file(state: State<'_, AppState>, path: String) -> CmdResult<SliceSummary> {
    let p = PathBuf::from(&path);
    let file = openresin_formats::open(&p).map_err(err)?;
    let info = file.info().clone();
    let format = p
        .extension()
        .and_then(|e| e.to_str())
        .and_then(Format::from_extension)
        .unwrap_or(Format::Zip);
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string();
    let summary = SliceSummary {
        format,
        extension: format.extension(),
        layer_count: info.layer_count,
        print_time_s: info.print_time_s as f64,
        volume_ml: info.volume_ml as f64,
        weight_g: 0.0,
        cost: 0.0,
        currency: String::new(),
        height_mm: info.layer_count as f64 * info.layer_height as f64,
        file_size: std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0),
        duration_ms: 0,
        info,
        layer_areas: Vec::new(),
        suggested_name: name.clone(),
        warnings: Vec::new(),
    };
    *state.slice.lock().unwrap() = Some(SliceOutput {
        path: p,
        file,
        job_name: name,
    });
    Ok(summary)
}
