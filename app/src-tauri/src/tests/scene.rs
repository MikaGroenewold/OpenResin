//! Import, transforms, copies, arrangement, undo/redo, supports, projects, export.

use std::f32::consts::FRAC_PI_2;

use openresin_core::primitives::cuboid;
use openresin_core::support::{SupportPoint, SupportSettings};
use openresin_core::{Mesh, Quat, Vec3};
use serde_json::json;

use super::*;

#[test]
fn import_stl_obj_and_3mf_report_real_geometry() {
    let app = TestApp::new();
    let stl = app.dir.join("cube.stl");
    write_stl(&stl, &cuboid(Vec3::new(50.0, -20.0, 30.0), Vec3::splat(10.0)));
    let cube = app.import_one(&stl);
    assert_eq!(cube.name, "cube");
    assert_eq!(cube.triangles, 12);
    assert_rel(cube.volume_mm3, 1000.0, 1e-4, "cube volume");
    assert_vec_near(size(&cube.bounds), Vec3::splat(10.0), 1e-4, "cube size");
    // a single model is centred on the plate and dropped onto it
    assert_vec_near(cube.bounds.min, Vec3::new(-5.0, -5.0, 0.0), 1e-4, "cube placement");
    assert_eq!((cube.support_points, cube.cutouts, cube.subtract), (0, 0, false));

    // the mesh is stored centred on its bounding box
    let mesh = app.mesh("get_mesh", cube.id);
    assert_eq!(mesh.triangle_count(), 12);
    assert_vec_near(mesh.bounds().min, Vec3::splat(-5.0), 1e-4, "local mesh");

    let obj = app.dir.join("quads.obj");
    cube_obj(&obj);
    let o = app.import_one(&obj);
    assert_eq!(o.name, "quads");
    assert_eq!(o.triangles, 12, "quads are triangulated");
    assert_rel(o.volume_mm3, 1000.0, 1e-4, "obj volume");

    let tmf = app.dir.join("parts.3mf");
    two_objects_3mf(&tmf);
    let parts = app.import(&[&tmf]);
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0].name, "Cube");
    assert_eq!(parts[1].name, "Block");
    assert_vec_near(size(&parts[1].bounds), Vec3::new(20.0, 10.0, 5.0), 1e-4, "3mf block");
    assert_rel(parts[1].volume_mm3, 1000.0, 1e-4, "3mf block volume");

    // several models are arranged without overlapping
    let all = app.objects();
    assert_eq!(all.len(), 4);
    assert_laid_out(&all, 192.0, 120.0, 2.9);
    let ids: std::collections::HashSet<_> = all.iter().map(|o| o.id).collect();
    assert_eq!(ids.len(), 4, "ids are unique");
}

#[test]
fn import_errors_leave_the_scene_alone() {
    let app = TestApp::new();
    let cube = app.add_box("cube", Vec3::splat(10.0));
    let before = app.objects();

    let e = app.fail("import_models", json!({ "paths": [s(&app.dir.join("missing.stl"))] }));
    assert!(!e.is_empty());
    let junk = app.dir.join("junk.stl");
    std::fs::write(&junk, b"solid nothing\nendsolid nothing\n").unwrap();
    let e = app.fail("import_models", json!({ "paths": [s(&junk)] }));
    assert!(e.contains("junk.stl") || e.contains("triangles"), "{e}");
    let txt = app.dir.join("model.txt");
    std::fs::write(&txt, "hello").unwrap();
    app.fail("import_models", json!({ "paths": [s(&txt)] }));
    // one bad file fails the whole import (no partial results)
    let good = app.dir.join("cube.stl");
    app.fail("import_models", json!({ "paths": [s(&good), s(&txt)] }));

    assert_eq!(app.objects(), before);
    assert_eq!(app.objects()[0].id, cube.id);
    // a missing argument is reported, not ignored
    let e = app.fail("import_models", json!({}));
    assert!(e.contains("paths"), "{e}");
}

#[test]
fn transforms_change_the_bounds() {
    let app = TestApp::new();
    let o = app.add_box("box", Vec3::new(10.0, 20.0, 30.0));
    let id = o.id;

    // move
    let moved = app.move_by(id, Vec3::new(20.0, -10.0, 0.0), true);
    assert_vec_near(
        moved.bounds.min,
        o.bounds.min + Vec3::new(20.0, -10.0, 0.0),
        1e-4,
        "moved",
    );
    assert_vec_near(size(&moved.bounds), size(&o.bounds), 1e-4, "move keeps size");

    // lift without keeping it on the plate
    let lifted = app.move_by(id, Vec3::new(0.0, 0.0, 5.0), false);
    assert_near(lifted.bounds.min.z, 5.0, 1e-4, "lifted");
    // keep_on_plate drops it back
    let t = lifted.transform;
    let dropped = app.set_transform(id, t, true);
    assert_near(dropped.bounds.min.z, 0.0, 1e-4, "dropped");

    // rotate 90 degrees about X: depth and height swap
    let mut t = dropped.transform;
    t.rotation = Quat::from_rotation_x(FRAC_PI_2);
    let r = app.set_transform(id, t, true);
    assert_vec_near(size(&r.bounds), Vec3::new(10.0, 30.0, 20.0), 1e-3, "rotated");
    assert_near(r.bounds.min.z, 0.0, 1e-4, "rotated on plate");
    assert_rel(r.volume_mm3, 6000.0, 1e-4, "rotation keeps volume");
    // non-normalised quaternions are normalised
    t.rotation = Quat::from_xyzw(0.0, 0.0, 0.0, 2.0);
    let n = app.set_transform(id, t, true);
    assert_near(n.transform.rotation.length(), 1.0, 1e-5, "normalised");
    assert_vec_near(size(&n.bounds), Vec3::new(10.0, 20.0, 30.0), 1e-3, "identity again");

    // uniform and non-uniform scale
    t.rotation = Quat::IDENTITY;
    t.scale = Vec3::splat(2.0);
    let sc = app.set_transform(id, t, true);
    assert_vec_near(size(&sc.bounds), Vec3::new(20.0, 40.0, 60.0), 1e-3, "scaled");
    assert_rel(sc.volume_mm3, 48000.0, 1e-4, "scaled volume");
    t.scale = Vec3::new(0.5, 1.0, 2.0);
    let sc = app.set_transform(id, t, true);
    assert_vec_near(size(&sc.bounds), Vec3::new(5.0, 20.0, 60.0), 1e-3, "non-uniform");
    assert_rel(sc.volume_mm3, 6000.0, 1e-4, "non-uniform volume");
    // mirroring keeps a positive volume
    t.scale = Vec3::new(-1.0, 1.0, 1.0);
    let m = app.set_transform(id, t, true);
    assert_vec_near(size(&m.bounds), Vec3::new(10.0, 20.0, 30.0), 1e-3, "mirrored");
    assert!(m.volume_mm3 > 0.0);

    let e = app.fail(
        "set_transform",
        json!({ "id": 999, "transform": t, "keepOnPlate": true }),
    );
    assert!(e.contains("not found"), "{e}");
    let e = app.fail("set_transform", json!({ "id": id, "transform": t }));
    assert!(e.contains("keepOnPlate"), "{e}");
}

#[test]
fn duplicate_remove_and_arrange() {
    let app = TestApp::new();
    let a = app.add_box("part", Vec3::new(30.0, 20.0, 10.0));
    let copies: Vec<ObjectInfo> = app.call("duplicate_object", json!({ "id": a.id, "count": 3 }));
    assert_eq!(copies.len(), 3);
    for c in &copies {
        assert_eq!(c.name, "part copy");
        assert_eq!(c.triangles, a.triangles);
        assert_rel(c.volume_mm3, a.volume_mm3, 1e-6, "copy volume");
        assert_ne!(c.id, a.id);
    }
    let all = app.objects();
    assert_eq!(all.len(), 4);
    assert_laid_out(&all, 192.0, 120.0, 2.9);

    // count is clamped to at least one copy
    let one: Vec<ObjectInfo> = app.call("duplicate_object", json!({ "id": a.id, "count": 0 }));
    assert_eq!(one.len(), 1);
    assert!(app
        .fail("duplicate_object", json!({ "id": 999, "count": 1 }))
        .contains("not found"));

    app.run("remove_objects", json!({ "ids": [copies[0].id, copies[1].id, 999] }));
    let left: Vec<u64> = app.objects().iter().map(|o| o.id).collect();
    assert_eq!(left, vec![a.id, copies[2].id, one[0].id]);

    // squeeze them together, arrange spreads them out again
    for id in &left {
        let o = app.object(*id);
        let mut t = o.transform;
        t.translation.x = 0.0;
        t.translation.y = 0.0;
        app.set_transform(*id, t, true);
    }
    let (fits, objs): (bool, Vec<ObjectInfo>) = app.call("arrange", json!({}));
    assert!(fits);
    assert_laid_out(&objs, 192.0, 120.0, 2.9);

    // too much for the plate
    let big = app.add_box("big", Vec3::new(100.0, 100.0, 10.0));
    app.run("duplicate_object", json!({ "id": big.id, "count": 1 }));
    let (fits, _): (bool, Vec<ObjectInfo>) = app.call("arrange", json!({}));
    assert!(!fits, "two 100 mm squares do not fit on 192 x 120 mm");
}

#[test]
fn undo_and_redo_restore_exact_states() {
    let app = TestApp::new();
    assert_eq!(app.history(), (false, false));
    assert_eq!(app.fail("undo", json!({})), "nothing to undo");
    assert_eq!(app.fail("redo", json!({})), "nothing to redo");

    let mut states = vec![app.objects()];
    let a = app.add_box("a", Vec3::splat(10.0));
    states.push(app.objects());
    let b = app.add_box("b", Vec3::new(5.0, 8.0, 12.0));
    states.push(app.objects());
    app.move_by(a.id, Vec3::new(3.0, 4.0, 0.0), true);
    states.push(app.objects());
    let mut t = app.object(b.id).transform;
    t.rotation = Quat::from_rotation_z(0.7);
    t.scale = Vec3::splat(1.5);
    app.set_transform(b.id, t, true);
    states.push(app.objects());
    app.run("duplicate_object", json!({ "id": b.id, "count": 2 }));
    states.push(app.objects());
    app.run("remove_objects", json!({ "ids": [a.id] }));
    states.push(app.objects());
    assert_eq!(app.history(), (true, false));

    // step all the way back ...
    for i in (0..states.len() - 1).rev() {
        let r: Vec<ObjectInfo> = app.call("undo", json!({}));
        assert_eq!(r, states[i], "undo to state {i}");
        assert_eq!(app.objects(), states[i]);
    }
    assert_eq!(app.history(), (false, true));
    // ... and forward again
    for (i, st) in states.iter().enumerate().skip(1) {
        let r: Vec<ObjectInfo> = app.call("redo", json!({}));
        assert_eq!(&r, st, "redo to state {i}");
    }
    assert_eq!(app.history(), (true, false));

    // a new edit after undo drops the redo history
    app.run("undo", json!({}));
    app.run("undo", json!({}));
    assert_eq!(app.history(), (true, true));
    app.call::<Vec<ObjectInfo>>("add_primitive", json!({ "kind": "cube" }));
    assert_eq!(app.history(), (true, false));
    assert_eq!(app.fail("redo", json!({})), "nothing to redo");
    // new project is undoable too
    let before = app.objects();
    app.run("new_project", json!({}));
    assert!(app.objects().is_empty());
    let r: Vec<ObjectInfo> = app.call("undo", json!({}));
    assert_eq!(r, before);
}

#[test]
fn rejected_edits_leave_the_history_alone() {
    let app = TestApp::new();
    let a = app.add_box("a", Vec3::splat(10.0));
    app.move_by(a.id, Vec3::new(5.0, 0.0, 0.0), true);
    app.run("undo", json!({}));
    let before = app.objects();
    assert_eq!(app.history(), (true, true));

    let t = a.transform;
    let n = [0.0, 0.0, 1.0];
    let p = [0.0, 0.0, 10.0];
    for (cmd, args) in [
        (
            "set_transform",
            json!({ "id": 99, "transform": t, "keepOnPlate": true }),
        ),
        ("duplicate_object", json!({ "id": 99, "count": 2 })),
        ("lay_flat", json!({ "id": 99 })),
        ("set_support_points", json!({ "id": 99, "points": [] })),
        ("clear_supports", json!({ "id": 99 })),
        ("fill_plate", json!({ "id": 99 })),
        (
            "add_drain_hole",
            json!({ "id": 99, "position": p, "normal": n, "diameter": 2.0, "depth": 2.0 }),
        ),
        ("clear_cutouts", json!({ "id": 99 })),
        (
            "add_label",
            json!({ "id": 99, "text": "A", "options": {}, "position": p, "normal": n }),
        ),
        ("add_platform", json!({ "id": 99, "options": {} })),
        ("hollow_object", json!({ "id": 99, "options": {} })),
        ("cut_object", json!({ "id": 99, "axis": "z", "position": 1.0 })),
        ("split_object", json!({ "id": 99 })),
        ("combine_objects", json!({ "ids": [99, 98] })),
        ("add_primitive", json!({ "kind": "pyramid" })),
    ] {
        let e = app.fail(cmd, args);
        assert!(!e.is_empty());
        assert_eq!(app.history(), (true, true), "{cmd} changed the undo history");
        assert_eq!(app.objects(), before, "{cmd} changed the scene");
    }
    // the redo step survived all of that
    let r: Vec<ObjectInfo> = app.call("redo", json!({}));
    assert_near(r[0].bounds.min.x, before[0].bounds.min.x + 5.0, 1e-4, "redo");
}

#[test]
fn undo_history_is_limited_to_fifty_steps() {
    let app = TestApp::new();
    let o = app.add_box("a", Vec3::splat(10.0));
    for i in 0..60 {
        app.move_by(o.id, Vec3::new(if i % 2 == 0 { 1.0 } else { -1.0 }, 0.0, 0.0), true);
    }
    let mut n = 0;
    while app.try_invoke("undo", json!({})).is_ok() {
        n += 1;
    }
    assert_eq!(n, 50);
}

#[test]
fn lay_flat_on_best_or_chosen_face() {
    let app = TestApp::new();
    let o = app.add_box("box", Vec3::new(10.0, 20.0, 30.0));
    // tilt it, then lay it on its largest face (20 x 30): 10 mm tall
    let mut t = o.transform;
    t.rotation = Quat::from_rotation_x(0.4) * Quat::from_rotation_y(0.3);
    let tilted = app.set_transform(o.id, t, true);
    assert!(size(&tilted.bounds).z > 30.0);
    let flat: ObjectInfo = app.call("lay_flat", json!({ "id": o.id }));
    assert_near(size(&flat.bounds).z, 10.0, 1e-3, "on the largest face");
    assert_near(flat.bounds.min.z, 0.0, 1e-4, "on the plate");
    assert_rel(flat.volume_mm3, 6000.0, 1e-4, "volume");

    // choose a face (index into the world mesh, which matches the local
    // triangle order): a +X face (10 mm in X) and a -Y face (20 mm in Y)
    app.set_transform(o.id, o.transform, true);
    let mesh = app.mesh("get_mesh", o.id);
    let face = (0..mesh.triangle_count())
        .find(|&i| mesh.face_normal(i).x > 0.99)
        .expect("a +X face");
    let face_y = (0..mesh.triangle_count())
        .find(|&i| mesh.face_normal(i).y < -0.99)
        .expect("a -Y face");
    let on_x: ObjectInfo = app.call("lay_flat", json!({ "id": o.id, "face": face }));
    assert_near(size(&on_x.bounds).z, 10.0, 1e-3, "lying on the X side");
    app.set_transform(o.id, o.transform, true);
    let on_y: ObjectInfo = app.call("lay_flat", json!({ "id": o.id, "face": face_y }));
    assert_near(size(&on_y.bounds).z, 20.0, 1e-3, "lying on the Y side");
    // an invalid face index falls back to the best face
    app.set_transform(o.id, o.transform, true);
    let best: ObjectInfo = app.call("lay_flat", json!({ "id": o.id, "face": 100000 }));
    assert_near(size(&best.bounds).z, 10.0, 1e-3, "fallback");
    assert!(app.fail("lay_flat", json!({ "id": 42 })).contains("not found"));
}

#[test]
fn supports_generate_edit_and_clear() {
    let app = TestApp::new();
    let o = app.add_box("cube", Vec3::splat(10.0));
    let settings = SupportSettings {
        lift_height: 4.0,
        ..Default::default()
    };
    let r: Vec<ObjectInfo> = app.call("auto_supports", json!({ "ids": [o.id, 999], "settings": settings }));
    assert_eq!(r.len(), 1, "unknown ids are skipped");
    let sup = &r[0];
    assert!(sup.support_points >= 4, "{} support points", sup.support_points);
    assert_near(sup.bounds.min.z, 4.0, 1e-4, "lifted to the lift height");
    // the settings apply to the object; the saved defaults are left alone
    let cfg: serde_json::Value = app.call("get_config", json!({}));
    let saved: SupportSettings = serde_json::from_value(cfg["support_settings"].clone()).unwrap();
    assert_eq!(saved, SupportSettings::default());

    let pts: Vec<SupportPoint> = app.call("get_support_points", json!({ "id": o.id }));
    assert_eq!(pts.len(), sup.support_points);
    for p in &pts {
        // contacts sit on the bottom face, which faces down
        assert_near(p.position.z, 4.0, 0.05, "contact height");
        assert!(
            p.position.x.abs() <= 5.01 && p.position.y.abs() <= 5.01,
            "contact {p:?} outside the face"
        );
        assert!(p.normal.z < -0.9, "normal {:?}", p.normal);
    }
    let tower = app.mesh("get_support_mesh", o.id);
    assert!(tower.triangle_count() > 0);
    let b = tower.bounds();
    assert_near(b.min.z, 0.0, 1e-3, "supports stand on the plate");
    assert!(b.max.z >= 4.0 - 1e-3, "supports reach the model ({})", b.max.z);

    // moving keeps the supports, they move with the model
    let moved = app.move_by(o.id, Vec3::new(7.0, 0.0, 0.0), true);
    assert_eq!(moved.support_points, sup.support_points);
    assert_near(moved.bounds.min.z, 4.0, 1e-4, "still lifted");
    let pts2: Vec<SupportPoint> = app.call("get_support_points", json!({ "id": o.id }));
    assert_vec_near(
        pts2[0].position,
        pts[0].position + Vec3::new(7.0, 0.0, 0.0),
        1e-3,
        "points follow",
    );

    // manual points replace the automatic ones
    let manual = vec![
        SupportPoint {
            position: Vec3::new(5.0, -2.0, 4.0),
            normal: Vec3::NEG_Z,
            size: Default::default(),
        },
        SupportPoint {
            position: Vec3::new(9.0, 2.0, 4.0),
            normal: Vec3::NEG_Z,
            size: Default::default(),
        },
    ];
    let m: ObjectInfo = app.call("set_support_points", json!({ "id": o.id, "points": manual }));
    assert_eq!(m.support_points, 2);
    let back: Vec<SupportPoint> = app.call("get_support_points", json!({ "id": o.id }));
    assert_eq!(back.len(), 2);
    for (a, b) in back.iter().zip(&manual) {
        assert_vec_near(a.position, b.position, 1e-4, "manual point");
        assert_vec_near(a.normal, b.normal, 1e-4, "manual normal");
    }

    // rotating invalidates the supports
    let mut t = m.transform;
    t.rotation = Quat::from_rotation_z(0.5);
    let rot = app.set_transform(o.id, t, true);
    assert_eq!(rot.support_points, 0);
    assert_near(rot.bounds.min.z, 0.0, 1e-4, "back on the plate");

    // the first manual point on an unsupported model lifts it
    let c = rot.bounds.center();
    let first = vec![SupportPoint {
        position: Vec3::new(c.x, c.y, 0.0),
        normal: Vec3::NEG_Z,
        size: Default::default(),
    }];
    let lifted: ObjectInfo = app.call("set_support_points", json!({ "id": o.id, "points": first }));
    assert_eq!(lifted.support_points, 1);
    assert_near(lifted.bounds.min.z, 4.0, 1e-3, "lifted by the first support");
    let pts: Vec<SupportPoint> = app.call("get_support_points", json!({ "id": o.id }));
    assert_near(pts[0].position.z, 4.0, 1e-3, "the point moved with the model");

    let cleared: ObjectInfo = app.call("clear_supports", json!({ "id": o.id }));
    assert_eq!(cleared.support_points, 0);
    assert_near(cleared.bounds.min.z, 0.0, 1e-4, "dropped after clearing");
    assert_eq!(app.mesh("get_support_mesh", o.id).triangle_count(), 0);
    assert!(app
        .fail("get_support_points", json!({ "id": 999 }))
        .contains("not found"));
    assert!(app.fail("clear_supports", json!({ "id": 999 })).contains("not found"));
}

#[test]
fn magic_supports_do_not_change_the_saved_support_settings() {
    let app = TestApp::new();
    let saved = SupportSettings {
        spacing: 3.0,
        overhang_angle: 45.0,
        lift_height: 6.0,
        detect_islands: false,
        model_contacts: false,
        ..Default::default()
    };
    app.run("set_support_settings", json!({ "settings": saved }));
    let o = app.add_box("cube", Vec3::splat(10.0));

    // what the frontend's Magic Support sends (actions.ts autoSupports({ magic: true })):
    // derived from the saved settings each time
    let magic = |s: &SupportSettings| SupportSettings {
        spacing: (s.spacing * 0.75).max(1.0),
        overhang_angle: (s.overhang_angle + 10.0).min(60.0),
        detect_islands: true,
        model_contacts: true,
        ..s.clone()
    };
    let mut counts = Vec::new();
    for _ in 0..2 {
        let cfg: serde_json::Value = app.call("get_config", json!({}));
        let current: SupportSettings = serde_json::from_value(cfg["support_settings"].clone()).unwrap();
        assert_eq!(current, saved, "saved support settings changed");
        let r: Vec<ObjectInfo> = app.call("auto_supports", json!({ "ids": [o.id], "settings": magic(&current) }));
        assert_near(r[0].bounds.min.z, 6.0, 1e-4, "magic run uses the given lift height");
        counts.push(r[0].support_points);
    }
    // both runs started from the same settings, so they place the same supports
    assert_eq!(counts[0], counts[1], "magic supports compounded: {counts:?}");
    let cfg: serde_json::Value = app.call("get_config", json!({}));
    assert_eq!(
        serde_json::from_value::<SupportSettings>(cfg["support_settings"].clone()).unwrap(),
        saved
    );
    // also on disk
    let again = app.reopen();
    let cfg: serde_json::Value = again.call("get_config", json!({}));
    assert_eq!(
        serde_json::from_value::<SupportSettings>(cfg["support_settings"].clone()).unwrap(),
        saved
    );

    // the denser magic spacing gives more supports than the saved settings
    let plain: Vec<ObjectInfo> = app.call("auto_supports", json!({ "ids": [o.id], "settings": saved }));
    assert!(
        plain[0].support_points < counts[0],
        "{} vs magic {}",
        plain[0].support_points,
        counts[0]
    );
}

#[test]
fn pick_surface_hits_the_closest_face() {
    let app = TestApp::new();
    let o = app.add_box("cube", Vec3::splat(10.0));
    let hit: Option<SupportPoint> = app.call(
        "pick_surface",
        json!({ "id": o.id, "origin": [1.0, 2.0, 50.0], "direction": [0.0, 0.0, -1.0] }),
    );
    let hit = hit.expect("ray hits the top face");
    assert_vec_near(hit.position, Vec3::new(1.0, 2.0, 10.0), 1e-3, "top hit");
    assert_vec_near(hit.normal, Vec3::Z, 1e-4, "top normal");
    // from below the bottom face is closest
    let hit: Option<SupportPoint> = app.call(
        "pick_surface",
        json!({ "id": o.id, "origin": [0.0, 0.0, -20.0], "direction": [0.0, 0.0, 5.0] }),
    );
    assert_vec_near(hit.unwrap().position, Vec3::ZERO, 1e-3, "bottom hit");
    let miss: Option<SupportPoint> = app.call(
        "pick_surface",
        json!({ "id": o.id, "origin": [30.0, 0.0, 50.0], "direction": [0.0, 0.0, -1.0] }),
    );
    assert!(miss.is_none());
}

#[test]
fn export_stl_with_and_without_supports() {
    let app = TestApp::new();
    let a = app.add_box("a", Vec3::splat(10.0));
    let b = app.add_box("b", Vec3::new(6.0, 6.0, 20.0));
    app.run(
        "auto_supports",
        json!({ "ids": [a.id], "settings": SupportSettings::default() }),
    );
    let objs = app.objects();
    let world = objs.iter().fold(Aabb::EMPTY, |acc, o| acc.union(&o.bounds));

    let plain = app.dir.join("plain.stl");
    app.run("export_stl", json!({ "path": s(&plain), "withSupports": false }));
    let m = openresin_core::io::parse_stl_bytes(&std::fs::read(&plain).unwrap()).unwrap();
    assert_eq!(m.triangle_count(), 24);
    assert_vec_near(m.bounds().min, world.min, 1e-3, "exported in plate coordinates");
    assert_vec_near(m.bounds().max, world.max, 1e-3, "exported in plate coordinates");
    let vol = m.signed_volume();
    assert_rel(vol, 1000.0 + 720.0, 1e-3, "exported volume");

    let full = app.dir.join("supports.stl");
    app.run("export_stl", json!({ "path": s(&full), "withSupports": true }));
    let ms = openresin_core::io::parse_stl_bytes(&std::fs::read(&full).unwrap()).unwrap();
    let support = app.mesh("get_support_mesh", a.id);
    assert_eq!(ms.triangle_count(), 24 + support.triangle_count());
    assert_near(ms.bounds().min.z, 0.0, 1e-3, "supports reach the plate");
    assert!(objs.iter().any(|o| o.id == b.id));

    let e = app.fail(
        "export_stl",
        json!({ "path": s(&app.dir.join("no-such-dir").join("x.stl")), "withSupports": false }),
    );
    assert!(!e.is_empty());
}

#[test]
fn project_roundtrip_restores_scene_and_settings() {
    let app = TestApp::new();
    app.use_printer("elegoo-mars-4-dlp", 0.04, 2);
    let a = app.add_box("part", Vec3::new(10.0, 12.0, 8.0));
    let b = app.add_box("cutter", Vec3::splat(4.0));
    let c = app.add_box("hollow", Vec3::splat(16.0));
    app.run(
        "auto_supports",
        json!({ "ids": [a.id], "settings": SupportSettings::default() }),
    );
    let mut t = app.object(b.id).transform;
    t.rotation = Quat::from_rotation_z(0.3);
    t.scale = Vec3::new(1.0, 2.0, 1.0);
    app.set_transform(b.id, t, true);
    app.run("set_subtract", json!({ "ids": [b.id], "subtract": true }));
    app.run(
        "hollow_object",
        json!({ "id": c.id, "options": { "wall_thickness": 2.0 } }),
    );
    app.run("rename_object", json!({ "id": c.id, "name": "Hollow box" }));
    let saved = app.objects();
    let support_tris = app.mesh("get_support_mesh", a.id).triangle_count();
    let cavity = app.mesh("get_cutout_mesh", c.id);
    assert!(support_tris > 0 && cavity.triangle_count() > 0);
    let path = app.dir.join("plate.orproj");
    app.run("save_project", json!({ "path": s(&path) }));
    assert!(path.is_file());

    // clear everything and switch printer
    app.run("new_project", json!({}));
    app.run("set_printer", json!({ "id": "generic-4k" }));
    assert!(app.objects().is_empty());

    let opened: Vec<ObjectInfo> = app.call("open_project", json!({ "path": s(&path) }));
    assert_eq!(opened.len(), saved.len());
    for (x, y) in opened.iter().zip(&saved) {
        assert_eq!(x.id, y.id);
        assert_eq!(x.name, y.name);
        assert_eq!(x.triangles, y.triangles);
        assert_eq!(x.support_points, y.support_points);
        assert_eq!(x.subtract, y.subtract);
        assert_eq!(x.cutouts, y.cutouts);
        assert_vec_near(x.transform.translation, y.transform.translation, 1e-5, "translation");
        assert!(x.transform.rotation.abs_diff_eq(y.transform.rotation, 1e-6));
        assert_vec_near(x.transform.scale, y.transform.scale, 1e-6, "scale");
        assert_vec_near(x.bounds.min, y.bounds.min, 1e-4, "bounds");
        assert_vec_near(x.bounds.max, y.bounds.max, 1e-4, "bounds");
        assert_rel(x.volume_mm3, y.volume_mm3, 1e-6, "volume");
    }
    // the project brings its printer and print settings
    let cfg: serde_json::Value = app.call("get_config", json!({}));
    assert_eq!(cfg["printer"]["id"], "elegoo-mars-4-dlp");
    assert_eq!(cfg["settings"]["anti_aliasing"], 2);
    assert!((cfg["settings"]["layer_height"].as_f64().unwrap() - 0.04).abs() < 1e-6);
    // supports and cutouts survived: same support geometry, same cavity
    assert_eq!(app.mesh("get_support_mesh", a.id).triangle_count(), support_tris);
    let cavity2 = app.mesh("get_cutout_mesh", c.id);
    assert_eq!(cavity2.triangle_count(), cavity.triangle_count());
    assert_vec_near(cavity2.bounds().min, cavity.bounds().min, 1e-3, "cavity");
    assert_vec_near(cavity2.bounds().max, cavity.bounds().max, 1e-3, "cavity");
    // a freshly started app opens the same project
    let fresh = app.reopen();
    let o: Vec<ObjectInfo> = fresh.call("open_project", json!({ "path": s(&path) }));
    assert_eq!(o.len(), 3);
    assert_eq!(
        o.iter().map(|x| &x.name).collect::<Vec<_>>(),
        saved.iter().map(|x| &x.name).collect::<Vec<_>>()
    );
    // new objects after loading get fresh ids
    let n = app.add_box("new", Vec3::splat(5.0));
    assert!(saved.iter().all(|o| o.id != n.id));
}

#[test]
fn failed_project_open_keeps_the_scene_and_history() {
    let app = TestApp::new();
    let a = app.add_box("a", Vec3::splat(10.0));
    app.move_by(a.id, Vec3::new(5.0, 0.0, 0.0), true);
    app.run("undo", json!({}));
    let before = app.objects();
    assert_eq!(app.history(), (true, true));

    let junk = app.dir.join("broken.orproj");
    std::fs::write(&junk, b"not a zip").unwrap();
    app.fail("open_project", json!({ "path": s(&junk) }));
    app.fail("open_project", json!({ "path": s(&app.dir.join("missing.orproj")) }));
    assert_eq!(app.objects(), before);
    // the failed opens did not add undo steps or discard the redo step
    assert_eq!(app.history(), (true, true));
    let r: Vec<ObjectInfo> = app.call("redo", json!({}));
    assert_near(
        r[0].bounds.min.x,
        before[0].bounds.min.x + 5.0,
        1e-4,
        "redo still works",
    );

    let e = app.fail(
        "save_project",
        json!({ "path": s(&app.dir.join("nope").join("x.orproj")) }),
    );
    assert!(!e.is_empty());
}

#[test]
fn mesh_commands_reject_unknown_objects() {
    let app = TestApp::new();
    for cmd in ["get_mesh", "get_support_mesh", "get_cutout_mesh"] {
        let e = app.fail(cmd, json!({ "id": 7 }));
        assert!(e.contains("not found"), "{cmd}: {e}");
    }
    let empty = Mesh::default();
    assert!(empty.is_empty());
}
