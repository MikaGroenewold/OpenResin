//! OpenResin desktop application (Tauri backend).

mod device_cmds;
mod edit_cmds;
mod geometry_cmds;
mod preset_cmds;
mod scene_cmds;
mod state;
#[cfg(test)]
mod tests;

use state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::new())
        .invoke_handler(invoke_handler())
        .build(tauri::generate_context!())
        .expect("error while building OpenResin")
        .run(on_run_event);
}

/// Deletes the work directory (sliced files in the temp folder) when the main
/// window is gone or the app exits.
fn on_run_event<R: tauri::Runtime>(app: &tauri::AppHandle<R>, event: tauri::RunEvent) {
    let quitting = match &event {
        tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit => true,
        tauri::RunEvent::WindowEvent {
            label,
            event: tauri::WindowEvent::Destroyed,
            ..
        } => label == "main",
        _ => false,
    };
    if quitting {
        use tauri::Manager;
        if let Some(state) = app.try_state::<AppState>() {
            state.cleanup_work_dir();
        }
    }
}

/// Every command the frontend can invoke (shared with the tests, which run
/// them on Tauri's mock runtime).
fn invoke_handler<R: tauri::Runtime>() -> impl Fn(tauri::ipc::Invoke<R>) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        scene_cmds::startup_files,
        scene_cmds::startup_flags,
        scene_cmds::list_printers,
        scene_cmds::get_config,
        scene_cmds::set_printer,
        scene_cmds::set_settings,
        scene_cmds::set_support_settings,
        scene_cmds::save_custom_printer,
        scene_cmds::set_theme,
        scene_cmds::import_models,
        scene_cmds::scene_objects,
        scene_cmds::get_mesh,
        scene_cmds::set_transform,
        scene_cmds::remove_objects,
        scene_cmds::duplicate_object,
        scene_cmds::arrange,
        scene_cmds::lay_flat,
        scene_cmds::auto_supports,
        scene_cmds::get_support_points,
        scene_cmds::set_support_points,
        scene_cmds::clear_supports,
        scene_cmds::get_support_mesh,
        scene_cmds::pick_surface,
        scene_cmds::save_project,
        scene_cmds::open_project,
        scene_cmds::new_project,
        scene_cmds::export_stl,
        scene_cmds::slice,
        scene_cmds::cancel_slice,
        scene_cmds::get_layer_image,
        scene_cmds::save_slice,
        scene_cmds::open_sliced_file,
        preset_cmds::list_presets,
        preset_cmds::save_preset,
        preset_cmds::delete_preset,
        geometry_cmds::analyze_object,
        geometry_cmds::repair_objects,
        geometry_cmds::split_object,
        geometry_cmds::add_label,
        geometry_cmds::add_platform,
        geometry_cmds::hollow_object,
        geometry_cmds::add_drain_hole,
        geometry_cmds::clear_cutouts,
        geometry_cmds::get_cutout_mesh,
        geometry_cmds::cut_object,
        edit_cmds::history_state,
        edit_cmds::undo,
        edit_cmds::redo,
        edit_cmds::fill_plate,
        edit_cmds::align_objects,
        edit_cmds::combine_objects,
        edit_cmds::auto_orient,
        edit_cmds::rename_object,
        edit_cmds::set_subtract,
        edit_cmds::add_primitive,
        device_cmds::discover_printers,
        device_cmds::saved_devices,
        device_cmds::save_device,
        device_cmds::forget_device,
        device_cmds::connect_printer,
        device_cmds::disconnect_printer,
        device_cmds::printer_files,
        device_cmds::printer_delete,
        device_cmds::printer_history,
        device_cmds::printer_command,
        device_cmds::upload_to_printer,
        device_cmds::cancel_upload,
        device_cmds::printer_download,
        device_cmds::cancel_download,
        device_cmds::printer_thumbnail,
        device_cmds::printer_set_time_lapse,
        device_cmds::printer_rename,
        device_cmds::camera_start,
        device_cmds::camera_stop,
        device_cmds::camera_snapshot,
    ]
}
