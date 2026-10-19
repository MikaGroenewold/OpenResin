//! Project files, print estimates and printer/print profiles.

mod common;

use std::collections::HashSet;
use std::io::{Cursor, Write};
use std::sync::Arc;

use common::*;
use openresin_core::estimate::{estimate, layer_time_s};
use openresin_core::mesh::Mesh;
use openresin_core::primitives::{cuboid, sphere};
use openresin_core::profile::{builtin_printers, find_printer, MirrorMode, Motion, PrintSettings, PrinterProfile};
use openresin_core::project::{self, PROJECT_EXTENSION};
use openresin_core::scene::{ObjectRole, Scene};
use openresin_core::slicer::Slicer;
use openresin_core::support::{SupportPoint, SupportSettings, SupportSize};
use openresin_core::{Error, Quat, Vec3};

// ---------------------------------------------------------------- project

fn sample_scene() -> (Scene, PrintSettings) {
    let mut s = Scene::default();
    let a = s.add("Part A", sphere(Vec3::ZERO, 8.0, 32, 16));
    {
        let o = s.get_mut(a).unwrap();
        o.transform.translation = Vec3::new(-20.0, 5.0, 6.0);
        o.transform.rotation = Quat::from_euler(glam::EulerRot::XYZ, 0.3, -0.2, 1.0);
        o.transform.scale = Vec3::new(1.0, 1.5, 0.75);
        o.supports = vec![
            SupportPoint {
                position: Vec3::new(0.0, 0.0, -8.0),
                normal: -Vec3::Z,
                size: SupportSize::Heavy,
            },
            SupportPoint {
                position: Vec3::new(3.0, 1.0, -7.0),
                normal: Vec3::new(0.3, 0.1, -0.9).normalize(),
                size: SupportSize::Light,
            },
        ];
        o.support_settings = SupportSettings {
            lift_height: 7.0,
            raft: false,
            cross_bracing: true,
            spacing: 2.25,
            tip_diameter: [0.2, 0.4, 0.6],
            ..Default::default()
        };
        o.cutouts.push(Arc::new(sphere(Vec3::ZERO, 5.0, 16, 8)));
        o.cutouts
            .push(Arc::new(cuboid(Vec3::new(0.0, 0.0, -7.0), Vec3::splat(2.0))));
    }
    let b = s.add("Cutter ✂", cuboid(Vec3::ZERO, Vec3::new(4.0, 30.0, 4.0)));
    {
        let o = s.get_mut(b).unwrap();
        o.role = ObjectRole::Subtract;
        o.transform.translation = Vec3::new(-20.0, 5.0, 6.0);
    }
    s.add("Part C", cuboid(Vec3::ZERO, Vec3::new(10.0, 20.0, 30.0)));
    let settings = PrintSettings {
        name: "Tough grey".into(),
        layer_height: 0.03,
        bottom_layers: 6,
        transition_layers: 3,
        anti_aliasing: 8,
        wait_mode: false,
        resin_density: 1.18,
        resin_price_per_liter: 42.5,
        currency: "$".into(),
        ..Default::default()
    };
    (s, settings)
}

#[test]
fn project_roundtrip_preserves_everything() {
    let (scene, settings) = sample_scene();
    let f = TempFile::new("roundtrip", PROJECT_EXTENSION);
    project::save(f.path(), &scene, Some("elegoo-saturn-4-ultra"), Some(&settings)).unwrap();
    let p = project::load(f.path()).unwrap();
    assert_eq!(p.printer_id.as_deref(), Some("elegoo-saturn-4-ultra"));
    assert_eq!(p.settings.as_ref(), Some(&settings));
    assert_eq!(p.scene.objects.len(), scene.objects.len());
    for (a, b) in scene.objects.iter().zip(&p.scene.objects) {
        assert_eq!(a.id, b.id);
        assert_eq!(a.name, b.name);
        assert_eq!(a.transform, b.transform);
        assert_eq!(a.supports, b.supports);
        assert_eq!(a.support_settings, b.support_settings);
        assert_eq!(a.role, b.role);
        assert_eq!(canonical_triangles(&a.mesh), canonical_triangles(&b.mesh));
        assert_eq!(a.cutouts.len(), b.cutouts.len());
        for (ca, cb) in a.cutouts.iter().zip(&b.cutouts) {
            assert_eq!(canonical_triangles(ca), canonical_triangles(cb));
        }
        assert_eq!(a.world_bounds(), b.world_bounds());
    }
    // ids continue after the loaded ones
    let mut loaded = p.scene;
    assert_eq!(loaded.add("new", cuboid(Vec3::ZERO, Vec3::ONE)), 4);
    loaded.remove(4);
    // the loaded scene slices exactly like the original
    let o = raster(300, 300, 90.0, 90.0, 4);
    let slice = |s: &Scene| {
        let (pos, neg) = s.slice_volumes();
        let pr: Vec<&Mesh> = pos.iter().collect();
        let nr: Vec<&Mesh> = neg.iter().collect();
        let sl = Slicer::with_negatives(&pr, &nr, 0.5, o);
        (0..sl.layer_count())
            .step_by(5)
            .map(|i| sl.rasterize(i).to_gray())
            .collect::<Vec<_>>()
    };
    assert_eq!(slice(&scene), slice(&loaded));
}

#[test]
fn project_without_optional_parts_and_empty_scene() {
    let mut buf = Cursor::new(Vec::new());
    project::write(&mut buf, &Scene::default(), None, None).unwrap();
    buf.set_position(0);
    let p = project::read(buf).unwrap();
    assert!(p.scene.objects.is_empty() && p.printer_id.is_none() && p.settings.is_none());
    // an object whose mesh is empty survives the round trip
    let mut s = Scene::default();
    s.add("nothing", Mesh::default());
    let mut buf = Cursor::new(Vec::new());
    project::write(&mut buf, &s, None, None).unwrap();
    buf.set_position(0);
    let p = project::read(buf).unwrap();
    assert_eq!(p.scene.objects.len(), 1);
    assert!(p.scene.objects[0].mesh.is_empty());
}

fn zip_files(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut w = zip::ZipWriter::new(Cursor::new(&mut buf));
        for (name, content) in files {
            w.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(content).unwrap();
        }
        w.finish().unwrap();
    }
    buf
}

#[test]
fn old_and_broken_project_files() {
    let mut stl = Vec::new();
    openresin_core::io::write_binary_stl(&cuboid(Vec3::ZERO, Vec3::splat(2.0)), &mut stl).unwrap();
    // version 1 files written before roles and cutouts existed
    let old = r#"{"format_version":1,"printer_id":null,"settings":null,"objects":[{"id":7,"name":"old",
        "mesh":"meshes/7.stl","transform":{"translation":[1,2,3],"rotation":[0,0,0,1],"scale":[1,1,1]},
        "supports":[],"support_settings":{"lift_height":3.0}}]}"#;
    let data = zip_files(&[("project.json", old.as_bytes()), ("meshes/7.stl", &stl)]);
    let p = project::read(Cursor::new(&data)).unwrap();
    let o = p.scene.get(7).unwrap();
    assert_eq!(o.role, ObjectRole::Solid);
    assert!(o.cutouts.is_empty());
    assert_eq!(o.support_settings.lift_height, 3.0);
    assert_eq!(o.support_settings.spacing, SupportSettings::default().spacing);
    assert_eq!(o.transform.translation, Vec3::new(1.0, 2.0, 3.0));
    assert_close(o.mesh.signed_volume(), 8.0, 1e-5, "mesh");

    let newer = old.replace("\"format_version\":1", "\"format_version\":99");
    let data = zip_files(&[("project.json", newer.as_bytes()), ("meshes/7.stl", &stl)]);
    assert!(matches!(project::read(Cursor::new(&data)), Err(Error::Invalid(_))));
    let f = TempFile::with_bytes("newer", PROJECT_EXTENSION, &data);
    assert!(matches!(project::load(f.path()), Err(Error::Parse { .. })));

    let missing_mesh = zip_files(&[("project.json", old.as_bytes())]);
    assert!(matches!(project::read(Cursor::new(&missing_mesh)), Err(Error::Zip(_))));
    let bad_json = zip_files(&[("project.json", b"{ not json")]);
    assert!(matches!(project::read(Cursor::new(&bad_json)), Err(Error::Json(_))));
    let bad_mesh = zip_files(&[("project.json", old.as_bytes()), ("meshes/7.stl", b"garbage")]);
    assert!(project::read(Cursor::new(&bad_mesh)).is_err());
    assert!(project::read(Cursor::new(b"not a zip".to_vec())).is_err());
    let missing = TempFile::new("missing", PROJECT_EXTENSION);
    assert!(matches!(project::load(missing.path()), Err(Error::Io(_))));
}

// ---------------------------------------------------------------- estimate

#[test]
fn estimate_of_a_sliced_cube_matches_hand_calculation() {
    let s = PrintSettings::default();
    assert!(s.validate().is_ok());
    // 10 mm cube, 0.05 mm layers -> 200 layers of 100 mm^2
    let cube = cuboid(Vec3::new(0.0, 0.0, 5.0), Vec3::splat(10.0));
    let o = raster(400, 400, 40.0, 40.0, 4);
    let sl = Slicer::new(&[&cube], s.layer_height, o);
    assert_eq!(sl.layer_count(), 200);
    let areas: Vec<f64> = (0..200).map(|i| lit_mm2(&sl.rasterize(i), &o)).collect();
    assert!(areas.iter().all(|a| (a - 100.0).abs() < 1e-4), "{:?}", &areas[..3]);
    let e = estimate(&s, &areas);
    assert_eq!(e.layer_count, 200);
    assert_close(e.height_mm, 10.0, 1e-5, "height");
    assert_close(e.volume_mm3, 1000.0, 1e-3, "volume");
    assert_close(e.volume_ml(), 1.0, 1e-6, "ml");
    assert_close(e.weight_g, 1.1, 1e-5, "weight: 1 ml * 1.1 g/ml");
    assert_close(e.cost, 0.03, 1e-7, "cost: 1 ml at 30 per litre");
    assert_eq!(e.currency, "€");
    // bottom: 30 s + 0.5 s rest + 6 mm at 60 mm/min (6 s) + 6 mm at 150 mm/min (2.4 s)
    let bottom = 30.0 + 0.5 + 6.0 + 2.4;
    // normal: 2.5 s + 0.5 s + 6 mm at 65 mm/min + 2.4 s
    let normal = 2.5 + 0.5 + 6.0 / 65.0 * 60.0 + 2.4;
    assert_close(layer_time_s(&s, 0) as f64, bottom, 1e-4, "bottom layer time");
    assert_close(layer_time_s(&s, 10) as f64, normal, 1e-4, "normal layer time");
    assert_close(e.print_time_s, 4.0 * bottom + 196.0 * normal, 1e-2, "print time");
}

#[test]
fn estimate_with_transitions_light_off_delay_and_two_stage_moves() {
    let mut s = PrintSettings {
        bottom_layers: 2,
        transition_layers: 3,
        wait_mode: false,
        layer_height: 0.1,
        resin_density: 1.25,
        resin_price_per_liter: 50.0,
        ..Default::default()
    };
    s.bottom.exposure = 20.0;
    s.normal.exposure = 4.0;
    s.bottom.light_off_delay = 1.0;
    s.normal.light_off_delay = 0.25;
    s.normal.lift = Motion {
        distance: 2.0,
        speed: 60.0,
        distance2: 4.0,
        speed2: 240.0,
    };
    s.normal.retract = Motion::new(6.0, 360.0);
    assert_eq!(s.normal.lift.total_distance(), 6.0);
    assert_close(s.normal.lift.duration_s() as f64, 2.0 + 1.0, 1e-6, "two stage lift");
    // exposure: 20, 20, then 16, 12, 8 (transition), then 4
    let exp: Vec<f32> = (0..7).map(|i| s.exposure_for_layer(i)).collect();
    assert_eq!(exp, vec![20.0, 20.0, 16.0, 12.0, 8.0, 4.0, 4.0]);
    let bottom_motion = 6.0 / 60.0 * 60.0 + 6.0 / 150.0 * 60.0;
    let normal_motion = 3.0 + 1.0;
    let mut expect = 0.0;
    for (i, e) in exp.iter().enumerate() {
        let (delay, motion) = if i < 2 {
            (1.0, bottom_motion)
        } else {
            (0.25, normal_motion)
        };
        let t = *e as f64 + delay + motion;
        assert_close(layer_time_s(&s, i as u32) as f64, t, 1e-4, &format!("layer {i}"));
        expect += t;
    }
    let areas = [50.0f64; 7];
    let e = estimate(&s, &areas);
    assert_close(e.print_time_s, expect, 1e-3, "total");
    assert_close(e.volume_mm3, 35.0, 1e-6, "volume");
    assert_close(e.weight_g, 0.035 * 1.25, 1e-8, "weight");
    assert_close(e.cost, 0.035 / 1000.0 * 50.0, 1e-9, "cost");
    // nothing to print
    let none = estimate(&s, &[]);
    assert_eq!(
        (none.layer_count, none.print_time_s, none.volume_mm3, none.cost),
        (0, 0.0, 0.0, 0.0)
    );
    // a zero-speed move takes no time instead of infinite time
    s.normal.retract.speed = 0.0;
    assert!(layer_time_s(&s, 10).is_finite());
}

#[test]
fn print_settings_validation() {
    let ok = PrintSettings::default();
    assert!(ok.validate().is_ok());
    for bad in [
        PrintSettings {
            layer_height: 0.0,
            ..ok.clone()
        },
        PrintSettings {
            layer_height: 0.6,
            ..ok.clone()
        },
        PrintSettings {
            anti_aliasing: 3,
            ..ok.clone()
        },
        PrintSettings {
            anti_aliasing: 0,
            ..ok.clone()
        },
    ] {
        assert!(bad.validate().is_err(), "{bad:?}");
    }
    let mut no_exposure = ok.clone();
    no_exposure.normal.exposure = 0.0;
    assert!(no_exposure.validate().is_err());
    for aa in [1u8, 2, 4, 8, 16] {
        assert!(PrintSettings {
            anti_aliasing: aa,
            ..ok.clone()
        }
        .validate()
        .is_ok());
    }
    // serde round trip
    let json = serde_json::to_string(&ok).unwrap();
    assert_eq!(serde_json::from_str::<PrintSettings>(&json).unwrap(), ok);
}

// ---------------------------------------------------------------- printers

#[test]
fn builtin_printers_are_valid_and_findable() {
    let list = builtin_printers();
    assert!(list.len() > 100);
    assert_eq!(list[0], PrinterProfile::generic());
    let mut ids = HashSet::new();
    for p in &list {
        assert!(ids.insert(p.id.clone()), "duplicate id {}", p.id);
        assert!(
            !p.id.is_empty() && p.id == p.id.to_lowercase() && !p.id.contains(' '),
            "bad id {:?}",
            p.id
        );
        assert!(!p.name.is_empty() && !p.vendor.is_empty(), "{}", p.id);
        assert!(p.resolution_x > 0 && p.resolution_y > 0, "{}", p.id);
        for v in [p.size_x, p.size_y, p.size_z] {
            assert!(v.is_finite() && v > 0.0, "{}: size {v}", p.id);
        }
        let (px_um, py_um) = p.pixel_size_um();
        assert!(
            (5.0..=200.0).contains(&px_um) && (5.0..=200.0).contains(&py_um),
            "{}: pixel {px_um} x {py_um} um",
            p.id
        );
        assert!(!p.formats.is_empty(), "{}", p.id);
        assert!(
            p.formats.iter().all(|f| !f.is_empty() && *f == f.to_lowercase()),
            "{}",
            p.id
        );
        let o = p.raster_options(4);
        assert_eq!(
            (o.width, o.height, o.size_x, o.size_y),
            (p.resolution_x, p.resolution_y, p.size_x, p.size_y)
        );
        assert_eq!(o.mirror_y, matches!(p.mirror, MirrorMode::Lcd | MirrorMode::Y));
        assert_eq!(o.mirror_x, p.mirror == MirrorMode::X);
        assert!(!p.display_name().is_empty() && !p.machine_type().is_empty());
        // every printer is found by its id
        let found = find_printer(&list, &p.id).unwrap_or_else(|| panic!("{} not found", p.id));
        assert_eq!(found.id, p.id);
        // and by its (upper case) display name
        assert!(
            find_printer(&list, &p.display_name().to_uppercase()).is_some(),
            "{}",
            p.id
        );
    }
    assert!(find_printer(&list, "no such printer 123").is_none());
    let g = find_printer(&list, "GENERIC 4K").unwrap();
    assert_eq!(g.id, "generic-4k");
    let (gx, gy) = g.pixel_size_um();
    assert!((gx - 50.0).abs() < 1e-4 && (gy - 50.0).abs() < 1e-4, "{gx} {gy}");
    // serde round trip of the whole database
    let json = serde_json::to_string(&list).unwrap();
    let back: Vec<PrinterProfile> = serde_json::from_str(&json).unwrap();
    assert_eq!(back, list);
}

#[test]
fn printer_profile_helpers() {
    let mut p = PrinterProfile::generic();
    assert_eq!(p.display_name(), "Generic 4K");
    assert_eq!(p.machine_type(), "Generic 4K");
    p.name = "Mighty 8K".into();
    p.vendor = "Phrozen".into();
    assert_eq!(p.display_name(), "Phrozen Mighty 8K");
    p.machine_type = Some("PHROZEN-M8K".into());
    assert_eq!(p.machine_type(), "PHROZEN-M8K");
    // mirror defaults to the LCD mirror when missing in JSON
    let mut v = serde_json::to_value(PrinterProfile::generic()).unwrap();
    v.as_object_mut().unwrap().remove("mirror");
    let back: PrinterProfile = serde_json::from_value(v).unwrap();
    assert_eq!(back.mirror, MirrorMode::Lcd);
}
