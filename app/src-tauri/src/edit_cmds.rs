//! Editing commands: undo/redo, copies, fill plate, align, combine, orientation.

use openresin_core::geometry::Aabb;
use openresin_core::scene::ObjectId;
use openresin_core::{Mesh, Vec3};
use tauri::State;

use crate::scene_cmds::{checkpoint, ensure_object, info, invalidate_slice, ObjectInfo};
use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

fn all_infos(state: &AppState) -> Vec<ObjectInfo> {
    state.scene.lock().unwrap().objects.iter().map(info).collect()
}

#[derive(serde::Serialize)]
pub struct HistoryState {
    pub can_undo: bool,
    pub can_redo: bool,
}

#[tauri::command]
pub fn history_state(state: State<'_, AppState>) -> HistoryState {
    let h = state.history.lock().unwrap();
    HistoryState {
        can_undo: !h.undo.is_empty(),
        can_redo: !h.redo.is_empty(),
    }
}

#[tauri::command]
pub fn undo(state: State<'_, AppState>) -> CmdResult<Vec<ObjectInfo>> {
    let prev = state.history.lock().unwrap().undo.pop().ok_or("nothing to undo")?;
    let cur = std::mem::replace(&mut *state.scene.lock().unwrap(), prev);
    state.history.lock().unwrap().redo.push(cur);
    invalidate_slice(&state);
    Ok(all_infos(&state))
}

#[tauri::command]
pub fn redo(state: State<'_, AppState>) -> CmdResult<Vec<ObjectInfo>> {
    let next = state.history.lock().unwrap().redo.pop().ok_or("nothing to redo")?;
    let cur = std::mem::replace(&mut *state.scene.lock().unwrap(), next);
    state.history.lock().unwrap().undo.push(cur);
    invalidate_slice(&state);
    Ok(all_infos(&state))
}

/// Adds copies of `id` until the plate is full. Returns all objects.
#[tauri::command]
pub fn fill_plate(
    state: State<'_, AppState>,
    id: ObjectId,
    spacing: Option<f32>,
) -> CmdResult<(usize, Vec<ObjectInfo>)> {
    let printer = state.current_printer();
    let spacing = spacing.unwrap_or(3.0).max(0.0);
    ensure_object(&state, id)?;
    checkpoint(&state);
    let mut added = 0;
    {
        let mut scene = state.scene.lock().unwrap();
        if scene.get(id).is_none() {
            return Err("object not found".into());
        }
        if !scene.arrange(&printer, spacing) {
            return Err("the plate is already full".into());
        }
        for _ in 0..1000 {
            let nid = scene.duplicate(id).ok_or("object not found")?;
            if !scene.arrange(&printer, spacing) {
                scene.remove(nid);
                scene.arrange(&printer, spacing);
                break;
            }
            added += 1;
        }
    }
    invalidate_slice(&state);
    Ok((added, all_infos(&state)))
}

/// Aligns objects relative to their common bounding box or the plate.
#[tauri::command]
pub fn align_objects(state: State<'_, AppState>, ids: Vec<ObjectId>, mode: String) -> CmdResult<Vec<ObjectInfo>> {
    checkpoint(&state);
    {
        let mut scene = state.scene.lock().unwrap();
        let group = ids
            .iter()
            .filter_map(|id| scene.get(*id))
            .fold(Aabb::EMPTY, |a, o| a.union(&o.world_bounds()));
        if group.is_empty() {
            return Err("nothing selected".into());
        }
        let gc = group.center();
        for id in &ids {
            let Some(o) = scene.get_mut(*id) else { continue };
            let b = o.world_bounds();
            let c = b.center();
            let d = match mode.as_str() {
                "left" => Vec3::new(group.min.x - b.min.x, 0.0, 0.0),
                "right" => Vec3::new(group.max.x - b.max.x, 0.0, 0.0),
                "front" => Vec3::new(0.0, group.min.y - b.min.y, 0.0),
                "back" => Vec3::new(0.0, group.max.y - b.max.y, 0.0),
                "center_x" => Vec3::new(gc.x - c.x, 0.0, 0.0),
                "center_y" => Vec3::new(0.0, gc.y - c.y, 0.0),
                "plate_center" => Vec3::new(-gc.x, -gc.y, 0.0),
                "drop" => Vec3::new(
                    0.0,
                    0.0,
                    -b.min.z
                        + if o.supports.is_empty() {
                            0.0
                        } else {
                            o.support_settings.lift_height
                        },
                ),
                other => return Err(format!("unknown align mode {other}")),
            };
            o.transform.translation += d;
        }
    }
    invalidate_slice(&state);
    Ok(all_infos(&state))
}

/// Merges several objects into one (supports are removed).
#[tauri::command]
pub fn combine_objects(state: State<'_, AppState>, ids: Vec<ObjectId>) -> CmdResult<Vec<ObjectInfo>> {
    let existing = {
        let scene = state.scene.lock().unwrap();
        let mut found: Vec<ObjectId> = ids.iter().copied().filter(|id| scene.get(*id).is_some()).collect();
        found.sort_unstable();
        found.dedup();
        found.len()
    };
    if existing < 2 {
        return Err("select at least two models to combine".into());
    }
    checkpoint(&state);
    {
        let mut scene = state.scene.lock().unwrap();
        let mut merged = Mesh::default();
        let mut names = Vec::new();
        for id in &ids {
            if let Some(o) = scene.remove(*id) {
                merged.append(&o.world_mesh());
                names.push(o.name);
            }
        }
        let c = merged.bounds().center();
        let min_z = merged.bounds().min.z;
        let nid = scene.add(
            format!("{} (combined)", names.first().cloned().unwrap_or_default()),
            merged,
        );
        let o = scene.get_mut(nid).unwrap();
        o.transform.translation = Vec3::new(c.x, c.y, o.transform.translation.z + min_z);
    }
    invalidate_slice(&state);
    Ok(all_infos(&state))
}

/// Rotates objects into a good printing orientation and drops them on the plate.
#[tauri::command]
pub async fn auto_orient(
    state: State<'_, AppState>,
    ids: Vec<ObjectId>,
    options: Option<openresin_core::orient::OrientOptions>,
) -> CmdResult<Vec<ObjectInfo>> {
    let options = options.unwrap_or_default();
    checkpoint(&state);
    for id in ids {
        let world = {
            let scene = state.scene.lock().unwrap();
            match scene.get(id) {
                Some(o) => o.world_mesh(),
                None => continue,
            }
        };
        let opts = options.clone();
        let q = tauri::async_runtime::spawn_blocking(move || openresin_core::orient::auto_orient(&world, &opts))
            .await
            .map_err(|e| e.to_string())?;
        let mut scene = state.scene.lock().unwrap();
        if let Some(o) = scene.get_mut(id) {
            o.transform.rotation = (q * o.transform.rotation).normalize();
            o.supports.clear();
        }
        scene.drop_to(id, 0.0);
    }
    invalidate_slice(&state);
    Ok(all_infos(&state))
}

/// Turns objects into subtracting volumes (boolean difference) or back into solids.
#[tauri::command]
pub fn set_subtract(state: State<'_, AppState>, ids: Vec<ObjectId>, subtract: bool) -> CmdResult<Vec<ObjectInfo>> {
    checkpoint(&state);
    {
        let mut scene = state.scene.lock().unwrap();
        for id in ids {
            if let Some(o) = scene.get_mut(id) {
                o.role = if subtract {
                    openresin_core::scene::ObjectRole::Subtract
                } else {
                    openresin_core::scene::ObjectRole::Solid
                };
                if subtract {
                    o.supports.clear();
                }
            }
        }
    }
    invalidate_slice(&state);
    Ok(all_infos(&state))
}

/// Adds a simple test shape (20 mm unless `size` is given).
#[tauri::command]
pub fn add_primitive(state: State<'_, AppState>, kind: String, size: Option<f32>) -> CmdResult<Vec<ObjectInfo>> {
    use openresin_core::primitives::{cuboid, frustum, sphere};
    let s = size.unwrap_or(20.0).clamp(1.0, 500.0);
    let (name, mesh) = match kind.as_str() {
        "cube" => ("Cube", cuboid(Vec3::ZERO, Vec3::splat(s))),
        "cylinder" => (
            "Cylinder",
            frustum(
                Vec3::new(0.0, 0.0, -s / 2.0),
                Vec3::new(0.0, 0.0, s / 2.0),
                s / 2.0,
                s / 2.0,
                96,
            ),
        ),
        "sphere" => ("Sphere", sphere(Vec3::ZERO, s / 2.0, 96, 48)),
        "cone" => (
            "Cone",
            frustum(
                Vec3::new(0.0, 0.0, -s / 2.0),
                Vec3::new(0.0, 0.0, s / 2.0),
                s / 2.0,
                0.0,
                96,
            ),
        ),
        other => return Err(format!("unknown shape {other}")),
    };
    checkpoint(&state);
    {
        let printer = state.current_printer();
        let mut scene = state.scene.lock().unwrap();
        scene.add(format!("{name} {s:.0} mm"), mesh);
        if scene.objects.len() > 1 {
            scene.arrange(&printer, 3.0);
        }
    }
    invalidate_slice(&state);
    Ok(all_infos(&state))
}

/// Renames an object.
#[tauri::command]
pub fn rename_object(state: State<'_, AppState>, id: ObjectId, name: String) -> CmdResult<ObjectInfo> {
    let mut scene = state.scene.lock().unwrap();
    let o = scene.get_mut(id).ok_or("object not found")?;
    o.name = name;
    Ok(info(o))
}
