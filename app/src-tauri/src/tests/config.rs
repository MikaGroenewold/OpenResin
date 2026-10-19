//! Printer selection, settings and presets, and their persistence across
//! application restarts (a fresh `AppState` loading the same config file).

use openresin_core::profile::{PrintSettings, PrinterProfile};
use openresin_core::support::SupportSettings;
use openresin_formats::Format;
use serde::Deserialize;
use serde_json::{json, Value};

use super::*;

#[derive(Debug, Deserialize)]
struct Preset {
    settings: PrintSettings,
    builtin: bool,
}

fn settings(name: &str, layer_height: f32, exposure: f32) -> PrintSettings {
    let mut s = PrintSettings {
        name: name.into(),
        layer_height,
        ..Default::default()
    };
    s.normal.exposure = exposure;
    s
}

fn current_settings(app: &TestApp) -> PrintSettings {
    let cfg: Value = app.call("get_config", json!({}));
    serde_json::from_value(cfg["settings"].clone()).unwrap()
}

#[test]
fn default_config_and_printer_list() {
    let app = TestApp::new();
    let cfg: Value = app.call("get_config", json!({}));
    assert_eq!(cfg["printer"]["id"], "generic-4k");
    assert_eq!(cfg["theme"], "dark");
    assert_eq!(cfg["devices"], json!([]));
    let s: PrintSettings = serde_json::from_value(cfg["settings"].clone()).unwrap();
    assert_eq!(s, PrintSettings::default());
    let ss: SupportSettings = serde_json::from_value(cfg["support_settings"].clone()).unwrap();
    assert_eq!(ss, SupportSettings::default());
    let formats = cfg["formats"].as_array().unwrap();
    assert_eq!(formats.len(), Format::ALL.len());
    for (f, v) in Format::ALL.iter().zip(formats) {
        assert_eq!(v["extension"], f.extension());
        assert_eq!(v["id"], f.extension());
        assert!(!v["description"].as_str().unwrap().is_empty());
    }
    let printers: Vec<PrinterProfile> = app.call("list_printers", json!({}));
    assert_eq!(printers, openresin_core::profile::builtin_printers());
    // nothing is written until something changes
    assert!(!app.config_file().exists());

    // startup arguments of the test process are not model files
    let files: Vec<String> = app.call("startup_files", json!({}));
    assert!(files.iter().all(|f| std::path::Path::new(f).is_file()));
    let flags: Vec<String> = app.call("startup_flags", json!({}));
    assert!(flags.iter().all(|f| f.starts_with("--")));
}

#[test]
fn printer_settings_and_theme_persist_across_restarts() {
    let app = TestApp::new();
    let cfg: Value = app.call("set_printer", json!({ "id": "elegoo-saturn-4-ultra" }));
    assert_eq!(cfg["printer"]["id"], "elegoo-saturn-4-ultra");
    assert_eq!(cfg["printer"]["resolution_x"], 11520);
    assert!(app.config_file().is_file(), "config is saved in the test directory");

    let saturn = settings("Saturn resin", 0.03, 2.2);
    app.run("set_settings", json!({ "settings": saturn }));
    assert_eq!(current_settings(&app), saturn);
    let sup = SupportSettings {
        lift_height: 7.5,
        spacing: 2.0,
        raft: false,
        ..Default::default()
    };
    app.run("set_support_settings", json!({ "settings": sup }));
    app.run("set_theme", json!({ "theme": "light" }));

    // settings are kept per printer
    app.run("set_printer", json!({ "id": "elegoo-mars" }));
    assert_eq!(current_settings(&app), PrintSettings::default());
    let mars = settings("Mars resin", 0.05, 3.1);
    app.run("set_settings", json!({ "settings": mars }));
    app.run("set_printer", json!({ "id": "elegoo-saturn-4-ultra" }));
    assert_eq!(current_settings(&app), saturn);

    // a restarted app sees everything
    let again = app.reopen();
    let cfg: Value = again.call("get_config", json!({}));
    assert_eq!(cfg["printer"]["id"], "elegoo-saturn-4-ultra");
    assert_eq!(cfg["theme"], "light");
    assert_eq!(
        serde_json::from_value::<PrintSettings>(cfg["settings"].clone()).unwrap(),
        saturn
    );
    assert_eq!(
        serde_json::from_value::<SupportSettings>(cfg["support_settings"].clone()).unwrap(),
        sup
    );
    again.run("set_printer", json!({ "id": "elegoo-mars" }));
    assert_eq!(current_settings(&again), mars);
    // the scene itself is not part of the configuration
    assert!(again.objects().is_empty());
}

#[test]
fn invalid_printer_and_settings_are_rejected() {
    let app = TestApp::new();
    let e = app.fail("set_printer", json!({ "id": "no-such-printer" }));
    assert!(e.contains("unknown printer"), "{e}");

    let mut bad = settings("bad", 0.0, 2.0);
    let e = app.fail("set_settings", json!({ "settings": bad }));
    assert!(e.contains("layer height"), "{e}");
    bad.layer_height = 0.05;
    bad.anti_aliasing = 3;
    let e = app.fail("set_settings", json!({ "settings": bad }));
    assert!(e.contains("anti-aliasing"), "{e}");
    bad.anti_aliasing = 4;
    bad.normal.exposure = -1.0;
    let e = app.fail("set_settings", json!({ "settings": bad }));
    assert!(e.contains("exposure"), "{e}");
    let e = app.fail("set_settings", json!({ "settings": { "name": "incomplete" } }));
    assert!(e.contains("settings"), "{e}");

    // nothing was stored
    let cfg: Value = app.call("get_config", json!({}));
    assert_eq!(cfg["printer"]["id"], "generic-4k");
    assert_eq!(current_settings(&app), PrintSettings::default());
    assert!(!app.config_file().exists());
}

#[test]
fn custom_printers_are_saved_and_selected() {
    let app = TestApp::new();
    let mut p = PrinterProfile::generic();
    p.id = "my-printer".into();
    p.vendor = "Me".into();
    p.name = "Garage DLP".into();
    p.resolution_x = 1000;
    p.resolution_y = 500;
    p.size_x = 100.0;
    p.size_y = 50.0;
    p.formats = vec!["goo".into()];
    let cfg: Value = app.call("save_custom_printer", json!({ "printer": p }));
    assert_eq!(cfg["printer"]["id"], "my-printer");
    let list: Vec<PrinterProfile> = app.call("list_printers", json!({}));
    assert_eq!(list.last().unwrap(), &p);

    // slicing uses the custom resolution
    app.run("set_settings", json!({ "settings": settings("x", 0.1, 2.0) }));
    app.add_box("cube", openresin_core::Vec3::splat(10.0));
    let sum = app.slice(None);
    assert_eq!(sum.extension, "goo");
    assert_eq!((sum.info.resolution_x, sum.info.resolution_y), (1000, 500));

    // saving again with the same id replaces it
    p.name = "Garage DLP v2".into();
    app.run("save_custom_printer", json!({ "printer": p }));
    let list: Vec<PrinterProfile> = app.call("list_printers", json!({}));
    assert_eq!(list.iter().filter(|x| x.id == "my-printer").count(), 1);
    assert_eq!(list.last().unwrap().name, "Garage DLP v2");

    let again = app.reopen();
    let cfg: Value = again.call("get_config", json!({}));
    assert_eq!(cfg["printer"]["name"], "Garage DLP v2");

    let mut broken = p.clone();
    broken.id = "broken".into();
    broken.resolution_x = 0;
    let e = app.fail("save_custom_printer", json!({ "printer": broken }));
    assert!(e.contains("positive"), "{e}");
    broken.resolution_x = 100;
    broken.size_y = 0.0;
    assert!(app
        .fail("save_custom_printer", json!({ "printer": broken }))
        .contains("positive"));
    let list: Vec<PrinterProfile> = app.call("list_printers", json!({}));
    assert!(list.iter().all(|x| x.id != "broken"));
}

#[test]
fn presets_list_save_delete_and_persist() {
    let app = TestApp::new();
    let list: Vec<Preset> = app.call("list_presets", json!({}));
    assert_eq!(list.len(), 6);
    assert!(list.iter().all(|p| p.builtin));
    assert_eq!(list[0].settings, PrintSettings::default());
    for p in &list {
        p.settings
            .validate()
            .unwrap_or_else(|e| panic!("builtin preset {}: {e}", p.settings.name));
    }
    let names: std::collections::HashSet<_> = list.iter().map(|p| p.settings.name.clone()).collect();
    assert_eq!(names.len(), 6, "unique names");

    // a user preset is added and becomes the active settings
    let mine = settings("My resin", 0.04, 2.7);
    app.run("save_preset", json!({ "settings": mine }));
    let list: Vec<Preset> = app.call("list_presets", json!({}));
    assert_eq!(list.len(), 7);
    let user: Vec<&Preset> = list.iter().filter(|p| !p.builtin).collect();
    assert_eq!(user.len(), 1);
    assert_eq!(user[0].settings, mine);
    assert_eq!(current_settings(&app), mine);

    // a user preset with a built-in name replaces the built-in one
    let abs = settings("ABS-like resin", 0.05, 3.3);
    app.run("save_preset", json!({ "settings": abs }));
    let list: Vec<Preset> = app.call("list_presets", json!({}));
    assert_eq!(list.len(), 7);
    let abs_entries: Vec<&Preset> = list.iter().filter(|p| p.settings.name == "ABS-like resin").collect();
    assert_eq!(abs_entries.len(), 1);
    assert!(!abs_entries[0].builtin);
    assert_eq!(abs_entries[0].settings, abs);
    // saving again under the same name updates it
    let mine2 = settings("My resin", 0.04, 2.9);
    app.run("save_preset", json!({ "settings": mine2 }));
    let list: Vec<Preset> = app.call("list_presets", json!({}));
    assert_eq!(list.iter().filter(|p| p.settings.name == "My resin").count(), 1);
    assert!(list.iter().any(|p| p.settings == mine2));

    // presets belong to the printer they were made for
    app.run("set_printer", json!({ "id": "elegoo-mars" }));
    let other: Vec<Preset> = app.call("list_presets", json!({}));
    assert!(other.iter().all(|p| p.builtin) && other.len() == 6);
    app.run("set_printer", json!({ "id": "generic-4k" }));

    // persisted across restarts
    let again = app.reopen();
    let list: Vec<Preset> = again.call("list_presets", json!({}));
    assert_eq!(list.iter().filter(|p| !p.builtin).count(), 2);

    // deleting the user version brings the built-in back
    again.run("delete_preset", json!({ "name": "ABS-like resin" }));
    again.run("delete_preset", json!({ "name": "does not exist" }));
    let list: Vec<Preset> = again.call("list_presets", json!({}));
    let abs_entries: Vec<&Preset> = list.iter().filter(|p| p.settings.name == "ABS-like resin").collect();
    assert_eq!(abs_entries.len(), 1);
    assert!(abs_entries[0].builtin);
    assert!(abs_entries[0].settings.normal.exposure == 3.0);
    let third = again.reopen();
    let list: Vec<Preset> = third.call("list_presets", json!({}));
    assert_eq!(list.iter().filter(|p| !p.builtin).count(), 1);

    // validation
    let e = app.fail("save_preset", json!({ "settings": settings("  ", 0.05, 2.0) }));
    assert!(e.contains("name"), "{e}");
    let e = app.fail("save_preset", json!({ "settings": settings("Too thick", 0.9, 2.0) }));
    assert!(e.contains("layer height"), "{e}");
}
