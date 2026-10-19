//! Cleanup of the work directory (sliced files in the temp folder) on exit.

use openresin_core::Vec3;
use serde_json::json;
use tauri::{Manager, RunEvent};

use super::*;

/// Slices a small cube so the work directory holds a real, open slice file.
fn sliced_app() -> TestApp {
    let app = TestApp::new();
    app.use_printer("elegoo-mars-4-dlp", 0.1, 1);
    app.add_box("cube", Vec3::splat(5.0));
    let sum = app.slice(Some("goo"));
    assert!(app.work_dir.join(&sum.suggested_name).is_file());
    app
}

#[test]
fn cleanup_removes_the_work_dir_and_closes_the_slice() {
    let app = sliced_app();
    std::fs::write(app.work_dir.join("extra.bin"), b"leftover").unwrap();
    assert!(app.config_file().is_file());

    // the slice result keeps its file open; cleanup must release it first
    app.state().cleanup_work_dir();
    assert!(
        !app.work_dir.exists(),
        "work dir still there: {:?}",
        std::fs::read_dir(&app.work_dir).map(|d| d.count())
    );
    assert_eq!(
        app.fail("get_layer_image", json!({ "index": 0, "maxSize": 0 })),
        "nothing sliced yet"
    );
    // only the work dir goes: configuration and the user's files stay
    assert!(app.config_file().is_file());
    assert!(app.dir.join("cube.stl").is_file());
    // again: harmless
    app.state().cleanup_work_dir();

    // slicing after a cleanup recreates the directory
    let sum = app.slice(Some("goo"));
    assert_eq!(sum.layer_count, 50);
    assert!(app.work_dir.join(&sum.suggested_name).is_file());
}

#[test]
fn cleanup_ignores_files_it_cannot_delete() {
    let app = sliced_app();
    let busy = app.work_dir.join("busy.bin");
    std::fs::write(&busy, b"in use").unwrap();
    // held open by someone else: Windows refuses to delete it
    let handle = std::fs::File::open(&busy).unwrap();
    app.state().cleanup_work_dir();
    drop(handle);
    // a later attempt removes what is left
    app.state().cleanup_work_dir();
    assert!(!app.work_dir.exists());
}

#[test]
fn closing_the_window_removes_the_work_dir() {
    let app = sliced_app();
    let work_dir = app.work_dir.clone();
    let config = app.config_file();
    let (tauri_app, dir) = app.into_app();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    // the real event handler; the window is closed as soon as the loop runs
    tauri_app.run(move |handle, event| {
        if matches!(event, RunEvent::Ready) {
            handle.get_webview_window("main").unwrap().close().unwrap();
        }
        match &event {
            RunEvent::ExitRequested { .. } => seen2.lock().unwrap().push("exit requested"),
            RunEvent::Exit => seen2.lock().unwrap().push("exit"),
            _ => {}
        }
        crate::on_run_event(handle, event);
    });
    assert_eq!(*seen.lock().unwrap(), ["exit requested", "exit"]);
    assert!(!work_dir.exists(), "the work dir survived the exit");
    assert!(config.is_file(), "the configuration is kept");
    assert!(dir.path_exists());
}
