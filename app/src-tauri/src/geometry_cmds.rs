//! Mesh tools: error detection, repair, split and cut.

use std::sync::Arc;

use openresin_core::repair::{analyze, repair, MeshReport, RepairOptions, RepairReport};
use openresin_core::scene::{ObjectId, Scene};
use openresin_core::{Mesh, Vec3};
use serde::Serialize;
use tauri::State;

use crate::scene_cmds::{checkpoint, ensure_object, info, invalidate_slice, ObjectInfo};
use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

fn all_infos(state: &AppState) -> Vec<ObjectInfo> {
    state.scene.lock().unwrap().objects.iter().map(info).collect()
}

/// Adds a world-space mesh as a new object, keeping it where it is.
fn add_in_place(scene: &mut Scene, name: String, world: Mesh) -> ObjectId {
    let b = world.bounds();
    let c = b.center();
    let id = scene.add(name, world);
    let o = scene.get_mut(id).unwrap();
    o.transform.translation = Vec3::new(c.x, c.y, c.z);
    id
}

#[tauri::command]
pub async fn analyze_object(state: State<'_, AppState>, id: ObjectId) -> CmdResult<MeshReport> {
    let mesh = state
        .scene
        .lock()
        .unwrap()
        .get(id)
        .ok_or("object not found")?
        .mesh
        .clone();
    tauri::async_runtime::spawn_blocking(move || analyze(&mesh))
        .await
        .map_err(|e| e.to_string())
}

#[derive(Serialize)]
pub struct RepairResult {
    pub id: ObjectId,
    pub name: String,
    pub report: RepairReport,
}

#[tauri::command]
pub async fn repair_objects(
    state: State<'_, AppState>,
    ids: Vec<ObjectId>,
    options: Option<RepairOptions>,
) -> CmdResult<(Vec<RepairResult>, Vec<ObjectInfo>)> {
    let opts = options.unwrap_or_default();
    checkpoint(&state);
    let mut results = Vec::new();
    for id in ids {
        let Some((mesh, name)) = state
            .scene
            .lock()
            .unwrap()
            .get(id)
            .map(|o| (o.mesh.clone(), o.name.clone()))
        else {
            continue;
        };
        let o2 = opts.clone();
        let (fixed, report) = tauri::async_runtime::spawn_blocking(move || repair(&mesh, &o2))
            .await
            .map_err(|e| e.to_string())?;
        let mut scene = state.scene.lock().unwrap();
        if let Some(o) = scene.get_mut(id) {
            if !fixed.is_empty() {
                o.mesh = Arc::new(fixed);
                o.supports.clear();
            }
        }
        let lift = scene.get(id).map(|o| {
            if o.supports.is_empty() {
                0.0
            } else {
                o.support_settings.lift_height
            }
        });
        if let Some(l) = lift {
            scene.drop_to(id, l);
        }
        results.push(RepairResult { id, name, report });
    }
    invalidate_slice(&state);
    Ok((results, all_infos(&state)))
}

/// Hollows an object: computes the inner cavity (in world space so the wall
/// thickness is exact even for scaled models) and stores it as a cutout.
/// Replaces previous cutouts (cavity and drain holes).
#[tauri::command]
pub async fn hollow_object(
    state: State<'_, AppState>,
    id: ObjectId,
    options: openresin_core::hollow::HollowOptions,
) -> CmdResult<(f64, ObjectInfo)> {
    let (world, matrix) = {
        let scene = state.scene.lock().unwrap();
        let o = scene.get(id).ok_or("object not found")?;
        (o.world_mesh(), o.transform.matrix())
    };
    let res = tauri::async_runtime::spawn_blocking(move || openresin_core::hollow::hollow_with_info(&world, &options))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    if res.cavity.is_empty() {
        return Err("the model is too thin to hollow with this wall thickness".into());
    }
    checkpoint(&state);
    let local = res.cavity.transformed(&matrix.inverse());
    let info = {
        let mut scene = state.scene.lock().unwrap();
        let o = scene.get_mut(id).ok_or("object not found")?;
        o.cutouts = vec![Arc::new(local)];
        info(o)
    };
    invalidate_slice(&state);
    Ok((res.cavity_volume, info))
}

/// Adds a drain hole at a surface point (world coordinates, outward normal).
#[tauri::command]
pub fn add_drain_hole(
    state: State<'_, AppState>,
    id: ObjectId,
    position: [f32; 3],
    normal: [f32; 3],
    diameter: f32,
    depth: f32,
) -> CmdResult<ObjectInfo> {
    let hole = openresin_core::hollow::drain_hole(Vec3::from(position), -Vec3::from(normal), diameter, depth);
    if hole.is_empty() {
        return Err("invalid drain hole".into());
    }
    ensure_object(&state, id)?;
    checkpoint(&state);
    let r = {
        let mut scene = state.scene.lock().unwrap();
        let o = scene.get_mut(id).ok_or("object not found")?;
        let local = hole.transformed(&o.transform.matrix().inverse());
        o.cutouts.push(Arc::new(local));
        info(o)
    };
    invalidate_slice(&state);
    Ok(r)
}

/// Removes hollowing and drain holes.
#[tauri::command]
pub fn clear_cutouts(state: State<'_, AppState>, id: ObjectId) -> CmdResult<ObjectInfo> {
    ensure_object(&state, id)?;
    checkpoint(&state);
    let r = {
        let mut scene = state.scene.lock().unwrap();
        let o = scene.get_mut(id).ok_or("object not found")?;
        o.cutouts.clear();
        info(o)
    };
    invalidate_slice(&state);
    Ok(r)
}

/// Cutout volumes (cavity, holes) of an object in world coordinates.
#[tauri::command]
pub fn get_cutout_mesh(state: State<'_, AppState>, id: ObjectId) -> CmdResult<tauri::ipc::Response> {
    let scene = state.scene.lock().unwrap();
    let o = scene.get(id).ok_or("object not found")?;
    let m = o.transform.matrix();
    let mut all = Mesh::default();
    for c in &o.cutouts {
        all.append(&c.transformed(&m));
    }
    Ok(tauri::ipc::Response::new(crate::scene_cmds::encode_mesh(&all)))
}

/// Adds embossed (merged into the model) or engraved (cut out) text at a
/// surface point given in world coordinates.
#[tauri::command]
pub fn add_label(
    state: State<'_, AppState>,
    id: ObjectId,
    text: String,
    options: openresin_core::label::LabelOptions,
    position: [f32; 3],
    normal: [f32; 3],
) -> CmdResult<ObjectInfo> {
    if text.trim().is_empty() {
        return Err("enter some text first".into());
    }
    let n = Vec3::from(normal).normalize_or_zero();
    // keep text upright: "up" is +Z unless the surface faces up/down
    let up = if n.z.abs() > 0.9 { Vec3::Y } else { Vec3::Z };
    let world = openresin_core::label::place_label(&text, &options, Vec3::from(position), n, up);
    if world.is_empty() {
        return Err("the text contains no printable characters".into());
    }
    ensure_object(&state, id)?;
    checkpoint(&state);
    let r = {
        let mut scene = state.scene.lock().unwrap();
        let o = scene.get_mut(id).ok_or("object not found")?;
        let local = world.transformed(&o.transform.matrix().inverse());
        match options.mode {
            openresin_core::label::LabelMode::Emboss => {
                let mut m = (*o.mesh).clone();
                m.append(&local);
                o.mesh = Arc::new(m);
            }
            openresin_core::label::LabelMode::Engrave => o.cutouts.push(Arc::new(local)),
        }
        info(o)
    };
    invalidate_slice(&state);
    Ok(r)
}

/// Puts the model on a solid pad ("platform") instead of supports.
#[tauri::command]
pub fn add_platform(
    state: State<'_, AppState>,
    id: ObjectId,
    options: openresin_core::primitives::BasePlateOptions,
) -> CmdResult<ObjectInfo> {
    // build the result on a copy: an invalid pad must not leave the model
    // half modified (moved, supports removed)
    let mut o = state.scene.lock().unwrap().get(id).cloned().ok_or("object not found")?;
    // lift the model onto the pad, slightly sunk in so they fuse
    let lift = (options.thickness - 0.2).max(0.0);
    o.transform.translation.z += lift - o.world_bounds().min.z;
    o.supports.clear();
    let world = o.world_mesh();
    let pad = openresin_core::primitives::base_plate_under(&world, &options);
    if pad.is_empty() {
        return Err("could not build a platform under this model".into());
    }
    let mut merged = world;
    merged.append(&pad);
    o.mesh = Arc::new(merged.transformed(&o.transform.matrix().inverse()));
    let r = info(&o);
    checkpoint(&state);
    if let Some(slot) = state.scene.lock().unwrap().get_mut(id) {
        *slot = o;
    }
    invalidate_slice(&state);
    Ok(r)
}

/// Splits an object into its separate parts (shells).
#[tauri::command]
pub async fn split_object(state: State<'_, AppState>, id: ObjectId) -> CmdResult<(usize, Vec<ObjectInfo>)> {
    let (world, name) = {
        let scene = state.scene.lock().unwrap();
        let o = scene.get(id).ok_or("object not found")?;
        (o.world_mesh(), o.name.clone())
    };
    let parts = tauri::async_runtime::spawn_blocking(move || openresin_core::split::split_shells(&world))
        .await
        .map_err(|e| e.to_string())?;
    if parts.len() < 2 {
        return Ok((1, all_infos(&state)));
    }
    checkpoint(&state);
    let n = parts.len();
    {
        let mut scene = state.scene.lock().unwrap();
        scene.remove(id);
        for (i, part) in parts.into_iter().enumerate() {
            add_in_place(&mut scene, format!("{name} part {}", i + 1), part);
        }
    }
    invalidate_slice(&state);
    Ok((n, all_infos(&state)))
}

/// Cuts an object with an axis-aligned plane at `position` (plate coordinates).
/// `axis` is "x", "y" or "z". Both halves stay where they are; for Z cuts the
/// upper half is put down on the plate next to the lower one.
#[tauri::command]
pub async fn cut_object(
    state: State<'_, AppState>,
    id: ObjectId,
    axis: String,
    position: f32,
) -> CmdResult<Vec<ObjectInfo>> {
    let (world, name) = {
        let scene = state.scene.lock().unwrap();
        let o = scene.get(id).ok_or("object not found")?;
        (o.world_mesh(), o.name.clone())
    };
    let (point, normal) = match axis.as_str() {
        "x" => (Vec3::new(position, 0.0, 0.0), Vec3::X),
        "y" => (Vec3::new(0.0, position, 0.0), Vec3::Y),
        "z" => (Vec3::new(0.0, 0.0, position), Vec3::Z),
        other => return Err(format!("unknown axis {other}")),
    };
    let (below, above) =
        tauri::async_runtime::spawn_blocking(move || openresin_core::cut::cut_mesh(&world, point, normal))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
    if below.is_empty() || above.is_empty() {
        return Err("the cut plane does not intersect the model".into());
    }
    checkpoint(&state);
    {
        let printer = state.current_printer();
        let mut scene = state.scene.lock().unwrap();
        scene.remove(id);
        let a = add_in_place(&mut scene, format!("{name} (1)"), below);
        let b = add_in_place(&mut scene, format!("{name} (2)"), above);
        scene.drop_to(a, 0.0);
        scene.drop_to(b, 0.0);
        if axis == "z" || scene.out_of_bounds(&printer).contains(&b) {
            scene.arrange(&printer, 3.0);
        }
    }
    invalidate_slice(&state);
    Ok(all_infos(&state))
}
