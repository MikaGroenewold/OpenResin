//! Print setting presets ("resin profiles").

use openresin_core::profile::{Motion, PrintSettings};
use serde::Serialize;
use tauri::State;

use crate::scene_cmds::invalidate_slice;
use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

#[derive(Serialize)]
pub struct Preset {
    pub settings: PrintSettings,
    pub builtin: bool,
}

/// Generic starting points; users should calibrate with an exposure test.
fn builtin() -> Vec<PrintSettings> {
    let standard = PrintSettings::default();
    let mut abs_like = standard.clone();
    abs_like.name = "ABS-like resin".into();
    abs_like.normal.exposure = 3.0;
    abs_like.bottom.exposure = 35.0;
    let mut washable = standard.clone();
    washable.name = "Water washable resin".into();
    washable.normal.exposure = 2.8;
    washable.bottom.exposure = 30.0;
    washable.resin_density = 1.1;
    let mut tough = standard.clone();
    tough.name = "Tough / engineering resin".into();
    tough.normal.exposure = 3.5;
    tough.bottom.exposure = 40.0;
    tough.normal.lift = Motion::new(7.0, 55.0);
    let mut fast = standard.clone();
    fast.name = "Fast draft (0.1 mm)".into();
    fast.layer_height = 0.1;
    fast.normal.exposure = 4.0;
    fast.anti_aliasing = 2;
    let mut detail = standard.clone();
    detail.name = "High detail (0.03 mm)".into();
    detail.layer_height = 0.03;
    detail.normal.exposure = 2.0;
    detail.anti_aliasing = 8;
    vec![standard, abs_like, washable, tough, fast, detail]
}

#[tauri::command]
pub fn list_presets(state: State<'_, AppState>) -> Vec<Preset> {
    let printer = state.current_printer();
    let cfg = state.config.lock().unwrap();
    let user = cfg.presets.get(&printer.id).cloned().unwrap_or_default();
    let mut out: Vec<Preset> = builtin()
        .into_iter()
        .filter(|b| !user.iter().any(|u| u.name == b.name))
        .map(|settings| Preset {
            settings,
            builtin: true,
        })
        .collect();
    out.extend(user.into_iter().map(|settings| Preset {
        settings,
        builtin: false,
    }));
    out
}

#[tauri::command]
pub fn save_preset(state: State<'_, AppState>, settings: PrintSettings) -> CmdResult<()> {
    settings.validate()?;
    if settings.name.trim().is_empty() {
        return Err("the preset needs a name".into());
    }
    let printer = state.current_printer();
    let mut cfg = state.config.lock().unwrap();
    let list = cfg.presets.entry(printer.id.clone()).or_default();
    list.retain(|p| p.name != settings.name);
    list.push(settings.clone());
    cfg.settings.insert(printer.id, settings);
    cfg.save();
    drop(cfg);
    invalidate_slice(&state);
    Ok(())
}

#[tauri::command]
pub fn delete_preset(state: State<'_, AppState>, name: String) {
    let printer = state.current_printer();
    let mut cfg = state.config.lock().unwrap();
    if let Some(list) = cfg.presets.get_mut(&printer.id) {
        list.retain(|p| p.name != name);
    }
    cfg.save();
}
