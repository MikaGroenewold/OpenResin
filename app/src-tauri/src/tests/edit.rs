//! Fill plate, align, combine, auto orientation, subtract, primitives, rename.

use std::f64::consts::PI;

use openresin_core::{Quat, Vec3};
use serde_json::json;

use super::*;

#[test]
fn fill_plate_adds_as_many_copies_as_fit() {
    let app = TestApp::new();
    let o = app.add_box("cube", Vec3::splat(20.0));
    let (added, all): (usize, Vec<ObjectInfo>) = app.call("fill_plate", json!({ "id": o.id, "spacing": 3.0 }));
    // generic 4K plate is 192 x 120 mm: 8 columns (8*20 + 7*3 = 181) and 5 rows (5*20 + 4*3 = 112)
    assert_eq!(added, 39);
    assert_eq!(all.len(), 40);
    assert_laid_out(&all, 192.0, 120.0, 2.99);
    assert_eq!(app.objects(), all);
    // a full plate stays full
    let (added, again): (usize, Vec<ObjectInfo>) = app.call("fill_plate", json!({ "id": o.id, "spacing": 3.0 }));
    assert_eq!(added, 0);
    assert_eq!(again, all, "nothing was added or moved");

    // one undo step removes all copies
    app.run("undo", json!({}));
    app.run("undo", json!({}));
    let back = app.objects();
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].id, o.id);

    // wider spacing, fewer copies (default spacing is 3 mm)
    let (added, all): (usize, Vec<ObjectInfo>) = app.call("fill_plate", json!({ "id": o.id, "spacing": 10.0 }));
    // 6 columns (6*20 + 5*10 = 170), 4 rows (4*20 + 3*10 = 110)
    assert_eq!(added + 1, 24);
    assert_laid_out(&all, 192.0, 120.0, 9.99);
    app.run("undo", json!({}));
    let (added, _): (usize, Vec<ObjectInfo>) = app.call("fill_plate", json!({ "id": o.id }));
    assert_eq!(added, 39);

    assert!(app.fail("fill_plate", json!({ "id": 999 })).contains("not found"));

    // an overfull plate is reported
    let big = app.add_box("big", Vec3::new(100.0, 100.0, 10.0));
    app.run("duplicate_object", json!({ "id": big.id, "count": 1 }));
    let n = app.objects().len();
    let e = app.fail("fill_plate", json!({ "id": big.id }));
    assert!(e.contains("already full"), "{e}");
    assert_eq!(app.objects().len(), n);
}

#[test]
fn align_objects_in_every_mode() {
    let app = TestApp::new();
    let a = app.add_box("a", Vec3::splat(10.0));
    let b = app.add_box("b", Vec3::new(20.0, 6.0, 8.0));
    let c = app.add_box("c", Vec3::new(4.0, 30.0, 12.0));
    let ids = [a.id, b.id, c.id];
    let objs = app.objects();
    let group = objs.iter().fold(Aabb::EMPTY, |acc, o| acc.union(&o.bounds));

    let check = |mode: &str, f: &dyn Fn(&ObjectInfo) -> f32, want: f32| {
        // start from the arranged layout every time
        app.call::<(bool, Vec<ObjectInfo>)>("arrange", json!({}));
        let r: Vec<ObjectInfo> = app.call("align_objects", json!({ "ids": ids, "mode": mode }));
        for o in &r {
            assert_near(f(o), want, 1e-3, &format!("{mode}: {}", o.name));
        }
    };
    check("left", &|o| o.bounds.min.x, group.min.x);
    check("right", &|o| o.bounds.max.x, group.max.x);
    check("front", &|o| o.bounds.min.y, group.min.y);
    check("back", &|o| o.bounds.max.y, group.max.y);
    check("center_x", &|o| o.bounds.center().x, group.center().x);
    check("center_y", &|o| o.bounds.center().y, group.center().y);

    // plate_center moves the whole group, keeping relative positions
    app.move_by(a.id, Vec3::new(30.0, 20.0, 0.0), true);
    let before = app.objects();
    let gb = before.iter().fold(Aabb::EMPTY, |acc, o| acc.union(&o.bounds));
    let r: Vec<ObjectInfo> = app.call("align_objects", json!({ "ids": ids, "mode": "plate_center" }));
    let ga = r.iter().fold(Aabb::EMPTY, |acc, o| acc.union(&o.bounds));
    assert_vec_near(
        ga.center() * Vec3::new(1.0, 1.0, 0.0),
        Vec3::ZERO,
        1e-3,
        "group centred",
    );
    let shift = ga.center() - gb.center();
    for (x, y) in r.iter().zip(&before) {
        assert_vec_near(x.bounds.min - y.bounds.min, shift, 1e-3, "rigid move");
    }

    // drop puts floating models back on the plate (or on their supports)
    app.move_by(b.id, Vec3::new(0.0, 0.0, 7.0), false);
    let r: Vec<ObjectInfo> = app.call("align_objects", json!({ "ids": [b.id], "mode": "drop" }));
    let b2 = r.iter().find(|o| o.id == b.id).unwrap();
    assert_near(b2.bounds.min.z, 0.0, 1e-4, "dropped");

    // only the selected objects move
    let before = app.objects();
    let r: Vec<ObjectInfo> = app.call("align_objects", json!({ "ids": [a.id, c.id], "mode": "left" }));
    let bb = r.iter().find(|o| o.id == b.id).unwrap();
    assert_eq!(bb, before.iter().find(|o| o.id == b.id).unwrap());

    let e = app.fail("align_objects", json!({ "ids": ids, "mode": "diagonal" }));
    assert!(e.contains("unknown align mode"), "{e}");
    let e = app.fail("align_objects", json!({ "ids": [], "mode": "left" }));
    assert!(e.contains("nothing selected"), "{e}");
    let e = app.fail("align_objects", json!({ "ids": [12345], "mode": "left" }));
    assert!(e.contains("nothing selected"), "{e}");
}

#[test]
fn combine_merges_objects_in_place() {
    let app = TestApp::new();
    let a = app.add_box("left", Vec3::splat(10.0));
    let b = app.add_box("right", Vec3::new(6.0, 6.0, 15.0));
    let c = app.add_box("other", Vec3::splat(5.0));
    let objs = app.objects();
    let union = objs[..2].iter().fold(Aabb::EMPTY, |acc, o| acc.union(&o.bounds));

    let r: Vec<ObjectInfo> = app.call("combine_objects", json!({ "ids": [a.id, b.id] }));
    assert_eq!(r.len(), 2);
    let m = r.iter().find(|o| o.id != c.id).unwrap();
    assert_eq!(m.name, "left (combined)");
    assert_eq!(m.triangles, 24);
    assert_rel(m.volume_mm3, 1000.0 + 540.0, 1e-4, "combined volume");
    assert_vec_near(m.bounds.min, union.min, 1e-3, "stays in place");
    assert_vec_near(m.bounds.max, union.max, 1e-3, "stays in place");
    assert!(m.id != a.id && m.id != b.id);
    assert_eq!(r.iter().find(|o| o.id == c.id).unwrap(), &objs[2], "untouched object");

    let e = app.fail("combine_objects", json!({ "ids": [c.id] }));
    assert!(e.contains("at least two"), "{e}");
    let before = app.objects();
    let (undo_before, _) = app.history();
    // ids that do not exist must not create anything
    let e = app.fail("combine_objects", json!({ "ids": [777, 778] }));
    assert!(e.contains("at least two"), "{e}");
    let e = app.fail("combine_objects", json!({ "ids": [c.id, 779] }));
    assert!(e.contains("at least two"), "{e}");
    assert_eq!(app.objects(), before, "failed combine changed the scene");
    assert_eq!(app.history().0, undo_before);

    // undo restores both originals
    let r: Vec<ObjectInfo> = app.call("undo", json!({}));
    assert_eq!(r, objs);
}

#[test]
fn auto_orient_goals() {
    let app = TestApp::new();
    let slab = app.add_box("slab", Vec3::new(4.0, 6.0, 20.0));
    let tilted = Quat::from_rotation_x(0.5) * Quat::from_rotation_y(0.35);

    for goal in ["lowest_height", "fewest_supports", "balanced"] {
        let mut t = slab.transform;
        t.rotation = tilted;
        let start = app.set_transform(slab.id, t, true);
        app.run("auto_supports", json!({ "ids": [slab.id], "settings": {} }));
        let r: Vec<ObjectInfo> = app.call(
            "auto_orient",
            json!({ "ids": [slab.id], "options": { "goal": goal, "candidates": 200 } }),
        );
        let o = &r[0];
        let h = size(&o.bounds).z;
        assert_near(o.bounds.min.z, 0.0, 1e-4, &format!("{goal}: on the plate"));
        assert_eq!(o.support_points, 0, "{goal}: supports are cleared");
        assert_rel(o.volume_mm3, 480.0, 1e-4, &format!("{goal}: volume"));
        assert!(o.transform.rotation.is_finite() && o.transform.rotation.is_normalized());
        match goal {
            "lowest_height" => {
                assert!(h < size(&start.bounds).z, "lower than the tilted start");
                assert_near(h, 4.0, 0.05, "lowest height lies on the 6 x 20 face");
            }
            // a box has no overhangs when it stands on any face: no tilt left
            "fewest_supports" => assert!(
                [4.0, 6.0, 20.0].iter().any(|d| (h - d).abs() < 0.05),
                "fewest supports: axis aligned, got height {h}"
            ),
            _ => {}
        }
    }
    // options are optional; unknown ids are ignored
    let r: Vec<ObjectInfo> = app.call("auto_orient", json!({ "ids": [slab.id, 999] }));
    assert_eq!(r.len(), 1);
    let e = app.fail(
        "auto_orient",
        json!({ "ids": [slab.id], "options": { "goal": "prettiest" } }),
    );
    assert!(e.contains("goal") || e.contains("prettiest"), "{e}");
}

#[test]
fn subtract_role_and_primitives() {
    let app = TestApp::new();
    let cube: Vec<ObjectInfo> = app.call("add_primitive", json!({ "kind": "cube", "size": 20.0 }));
    assert_eq!(cube.len(), 1);
    assert_eq!(cube[0].name, "Cube 20 mm");
    assert_rel(cube[0].volume_mm3, 8000.0, 1e-4, "cube");
    assert_vec_near(size(&cube[0].bounds), Vec3::splat(20.0), 1e-4, "cube size");

    let r = 10.0f64;
    for (kind, name, volume, tol) in [
        ("cylinder", "Cylinder", PI * r * r * 20.0, 0.01),
        ("sphere", "Sphere", 4.0 / 3.0 * PI * r * r * r, 0.01),
        ("cone", "Cone", PI * r * r * 20.0 / 3.0, 0.01),
    ] {
        let all: Vec<ObjectInfo> = app.call("add_primitive", json!({ "kind": kind }));
        let o = all.last().unwrap();
        assert_eq!(o.name, format!("{name} 20 mm"));
        assert_rel(o.volume_mm3, volume, tol, kind);
        assert_vec_near(size(&o.bounds), Vec3::splat(20.0), 1e-2, kind);
    }
    let all = app.objects();
    assert_eq!(all.len(), 4);
    assert_laid_out(&all, 192.0, 120.0, 2.9);
    // size is clamped to 1..500 mm
    let tiny: Vec<ObjectInfo> = app.call("add_primitive", json!({ "kind": "cube", "size": 0.01 }));
    assert_vec_near(size(&tiny.last().unwrap().bounds), Vec3::splat(1.0), 1e-4, "clamped");
    assert!(app
        .fail("add_primitive", json!({ "kind": "torus" }))
        .contains("unknown shape"));

    // subtract role
    let id = cube[0].id;
    app.run("auto_supports", json!({ "ids": [id], "settings": {} }));
    let r: Vec<ObjectInfo> = app.call("set_subtract", json!({ "ids": [id, 999], "subtract": true }));
    let c = r.iter().find(|o| o.id == id).unwrap();
    assert!(c.subtract);
    assert_eq!(c.support_points, 0, "cutters carry no supports");
    let r: Vec<ObjectInfo> = app.call("set_subtract", json!({ "ids": [id], "subtract": false }));
    assert!(!r.iter().find(|o| o.id == id).unwrap().subtract);
    app.run("undo", json!({}));
    assert!(app.object(id).subtract, "undo restores the role");
    // cutters are not moved by arrange
    let before = app.object(id);
    let (_, after): (bool, Vec<ObjectInfo>) = app.call("arrange", json!({}));
    assert_eq!(after.iter().find(|o| o.id == id).unwrap(), &before);
}

#[test]
fn rename_object_changes_only_the_name() {
    let app = TestApp::new();
    let o = app.add_box("old", Vec3::splat(10.0));
    let r: ObjectInfo = app.call("rename_object", json!({ "id": o.id, "name": "New name ✓" }));
    assert_eq!(r.name, "New name ✓");
    assert_eq!(
        ObjectInfo {
            name: o.name.clone(),
            ..r.clone()
        },
        o
    );
    assert_eq!(app.object(o.id).name, "New name ✓");
    assert!(app
        .fail("rename_object", json!({ "id": 5, "name": "x" }))
        .contains("not found"));
}
