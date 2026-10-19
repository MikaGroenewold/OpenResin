//! Mesh tools: analysis and repair, split, cut, hollowing, drain holes,
//! labels and platforms. Where the result matters for printing it is checked
//! by slicing and measuring the layer images.

use openresin_core::primitives::cuboid;
use openresin_core::repair::{MeshReport, RepairReport};
use openresin_core::{Mesh, Vec3};
use serde::Deserialize;
use serde_json::json;

use super::*;

const PRINTER: &str = "elegoo-mars-4-dlp";

#[derive(Deserialize)]
struct RepairResult {
    id: u64,
    name: String,
    report: RepairReport,
}

fn cube20(app: &TestApp) -> ObjectInfo {
    let all: Vec<ObjectInfo> = app.call("add_primitive", json!({ "kind": "cube", "size": 20.0 }));
    all.last().unwrap().clone()
}

#[test]
fn analyze_and_repair_a_mesh_with_a_hole() {
    let app = TestApp::new();
    let mut broken = cuboid(Vec3::ZERO, Vec3::splat(10.0));
    broken.triangles.pop();
    let path = app.dir.join("holey.stl");
    write_stl(&path, &broken);
    let o = app.import_one(&path);
    assert_eq!(o.triangles, 11);

    let rep: MeshReport = app.call("analyze_object", json!({ "id": o.id }));
    assert!(!rep.watertight);
    assert_eq!(rep.triangles, 11);
    assert_eq!(rep.boundary_edges, 3);
    assert_eq!(rep.holes, 1);
    assert_eq!(rep.shells, 1);
    assert!(!rep.is_clean());

    let (results, objs): (Vec<RepairResult>, Vec<ObjectInfo>) =
        app.call("repair_objects", json!({ "ids": [o.id, 404], "options": null }));
    assert_eq!(results.len(), 1, "unknown ids are skipped");
    let r = &results[0];
    assert_eq!((r.id, r.name.as_str()), (o.id, "holey"));
    assert_eq!(r.report.before.holes, 1);
    assert_eq!(r.report.holes_filled, 1);
    assert!(r.report.after.watertight, "{:?}", r.report.after);
    let fixed = objs.iter().find(|x| x.id == o.id).unwrap();
    assert_eq!(fixed.triangles, 12);
    assert_ne!(fixed.mesh_key, o.mesh_key, "the mesh was replaced");
    assert_rel(fixed.volume_mm3, 1000.0, 1e-3, "repaired volume");
    assert_near(fixed.bounds.min.z, 0.0, 1e-4, "on the plate");

    let rep: MeshReport = app.call("analyze_object", json!({ "id": o.id }));
    assert!(rep.watertight && rep.is_clean(), "{rep:?}");
    assert_rel(rep.volume, 1000.0, 1e-3, "analysed volume");
    assert_rel(rep.area, 600.0, 1e-3, "analysed area");

    // undo brings the broken mesh back
    app.run("undo", json!({}));
    let rep: MeshReport = app.call("analyze_object", json!({ "id": o.id }));
    assert_eq!(rep.holes, 1);
    assert!(app.fail("analyze_object", json!({ "id": 404 })).contains("not found"));
}

#[test]
fn split_separates_shells_in_place() {
    let app = TestApp::new();
    let mut two = cuboid(Vec3::new(-10.0, 0.0, 5.0), Vec3::splat(10.0));
    two.append(&cuboid(Vec3::new(12.0, 3.0, 4.0), Vec3::new(6.0, 4.0, 8.0)));
    let path = app.dir.join("pair.stl");
    write_stl(&path, &two);
    let o = app.import_one(&path);
    assert_eq!(o.triangles, 24);
    let (n, objs): (usize, Vec<ObjectInfo>) = app.call("split_object", json!({ "id": o.id }));
    assert_eq!(n, 2);
    assert_eq!(objs.len(), 2);
    assert!(objs.iter().all(|x| x.id != o.id));
    let names: Vec<&str> = objs.iter().map(|x| x.name.as_str()).collect();
    assert_eq!(names, ["pair part 1", "pair part 2"]);
    let mut vols: Vec<f64> = objs.iter().map(|x| x.volume_mm3).collect();
    vols.sort_by(f64::total_cmp);
    assert_rel(vols[0], 192.0, 1e-4, "small part");
    assert_rel(vols[1], 1000.0, 1e-4, "big part");
    let union = objs.iter().fold(Aabb::EMPTY, |a, x| a.union(&x.bounds));
    assert_vec_near(union.min, o.bounds.min, 1e-3, "parts stay where they were");
    assert_vec_near(union.max, o.bounds.max, 1e-3, "parts stay where they were");

    // a single shell is left alone and adds no undo step
    let hist = app.history();
    let (n, same): (usize, Vec<ObjectInfo>) = app.call("split_object", json!({ "id": objs[0].id }));
    assert_eq!(n, 1);
    assert_eq!(same, objs);
    assert_eq!(app.history(), hist);
    assert!(app.fail("split_object", json!({ "id": 9 })).contains("not found"));
}

#[test]
fn cut_along_each_axis() {
    let app = TestApp::new();
    let o = cube20(&app);
    let objs: Vec<ObjectInfo> = app.call("cut_object", json!({ "id": o.id, "axis": "z", "position": 5.0 }));
    assert_eq!(objs.len(), 2);
    let lower = objs.iter().find(|x| x.name == "Cube 20 mm (1)").unwrap();
    let upper = objs.iter().find(|x| x.name == "Cube 20 mm (2)").unwrap();
    assert_near(size(&lower.bounds).z, 5.0, 1e-3, "lower half");
    assert_near(size(&upper.bounds).z, 15.0, 1e-3, "upper half");
    assert_rel(lower.volume_mm3, 2000.0, 1e-3, "lower volume");
    assert_rel(upper.volume_mm3, 6000.0, 1e-3, "upper volume");
    // both halves are closed solids, side by side on the plate
    assert_laid_out(&objs, 192.0, 120.0, 2.9);
    for x in &objs {
        let rep: MeshReport = app.call("analyze_object", json!({ "id": x.id }));
        assert!(rep.watertight, "{}: {rep:?}", x.name);
    }

    // a vertical cut keeps both halves in place
    app.run("undo", json!({}));
    let o = app.object(o.id);
    let cx = o.bounds.center().x;
    let objs: Vec<ObjectInfo> = app.call("cut_object", json!({ "id": o.id, "axis": "x", "position": cx + 4.0 }));
    let left = objs.iter().find(|x| x.name.ends_with("(1)")).unwrap();
    let right = objs.iter().find(|x| x.name.ends_with("(2)")).unwrap();
    assert_near(left.bounds.min.x, o.bounds.min.x, 1e-3, "left half in place");
    assert_near(left.bounds.max.x, cx + 4.0, 1e-3, "left half ends at the cut");
    assert_near(right.bounds.min.x, cx + 4.0, 1e-3, "right half starts at the cut");
    assert_near(right.bounds.max.x, o.bounds.max.x, 1e-3, "right half in place");
    assert_rel(left.volume_mm3 + right.volume_mm3, 8000.0, 1e-3, "volume is preserved");
    assert_rel(left.volume_mm3, 14.0 * 400.0, 1e-3, "left volume");

    app.run("undo", json!({}));
    let y: Vec<ObjectInfo> = app.call(
        "cut_object",
        json!({ "id": o.id, "axis": "y", "position": o.bounds.center().y }),
    );
    assert_eq!(y.len(), 2);
    assert_rel(y[0].volume_mm3, 4000.0, 1e-3, "y half");

    app.run("undo", json!({}));
    let before = app.objects();
    let e = app.fail("cut_object", json!({ "id": o.id, "axis": "z", "position": 50.0 }));
    assert!(e.contains("does not intersect"), "{e}");
    let e = app.fail("cut_object", json!({ "id": o.id, "axis": "w", "position": 1.0 }));
    assert!(e.contains("unknown axis"), "{e}");
    assert_eq!(app.objects(), before);
}

#[test]
fn hollowing_drain_holes_and_clearing_change_the_print() {
    let app = TestApp::new();
    app.use_printer(PRINTER, 0.1, 1);
    let o = cube20(&app);
    let solid_px = area_px(PRINTER, 400.0);

    let (cavity, info): (f64, ObjectInfo) = app.call(
        "hollow_object",
        json!({ "id": o.id, "options": { "wall_thickness": 2.0 } }),
    );
    assert_eq!(info.cutouts, 1);
    assert_rel(cavity, 16.0f64.powi(3), 0.12, "cavity volume");
    let cut = app.mesh("get_cutout_mesh", o.id);
    let cb = cut.bounds();
    assert_vec_near(cb.min, o.bounds.min + Vec3::splat(2.0), 0.5, "cavity inset");
    assert_vec_near(cb.max, o.bounds.max - Vec3::splat(2.0), 0.5, "cavity inset");

    let sum = app.slice(Some("goo"));
    assert_eq!(sum.layer_count, 200);
    // middle layer: 2 mm walls around a 16 mm cavity
    let walls = app.layer_lit(100) as f64;
    assert_rel(walls, area_px(PRINTER, 400.0 - 256.0), 0.1, "hollow section");
    // the bottom and top walls are solid
    assert_rel(app.layer_lit(5) as f64, solid_px, 0.02, "bottom wall");
    assert_rel(app.layer_lit(195) as f64, solid_px, 0.02, "top wall");
    assert_rel(
        sum.volume_ml,
        (8000.0 - cavity) / 1000.0,
        0.05,
        "resin volume of the shell",
    );

    // drain hole through the bottom wall
    let c = o.bounds.center();
    let hole_area = std::f64::consts::PI * 2.0 * 2.0;
    let h: ObjectInfo = app.call(
        "add_drain_hole",
        json!({ "id": o.id, "position": [c.x, c.y, 0.0], "normal": [0.0, 0.0, -1.0], "diameter": 4.0, "depth": 4.0 }),
    );
    assert_eq!(h.cutouts, 2);
    assert_eq!(
        app.fail("get_layer_image", json!({ "index": 0, "maxSize": 0 })),
        "nothing sliced yet"
    );
    app.slice(Some("goo"));
    let bottom = app.layer_lit(5) as f64;
    assert_rel(
        solid_px - bottom,
        area_px(PRINTER, hole_area),
        0.15,
        "hole in the bottom wall",
    );
    assert_rel(app.layer_lit(195) as f64, solid_px, 0.02, "top wall untouched");
    let e = app.fail(
        "add_drain_hole",
        json!({ "id": o.id, "position": [0.0, 0.0, 0.0], "normal": [0.0, 0.0, -1.0], "diameter": 0.0, "depth": 4.0 }),
    );
    assert!(e.contains("invalid drain hole"), "{e}");
    assert_eq!(app.object(o.id).cutouts, 2);

    // clearing restores the solid cube
    let cl: ObjectInfo = app.call("clear_cutouts", json!({ "id": o.id }));
    assert_eq!(cl.cutouts, 0);
    assert_eq!(app.mesh("get_cutout_mesh", o.id).triangle_count(), 0);
    app.slice(Some("goo"));
    assert_rel(app.layer_lit(100) as f64, solid_px, 0.02, "solid again");

    // hollowing again replaces the old cutouts
    app.run(
        "add_drain_hole",
        json!({ "id": o.id, "position": [c.x, c.y, 0.0], "normal": [0.0, 0.0, -1.0], "diameter": 3.0, "depth": 3.0 }),
    );
    let (_, again): (f64, ObjectInfo) = app.call(
        "hollow_object",
        json!({ "id": o.id, "options": { "wall_thickness": 3.0 } }),
    );
    assert_eq!(again.cutouts, 1);

    // too thin to hollow: error, nothing changes
    let plate = app.add_box("plate", Vec3::new(30.0, 30.0, 2.0));
    let before = app.objects();
    let e = app.fail(
        "hollow_object",
        json!({ "id": plate.id, "options": { "wall_thickness": 2.0 } }),
    );
    assert!(e.contains("too thin"), "{e}");
    assert_eq!(app.objects(), before);
    assert!(app
        .fail("hollow_object", json!({ "id": 999, "options": {} }))
        .contains("not found"));
    assert!(app.fail("clear_cutouts", json!({ "id": 999 })).contains("not found"));
}

#[test]
fn labels_emboss_and_engrave() {
    let app = TestApp::new();
    app.use_printer(PRINTER, 0.1, 1);
    let a = cube20(&app);
    let b = cube20(&app);
    let a = app.object(a.id);
    let ca = a.bounds.center();
    let cb = app.object(b.id).bounds.center();
    let opts = |mode: &str| json!({ "height": 6.0, "depth": 1.0, "mode": mode });

    let em: ObjectInfo = app.call(
        "add_label",
        json!({ "id": a.id, "text": "OR", "options": opts("emboss"), "position": [ca.x, ca.y, 20.0], "normal": [0.0, 0.0, 1.0] }),
    );
    assert!(em.triangles > 12, "text was merged into the mesh");
    assert_eq!(em.cutouts, 0);
    assert_near(em.bounds.max.z, 21.0, 0.01, "raised by the label depth");
    assert_near(em.bounds.min.z, 0.0, 1e-4, "still on the plate");
    assert!(em.bounds.max.x <= a.bounds.max.x + 1e-3, "text fits on the face");

    let en: ObjectInfo = app.call(
        "add_label",
        json!({ "id": b.id, "text": "OR", "options": opts("engrave"), "position": [cb.x, cb.y, 20.0], "normal": [0.0, 0.0, 1.0] }),
    );
    assert_eq!(en.cutouts, 1, "engraved text is a cutout");
    assert_eq!(en.triangles, 12, "the model mesh is unchanged");
    assert_near(en.bounds.max.z, 20.0, 1e-3, "no raised parts");

    let sum = app.slice(Some("goo"));
    assert_eq!(sum.layer_count, 210, "embossed text adds 1 mm");
    // above the cubes only the embossed letters are lit
    let letters = app.layer_lit(205) as f64;
    assert!(
        letters > area_px(PRINTER, 5.0) && letters < area_px(PRINTER, 72.0),
        "letter area {letters}"
    );
    // just below the top the engraved letters are missing from cube b
    let below_top = app.layer_lit(195) as f64;
    let two_cubes = area_px(PRINTER, 800.0);
    assert_rel(
        two_cubes - below_top,
        letters,
        0.2,
        "engraved area matches the embossed area",
    );
    // deeper than the engraving both cubes are solid
    assert_rel(app.layer_lit(150) as f64, two_cubes, 0.02, "below the engraving");

    let e = app.fail(
        "add_label",
        json!({ "id": a.id, "text": "   ", "options": opts("emboss"), "position": [0.0, 0.0, 20.0], "normal": [0.0, 0.0, 1.0] }),
    );
    assert!(e.contains("enter some text"), "{e}");
    let e = app.fail(
        "add_label",
        json!({ "id": 999, "text": "X", "options": opts("emboss"), "position": [0.0, 0.0, 20.0], "normal": [0.0, 0.0, 1.0] }),
    );
    assert!(e.contains("not found"), "{e}");
    // undo removes the engraving again
    app.run("undo", json!({}));
    assert_eq!(app.object(b.id).cutouts, 0);
}

#[test]
fn platform_replaces_supports_with_a_pad() {
    let app = TestApp::new();
    app.use_printer(PRINTER, 0.1, 1);
    let o = app.add_box("cube", Vec3::splat(10.0));
    app.run("auto_supports", json!({ "ids": [o.id], "settings": {} }));
    let p: ObjectInfo = app.call(
        "add_platform",
        json!({ "id": o.id, "options": { "margin": 2.0, "thickness": 1.5, "chamfer": 1.0 } }),
    );
    assert_eq!(p.support_points, 0);
    assert!(p.triangles > 12);
    assert_near(p.bounds.min.z, 0.0, 1e-4, "pad on the plate");
    assert_near(p.bounds.max.z, 11.3, 1e-3, "model sits 0.2 mm into the pad");
    assert_vec_near(
        size(&p.bounds) * Vec3::new(1.0, 1.0, 0.0),
        Vec3::new(16.0, 16.0, 0.0),
        0.05,
        "pad outline",
    );
    assert!(
        p.volume_mm3 > 1000.0 + 14.0 * 14.0 * 1.3,
        "pad adds material ({})",
        p.volume_mm3
    );

    app.slice(Some("goo"));
    // the first layer is the wide pad bottom, higher up only the cube
    let first = app.layer_lit(0) as f64;
    assert!(
        first > area_px(PRINTER, 14.0 * 14.0) && first <= area_px(PRINTER, 16.5 * 16.5),
        "pad {first}"
    );
    assert_rel(app.layer_lit(60) as f64, area_px(PRINTER, 100.0), 0.03, "cube section");
    assert!(app
        .fail("add_platform", json!({ "id": 999, "options": {} }))
        .contains("not found"));

    // an impossible pad is refused without touching the model or its supports
    let q = app.add_box("other", Vec3::splat(8.0));
    app.run("auto_supports", json!({ "ids": [q.id], "settings": {} }));
    let before = app.objects();
    let hist = app.history();
    let e = app.fail("add_platform", json!({ "id": q.id, "options": { "thickness": 0.0 } }));
    assert!(e.contains("could not build a platform"), "{e}");
    assert_eq!(app.objects(), before);
    assert!(app.object(q.id).support_points > 0, "supports kept");
    assert_eq!(app.history(), hist);
    // undo after a successful platform restores model and supports
    app.run("add_platform", json!({ "id": q.id, "options": {} }));
    let r: Vec<ObjectInfo> = app.call("undo", json!({}));
    assert_eq!(r, before);
    assert!(Mesh::default().is_empty());
}
