//! Slicing, layer previews, saving and opening sliced files, cancelling.

use std::time::{Duration, Instant};

use openresin_core::support::SupportSettings;
use openresin_core::{Quat, Vec3};
use openresin_formats::Format;
use serde_json::json;

use super::*;

const PRINTER: &str = "elegoo-mars-4-dlp";

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[test]
fn slice_preview_and_save_goo_and_ctb() {
    let app = TestApp::new();
    app.use_printer(PRINTER, 0.1, 1);
    let cube = app.add_box("Test cube", Vec3::splat(10.0));

    let sum = app.slice(Some("goo"));
    assert_eq!(sum.format, Format::Goo);
    assert_eq!(sum.extension, "goo");
    assert_eq!(sum.layer_count, 100);
    assert_eq!(sum.layer_areas.len(), 100);
    assert_near(sum.height_mm as f32, 10.0, 1e-3, "height");
    assert_rel(sum.volume_ml, 1.0, 0.03, "1 cm³ of resin");
    assert_rel(sum.weight_g, 1.1, 0.03, "default density 1.1 g/ml");
    assert!(sum.print_time_s > 0.0 && sum.cost > 0.0 && sum.file_size > 0);
    assert_eq!(sum.currency, "€");
    assert_eq!(sum.suggested_name, "Test_cube_0.100_2.5.goo");
    assert!(sum.warnings.is_empty(), "{:?}", sum.warnings);
    assert_eq!((sum.info.resolution_x, sum.info.resolution_y), (2560, 1440));
    assert_eq!(sum.info.layer_count, 100);
    assert_near(sum.info.layer_height, 0.1, 1e-5, "layer height in the file");
    for a in &sum.layer_areas {
        assert_rel(*a, 100.0, 0.03, "layer area in mm²");
    }
    // the file was written to the work directory, not next to the user's files
    let tmp = app.work_dir.join(&sum.suggested_name);
    assert!(tmp.is_file());

    // full resolution and downscaled layer images
    let (w, h, lit) = png_lit(&app.call_raw("get_layer_image", json!({ "index": 50, "maxSize": 0 })));
    assert_eq!((w, h), (2560, 1440));
    assert_rel(lit as f64, area_px(PRINTER, 100.0), 0.03, "lit pixels");
    let (w, h, lit_small) = png_lit(&app.call_raw("get_layer_image", json!({ "index": 50, "maxSize": 320 })));
    assert_eq!((w, h), (320, 180));
    assert_rel(lit_small as f64, lit as f64 / 64.0, 0.1, "downscaled lit pixels");
    let e = app.fail("get_layer_image", json!({ "index": 100, "maxSize": 0 }));
    assert!(e.contains("100") || e.contains("range"), "{e}");

    // save and read back with the format readers
    let dest = app.dir.join("saved.goo");
    app.run("save_slice", json!({ "path": s(&dest) }));
    let f = openresin_formats::open(&dest).unwrap();
    assert_eq!(f.info().layer_count, 100);
    assert_eq!(f.info().resolution_x, 2560);
    assert_rel(
        f.layer(10).unwrap().lit_area_px(),
        area_px(PRINTER, 100.0),
        0.03,
        "saved layer",
    );
    assert!(f.thumbnail().is_some(), "a preview is rendered when the UI sends none");

    // same plate as CTB
    let sum = app.slice(Some("ctb"));
    assert_eq!(sum.extension, "ctb");
    assert_eq!(sum.layer_count, 100);
    let dest = app.dir.join("saved.ctb");
    app.run("save_slice", json!({ "path": s(&dest) }));
    let f = openresin_formats::open(&dest).unwrap();
    assert_eq!(f.info().layer_count, 100);
    assert_near(f.info().exposure, 2.5, 1e-4, "exposure");
    assert_rel(
        f.layer(99).unwrap().lit_area_px(),
        area_px(PRINTER, 100.0),
        0.03,
        "ctb layer",
    );

    // without a format the printer's preferred one (goo) is used
    let sum = app.slice(None);
    assert_eq!(sum.format, Format::Goo);
    // an Anycubic format by its extension name
    let sum = app.slice(Some("pwmx"));
    assert_eq!(sum.extension, "pwmx");
    assert_eq!(sum.info.layer_count, 100);
    assert!(app.try_call::<Summary>("slice", json!({ "format": "bmp" })).is_err());

    // changing the plate invalidates the preview
    app.move_by(cube.id, Vec3::new(1.0, 0.0, 0.0), true);
    assert_eq!(
        app.fail("get_layer_image", json!({ "index": 0, "maxSize": 0 })),
        "nothing sliced yet"
    );
    assert_eq!(
        app.fail("save_slice", json!({ "path": s(&app.dir.join("x.goo")) })),
        "nothing sliced yet"
    );
    // so do printer and settings changes
    app.slice(None);
    app.run("set_printer", json!({ "id": PRINTER }));
    assert_eq!(
        app.fail("get_layer_image", json!({ "index": 0, "maxSize": 0 })),
        "nothing sliced yet"
    );
}

#[test]
fn slice_uses_the_thumbnail_from_the_ui() {
    let app = TestApp::new();
    app.use_printer(PRINTER, 0.1, 1);
    app.add_box("cube", Vec3::splat(5.0));
    let red = image::RgbaImage::from_pixel(64, 64, image::Rgba([255, 0, 0, 255]));
    let mut png = std::io::Cursor::new(Vec::new());
    red.write_to(&mut png, image::ImageFormat::Png).unwrap();
    let url = format!("data:image/png;base64,{}", base64(png.get_ref()));
    let sum: Summary = app.call("slice", json!({ "format": "goo", "thumbnail": url }));
    let dest = app.dir.join("thumb.goo");
    app.run("save_slice", json!({ "path": s(&dest) }));
    let t = openresin_formats::open(&dest).unwrap().thumbnail().expect("thumbnail");
    let n = (t.width * t.height) as usize;
    let (mut r, mut g, mut b) = (0u64, 0u64, 0u64);
    for p in t.rgba.chunks_exact(4).take(n) {
        r += p[0] as u64;
        g += p[1] as u64;
        b += p[2] as u64;
    }
    let n = n as u64;
    assert!(
        r / n > 200 && g / n < 40 && b / n < 40,
        "thumbnail is not the red UI image: {:?}",
        (r / n, g / n, b / n)
    );
    assert_eq!(sum.layer_count, 50);

    // garbage thumbnails fall back to the rendered preview
    let sum: Summary = app.call(
        "slice",
        json!({ "format": "goo", "thumbnail": "data:image/png;base64,!!!" }),
    );
    assert_eq!(sum.layer_count, 50);
}

#[test]
fn slice_problems_are_reported() {
    let app = TestApp::new();
    app.use_printer(PRINTER, 0.1, 1);
    assert_eq!(app.fail("slice", json!({})), "the build plate is empty");

    let o = app.add_box("big", Vec3::new(150.0, 20.0, 10.0));
    let e = app.fail("slice", json!({}));
    assert!(e.contains("outside the printable area"), "{e}");
    app.run("remove_objects", json!({ "ids": [o.id] }));

    // floating without supports: sliced, with a warning
    let f = app.add_box("floater", Vec3::splat(5.0));
    app.move_by(f.id, Vec3::new(0.0, 0.0, 2.0), false);
    let sum = app.slice(None);
    assert_eq!(sum.warnings.len(), 1);
    assert!(sum.warnings[0].contains("floater"), "{:?}", sum.warnings);
    assert_eq!(sum.layer_count, 70);
    assert_eq!(app.layer_lit(10), 0, "nothing below the floating model");

    // only a cutter on the plate: nothing to print
    app.run("set_subtract", json!({ "ids": [f.id], "subtract": true }));
    let e = app.fail("slice", json!({}));
    assert!(e.contains("nothing to slice"), "{e}");
}

#[test]
fn supports_and_subtracted_volumes_show_up_in_the_layers() {
    let app = TestApp::new();
    app.use_printer(PRINTER, 0.1, 1);
    let big: Vec<ObjectInfo> = app.call("add_primitive", json!({ "kind": "cube", "size": 20.0 }));
    let big = big[0].clone();
    let small: Vec<ObjectInfo> = app.call("add_primitive", json!({ "kind": "cube", "size": 10.0 }));
    let small = small.last().unwrap().clone();
    // put the small cube into the big one and make it a cutter
    let big = app.object(big.id);
    let c = big.bounds.center();
    let mut t = app.object(small.id).transform;
    t.translation.x = c.x;
    t.translation.y = c.y;
    app.set_transform(small.id, t, true);
    app.run("set_subtract", json!({ "ids": [small.id], "subtract": true }));
    app.slice(None);
    assert_rel(
        app.layer_lit(50) as f64,
        area_px(PRINTER, 400.0 - 100.0),
        0.03,
        "pocket",
    );
    assert_rel(
        app.layer_lit(150) as f64,
        area_px(PRINTER, 400.0),
        0.02,
        "above the pocket",
    );
    app.run("set_subtract", json!({ "ids": [small.id], "subtract": false }));
    app.slice(None);
    assert_rel(
        app.layer_lit(50) as f64,
        area_px(PRINTER, 400.0),
        0.02,
        "union of solids",
    );
    app.run("remove_objects", json!({ "ids": [small.id] }));

    // supports: 5 mm of extra layers with raft and pillars below the model
    let r: Vec<ObjectInfo> = app.call(
        "auto_supports",
        json!({ "ids": [big.id], "settings": SupportSettings::default() }),
    );
    assert!(r[0].support_points > 0);
    let sum = app.slice(None);
    assert_eq!(sum.layer_count, 250);
    assert!(
        sum.warnings.is_empty(),
        "supported model is not floating: {:?}",
        sum.warnings
    );
    let raft = app.layer_lit(3);
    let pillars = app.layer_lit(35);
    assert!(raft > 0 && pillars > 0);
    assert!((pillars as f64) < area_px(PRINTER, 400.0) * 0.5, "pillars {pillars}");
    assert!(raft > pillars, "raft {raft} vs pillars {pillars}");
    assert_rel(
        app.layer_lit(150) as f64,
        area_px(PRINTER, 400.0),
        0.02,
        "model above the supports",
    );
    let total: f64 = sum.layer_areas.iter().sum::<f64>() * 0.1 / 1000.0;
    assert_rel(sum.volume_ml, total, 0.01, "volume matches the layer areas");
    assert!(sum.volume_ml > 8.0, "supports add resin");
}

#[test]
fn cancel_slice_stops_a_running_job() {
    let app = TestApp::new();
    app.use_printer("generic-4k", 0.01, 1);
    app.add_box("tall", Vec3::new(80.0, 60.0, 150.0)); // 15000 layers
    let webview = app.webview();
    let cancelled = std::thread::scope(|scope| {
        let job = scope.spawn(move || invoke_on(&webview, "slice", json!({ "format": "goo" })).map(|_| ()));
        let started = Instant::now();
        while !job.is_finished() {
            app.run("cancel_slice", json!({}));
            std::thread::sleep(Duration::from_millis(20));
            assert!(
                started.elapsed() < Duration::from_secs(120),
                "cancel did not stop the job"
            );
        }
        job.join().unwrap()
    });
    assert_eq!(cancelled.unwrap_err(), "cancelled");
    // no output (or partial output) is left behind
    let left: Vec<_> = std::fs::read_dir(&app.work_dir).unwrap().collect();
    assert!(left.is_empty(), "{left:?}");
    assert_eq!(
        app.fail("get_layer_image", json!({ "index": 0, "maxSize": 0 })),
        "nothing sliced yet"
    );

    // the next slice is not affected by the old cancel request
    app.use_printer(PRINTER, 0.1, 1);
    app.run("new_project", json!({}));
    app.add_box("small", Vec3::splat(3.0));
    let sum = app.slice(None);
    assert_eq!(sum.layer_count, 30);
}

#[test]
fn open_sliced_files_from_disk() {
    let app = TestApp::new();
    app.use_printer(PRINTER, 0.05, 1);
    let o = app.add_box("part", Vec3::new(10.0, 10.0, 4.0));
    let mut t = o.transform;
    t.rotation = Quat::from_rotation_z(0.6);
    app.set_transform(o.id, t, true);
    let sliced = app.slice(Some("goo"));
    let path = app.dir.join("part.goo");
    app.run("save_slice", json!({ "path": s(&path) }));
    let lit = app.layer_lit(40);

    // a different app instance opens the file for preview and upload
    let other = TestApp::new();
    let sum: Summary = other.call("open_sliced_file", json!({ "path": s(&path) }));
    assert_eq!(sum.format, Format::Goo);
    assert_eq!(sum.layer_count, sliced.layer_count);
    assert_eq!(sum.suggested_name, "part.goo");
    assert_near(sum.height_mm as f32, 4.0, 1e-3, "height from the file");
    assert_eq!(sum.file_size, std::fs::metadata(&path).unwrap().len());
    assert_eq!(sum.duration_ms, 0, "not sliced here");
    assert_eq!(sum.info.resolution_x, 2560);
    assert_eq!(other.layer_lit(40), lit, "same layer image as when it was sliced");
    // saving it again copies it
    let copy = other.dir.join("copy.goo");
    other.run("save_slice", json!({ "path": s(&copy) }));
    assert_eq!(std::fs::read(&copy).unwrap(), std::fs::read(&path).unwrap());

    assert!(!other
        .fail("open_sliced_file", json!({ "path": s(&other.dir.join("missing.goo")) }))
        .is_empty());
    let stl = other.dir.join("model.stl");
    write_stl(&stl, &openresin_core::primitives::cuboid(Vec3::ZERO, Vec3::ONE));
    let e = other.fail("open_sliced_file", json!({ "path": s(&stl) }));
    assert!(e.contains("stl"), "{e}");
    let junk = other.dir.join("junk.ctb");
    std::fs::write(&junk, b"definitely not a ctb file").unwrap();
    other.fail("open_sliced_file", json!({ "path": s(&junk) }));
    // the previously opened file is still shown
    assert_eq!(other.layer_lit(40), lit);
}
