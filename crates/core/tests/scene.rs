//! Build plate scene: placement, arrangement, bounds checks, subtracted volumes
//! and snapshot semantics.

mod common;

use std::sync::Arc;

use common::*;
use openresin_core::geometry::Aabb;
use openresin_core::mesh::Mesh;
use openresin_core::primitives::{cuboid, frustum, sphere};
use openresin_core::profile::PrinterProfile;
use openresin_core::scene::{ObjectRole, Scene, SceneObject, Transform};
use openresin_core::slicer::Slicer;
use openresin_core::support::{SupportPoint, SupportSize};
use openresin_core::{Quat, Vec3};

fn overlap_3d(a: &Aabb, b: &Aabb) -> bool {
    overlap_xy(a, b) && a.min.z < b.max.z - 1e-3 && a.max.z > b.min.z + 1e-3
}

fn overlap_xy(a: &Aabb, b: &Aabb) -> bool {
    a.min.x < b.max.x - 1e-3 && a.max.x > b.min.x + 1e-3 && a.min.y < b.max.y - 1e-3 && a.max.y > b.min.y + 1e-3
}

/// Horizontal gap between two footprints (0 when they overlap in x or y projection only).
fn gap_xy(a: &Aabb, b: &Aabb) -> f32 {
    let gx = (b.min.x - a.max.x).max(a.min.x - b.max.x);
    let gy = (b.min.y - a.max.y).max(a.min.y - b.max.y);
    gx.max(gy)
}

#[test]
fn add_recentres_and_drops_objects_onto_the_plate() {
    let mut s = Scene::default();
    let id = s.add("far", cuboid(Vec3::new(100.0, -50.0, 40.0), Vec3::new(10.0, 6.0, 4.0)));
    let o = s.get(id).unwrap();
    assert_eq!(o.name, "far");
    assert_eq!(o.role, ObjectRole::Solid);
    // the local mesh is centred on its bounding box
    let lb = o.mesh.bounds();
    assert!(lb.center().length() < 1e-5, "{lb:?}");
    let wb = o.world_bounds();
    assert!(wb.min.z.abs() < 1e-5 && (wb.max.z - 4.0).abs() < 1e-5);
    assert!(wb.center().truncate().length() < 1e-5);
    assert_eq!(o.elevation(), wb.min.z);
    // a rotated mesh is dropped by its lowest point
    let id2 = s.add("ball", sphere(Vec3::new(0.0, 0.0, -30.0), 5.0, 24, 12));
    assert_ne!(id, id2);
    assert!(s.get(id2).unwrap().world_bounds().min.z.abs() < 1e-5);
    // explicit elevation
    s.drop_to(id2, 7.5);
    assert!((s.get(id2).unwrap().elevation() - 7.5).abs() < 1e-5);
    // world mesh equals the transformed local mesh
    let w = s.get(id2).unwrap().world_mesh();
    assert_close(
        w.signed_volume(),
        s.get(id2).unwrap().mesh.signed_volume(),
        1e-3,
        "volume",
    );
    assert!((w.bounds().min.z - 7.5).abs() < 1e-5);
}

#[test]
fn ids_insert_get_remove_and_duplicate() {
    let mut s = Scene::default();
    let a = s.add("a", cuboid(Vec3::ZERO, Vec3::new(10.0, 5.0, 5.0)));
    let b = s.add("b", cuboid(Vec3::ZERO, Vec3::splat(3.0)));
    assert_eq!((a, b), (1, 2));
    // duplicates share the mesh, copy supports and sit next to the original
    s.get_mut(a).unwrap().supports.push(SupportPoint {
        position: Vec3::new(1.0, 1.0, -2.5),
        normal: -Vec3::Z,
        size: SupportSize::Heavy,
    });
    let d = s.duplicate(a).unwrap();
    assert_eq!(d, 3);
    let (oa, od) = (s.get(a).unwrap(), s.get(d).unwrap());
    assert_eq!(od.name, "a copy");
    assert!(Arc::ptr_eq(&oa.mesh, &od.mesh));
    assert_eq!(od.supports, oa.supports);
    assert!((od.transform.translation.x - oa.transform.translation.x - 12.0).abs() < 1e-5);
    assert!(gap_xy(&oa.world_bounds(), &od.world_bounds()) >= 2.0 - 1e-4);
    assert!(s.duplicate(99).is_none());
    // explicit ids from project files bump the counter
    let mut obj = s.get(b).unwrap().clone();
    obj.id = 42;
    s.insert(obj);
    assert_eq!(s.add("next", cuboid(Vec3::ZERO, Vec3::ONE)), 43);
    // remove
    let removed = s.remove(b).unwrap();
    assert_eq!(removed.name, "b");
    assert!(s.get(b).is_none() && s.remove(b).is_none());
    assert_eq!(s.objects.len(), 4);
}

#[test]
fn out_of_bounds_detection() {
    let p = PrinterProfile::generic(); // 192 x 120 x 200
    let mut s = Scene::default();
    let ok = s.add("ok", cuboid(Vec3::ZERO, Vec3::splat(20.0)));
    let right = s.add("right", cuboid(Vec3::ZERO, Vec3::splat(20.0)));
    s.get_mut(right).unwrap().transform.translation.x = 90.0; // reaches x = 100 > 96
    let back = s.add("back", cuboid(Vec3::ZERO, Vec3::splat(20.0)));
    s.get_mut(back).unwrap().transform.translation.y = -55.0; // reaches y = -65 < -60
    let tall = s.add("tall", cuboid(Vec3::ZERO, Vec3::new(10.0, 10.0, 201.0)));
    let sunk = s.add("sunk", cuboid(Vec3::ZERO, Vec3::splat(5.0)));
    s.get_mut(sunk).unwrap().transform.translation.z -= 0.5;
    let edge = s.add("edge", cuboid(Vec3::ZERO, Vec3::new(192.0, 120.0, 200.0)));
    let cutter = s.add("cutter", cuboid(Vec3::ZERO, Vec3::splat(20.0)));
    {
        let c = s.get_mut(cutter).unwrap();
        c.role = ObjectRole::Subtract;
        c.transform.translation.x = 500.0;
    }
    let mut oob = s.out_of_bounds(&p);
    oob.sort_unstable();
    assert_eq!(oob, vec![right, back, tall, sunk], "ok={ok} edge={edge}");
}

#[test]
fn arrange_packs_without_overlap_inside_the_plate() {
    let p = PrinterProfile::generic();
    let mut s = Scene::default();
    let sizes = [
        Vec3::new(30.0, 20.0, 10.0),
        Vec3::new(12.0, 40.0, 5.0),
        Vec3::new(25.0, 25.0, 30.0),
        Vec3::new(8.0, 8.0, 8.0),
        Vec3::new(50.0, 10.0, 3.0),
        Vec3::new(18.0, 33.0, 12.0),
        Vec3::new(5.0, 5.0, 50.0),
    ];
    for (i, sz) in sizes.iter().enumerate() {
        let id = s.add(format!("p{i}"), cuboid(Vec3::ZERO, *sz));
        // rotated objects are packed by their world footprint
        if i == 2 {
            s.get_mut(id).unwrap().transform.rotation = Quat::from_rotation_z(0.6);
            s.drop_to(id, 0.0);
        }
        // a lifted object keeps its elevation
        if i == 3 {
            s.drop_to(id, 4.0);
        }
    }
    let spacing = 3.0;
    assert!(s.arrange(&p, spacing));
    assert!(s.out_of_bounds(&p).is_empty());
    let bounds: Vec<Aabb> = s.objects.iter().map(|o| o.world_bounds()).collect();
    for i in 0..bounds.len() {
        for j in i + 1..bounds.len() {
            assert!(!overlap_xy(&bounds[i], &bounds[j]), "{i} and {j} overlap");
            assert!(
                gap_xy(&bounds[i], &bounds[j]) >= spacing - 1e-3,
                "{i} and {j} closer than the spacing"
            );
        }
    }
    // the packed block is centred on the plate
    let all = s.bounds();
    assert!(all.center().truncate().length() < 1e-3, "{all:?}");
    assert!((s.objects[3].elevation() - 4.0).abs() < 1e-5);
    assert!(s
        .objects
        .iter()
        .enumerate()
        .all(|(i, o)| i == 3 || o.elevation().abs() < 1e-4));
}

#[test]
fn arrange_reports_when_the_plate_is_full() {
    let p = PrinterProfile::generic();
    let mut s = Scene::default();
    for i in 0..40 {
        s.add(format!("c{i}"), cuboid(Vec3::ZERO, Vec3::splat(25.0)));
    }
    // 6 per row (6 * 28 - 3 = 165 <= 192), 4 rows (4 * 28 - 3 = 109 <= 120): at most 24 fit
    assert!(!s.arrange(&p, 3.0));
    let mut s = Scene::default();
    for i in 0..24 {
        s.add(format!("c{i}"), cuboid(Vec3::ZERO, Vec3::splat(25.0)));
    }
    assert!(s.arrange(&p, 3.0));
    assert!(s.out_of_bounds(&p).is_empty());
    // a single object larger than the plate never fits
    let mut big = Scene::default();
    big.add("big", cuboid(Vec3::ZERO, Vec3::new(200.0, 10.0, 10.0)));
    assert!(!big.arrange(&p, 3.0));
    // empty scenes trivially fit
    assert!(Scene::default().arrange(&p, 3.0));
}

#[test]
fn arrange_keeps_cutters_attached_to_their_part() {
    let p = PrinterProfile::generic();
    let mut s = Scene::default();
    let a = s.add("a", cuboid(Vec3::ZERO, Vec3::splat(20.0)));
    let b = s.add("b", cuboid(Vec3::ZERO, Vec3::new(30.0, 10.0, 10.0)));
    assert!(s.arrange(&p, 3.0));
    // a cutter notching the +x/top edge of `a`
    let ca = s.get(a).unwrap().world_bounds();
    let cutter = s.add("cutter", cuboid(Vec3::ZERO, Vec3::splat(8.0)));
    {
        let c = s.get_mut(cutter).unwrap();
        c.role = ObjectRole::Subtract;
        c.transform.translation = Vec3::new(ca.max.x, ca.center().y, ca.max.z);
    }
    let rel = s.get(cutter).unwrap().transform.translation - s.get(a).unwrap().transform.translation;
    // adding another part re-arranges the plate (like the app does)
    s.add("c", cuboid(Vec3::ZERO, Vec3::new(40.0, 40.0, 5.0)));
    assert!(s.arrange(&p, 3.0));
    let moved = s.get(a).unwrap().transform.translation - ca.center() * Vec3::new(1.0, 1.0, 0.0);
    assert!(
        moved.truncate().length() > 1.0,
        "the part should have moved for this test"
    );
    let rel_after = s.get(cutter).unwrap().transform.translation - s.get(a).unwrap().transform.translation;
    assert!(
        (rel_after - rel).length() < 1e-4,
        "cutter left behind: {rel:?} -> {rel_after:?}"
    );
    // and it still removes material from `a` only
    let (pos, neg) = s.slice_volumes();
    assert_eq!((pos.len(), neg.len()), (3, 1));
    let nb = neg[0].bounds();
    let ab = s.get(a).unwrap().world_bounds();
    let bb = s.get(b).unwrap().world_bounds();
    assert!(overlap_3d(&nb, &ab) && !overlap_3d(&nb, &bb));
}

#[test]
fn slice_volumes_collects_supports_cutters_and_cutouts() {
    let mut s = Scene::default();
    let part = s.add("part", cuboid(Vec3::ZERO, Vec3::new(20.0, 20.0, 10.0)));
    // a cavity in local coordinates; the part is moved and rotated afterwards
    let cavity = Arc::new(cuboid(Vec3::new(0.0, 0.0, 0.0), Vec3::new(10.0, 10.0, 4.0)));
    {
        let o = s.get_mut(part).unwrap();
        o.cutouts.push(cavity.clone());
        o.transform.translation += Vec3::new(5.0, -3.0, 0.0);
        o.transform.rotation = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
    }
    let cutter = s.add("cutter", frustum(Vec3::ZERO, Vec3::new(0.0, 0.0, 30.0), 2.0, 2.0, 64));
    {
        let c = s.get_mut(cutter).unwrap();
        c.role = ObjectRole::Subtract;
        c.transform.translation.x = 5.0 + 7.5;
        c.transform.translation.y = -3.0;
    }
    let (pos, neg) = s.slice_volumes();
    assert_eq!(pos.len(), 1, "no supports yet");
    assert_eq!(neg.len(), 2);
    assert_eq!(s.slice_meshes().len(), 1);
    // the cutout moved with its object
    let cb = neg.iter().map(|m| m.bounds()).find(|b| b.size().z < 5.0).unwrap();
    assert!((cb.center() - Vec3::new(5.0, -3.0, 5.0)).length() < 1e-4, "{cb:?}");
    // sliced: 400 - cavity 100 - cutter circle at the middle layer, 400 - circle at the bottom
    let o = raster(400, 400, 80.0, 80.0, 4);
    let posr: Vec<&Mesh> = pos.iter().collect();
    let negr: Vec<&Mesh> = neg.iter().collect();
    let sl = Slicer::with_negatives(&posr, &negr, 0.5, o);
    assert_eq!(sl.layer_count(), 20);
    let circle = ngon_area(64, 2.0);
    assert_close(
        lit_mm2(&sl.rasterize(10), &o),
        400.0 - 100.0 - circle,
        0.3,
        "middle layer",
    );
    assert_close(lit_mm2(&sl.rasterize(1), &o), 400.0 - circle, 0.3, "bottom layer");
    // supports join the positive volumes
    s.get_mut(part).unwrap().transform.translation.z = 10.0;
    let pts = vec![SupportPoint {
        position: Vec3::new(5.0, -3.0, 5.0),
        normal: -Vec3::Z,
        size: SupportSize::Medium,
    }];
    s.get_mut(part).unwrap().set_world_support_points(&pts);
    let (pos, _) = s.slice_volumes();
    assert_eq!(pos.len(), 2);
    assert!(pos[1].bounds().min.z <= 1e-4);
    // removing the support points removes the support volume again
    s.get_mut(part).unwrap().supports.clear();
    assert_eq!(s.slice_volumes().0.len(), 1);
}

#[test]
fn fill_plate_with_copies() {
    // what the app's "fill plate" does: duplicate and re-arrange until it no longer fits
    let p = PrinterProfile::generic();
    let mut s = Scene::default();
    let id = s.add("cube", cuboid(Vec3::ZERO, Vec3::splat(25.0)));
    assert!(s.arrange(&p, 3.0));
    loop {
        let copy = s.duplicate(id).unwrap();
        if !s.arrange(&p, 3.0) {
            s.remove(copy);
            assert!(s.arrange(&p, 3.0));
            break;
        }
        assert!(s.objects.len() < 100, "never full");
    }
    // 6 per row (6 * 28 - 3 = 165 <= 192) times 4 rows (4 * 28 - 3 = 109 <= 120)
    assert_eq!(s.objects.len(), 24);
    assert!(s.out_of_bounds(&p).is_empty());
    let bounds: Vec<Aabb> = s.objects.iter().map(|o| o.world_bounds()).collect();
    for i in 0..bounds.len() {
        for j in i + 1..bounds.len() {
            assert!(gap_xy(&bounds[i], &bounds[j]) >= 3.0 - 1e-3);
        }
    }
    assert!(s
        .objects
        .iter()
        .skip(1)
        .all(|o| o.name.ends_with("copy") && Arc::ptr_eq(&o.mesh, &s.objects[0].mesh)));
}

#[test]
fn support_points_roundtrip_between_local_and_world() {
    let mut obj = SceneObject {
        id: 1,
        name: "x".into(),
        mesh: Arc::new(cuboid(Vec3::ZERO, Vec3::new(4.0, 6.0, 8.0))),
        transform: Transform {
            translation: Vec3::new(10.0, -4.0, 7.0),
            rotation: Quat::from_euler(glam::EulerRot::ZXY, 0.3, -0.4, 1.1),
            scale: Vec3::new(2.0, 1.0, 0.5),
        },
        supports: Vec::new(),
        support_settings: Default::default(),
        role: ObjectRole::Solid,
        cutouts: Vec::new(),
    };
    // local contact on the sloped plane x + z = 1 (normal (1,0,1)/sqrt2)
    let local_n = Vec3::new(1.0, 0.0, 1.0).normalize();
    obj.supports.push(SupportPoint {
        position: Vec3::new(0.5, 0.25, 0.5),
        normal: local_n,
        size: SupportSize::Light,
    });
    obj.supports.push(SupportPoint {
        position: Vec3::new(0.0, 0.0, 4.0),
        normal: Vec3::Z,
        size: SupportSize::Heavy,
    });
    let m = obj.transform.matrix();
    let world = obj.world_support_points();
    assert!((world[0].position - m.transform_point3(Vec3::new(0.5, 0.25, 0.5))).length() < 1e-5);
    // normals transform with the inverse transpose: scaled by (2, 1, 0.5) the plane
    // x + z = 1 becomes x/2 + 2z = 1 with normal (1/2, 0, 2)
    let expect = (obj.transform.rotation * Vec3::new(0.5, 0.0, 2.0)).normalize();
    assert!(
        (world[0].normal - expect).length() < 1e-5,
        "{} vs {expect}",
        world[0].normal
    );
    assert!((world[1].normal - obj.transform.rotation * Vec3::Z).length() < 1e-5);
    assert_eq!(world[1].size, SupportSize::Heavy);
    // and back
    let original = obj.supports.clone();
    obj.set_world_support_points(&world);
    for (a, b) in obj.supports.iter().zip(&original) {
        assert!((a.position - b.position).length() < 1e-4);
        assert!((a.normal - b.normal).length() < 1e-5, "{} vs {}", a.normal, b.normal);
        assert_eq!(a.size, b.size);
    }
}

#[test]
fn clones_are_independent_undo_snapshots() {
    let mut s = Scene::default();
    let a = s.add("a", cuboid(Vec3::ZERO, Vec3::splat(10.0)));
    let snapshot = s.clone();
    // cheap: meshes are shared, not copied
    assert!(Arc::ptr_eq(&s.objects[0].mesh, &snapshot.objects[0].mesh));
    s.get_mut(a).unwrap().transform.translation.x = 33.0;
    s.get_mut(a).unwrap().name = "renamed".into();
    s.get_mut(a).unwrap().mesh = Arc::new(sphere(Vec3::ZERO, 1.0, 8, 4));
    let b = s.add("b", cuboid(Vec3::ZERO, Vec3::ONE));
    let o = snapshot.get(a).unwrap();
    assert_eq!(o.transform.translation.x, 0.0);
    assert_eq!(o.name, "a");
    assert_eq!(o.mesh.triangle_count(), 12);
    assert!(snapshot.get(b).is_none());
    // restoring the snapshot (undo) also restores the id counter
    let mut restored = snapshot.clone();
    assert_eq!(restored.add("again", cuboid(Vec3::ZERO, Vec3::ONE)), b);
}

#[test]
fn empty_scene_and_empty_meshes_are_harmless() {
    let p = PrinterProfile::generic();
    let mut s = Scene::default();
    assert!(s.bounds().is_empty());
    assert!(s.out_of_bounds(&p).is_empty());
    let (pos, neg) = s.slice_volumes();
    assert!(pos.is_empty() && neg.is_empty());
    // an empty mesh must not poison the transform with NaN/inf
    let id = s.add("nothing", Mesh::default());
    let o = s.get(id).unwrap();
    assert!(o.transform.translation.is_finite(), "{:?}", o.transform);
    s.drop_to(id, 0.0);
    assert!(s.get(id).unwrap().transform.translation.is_finite());
    let d = s.duplicate(id).unwrap();
    assert!(s.get(d).unwrap().transform.translation.is_finite());
    assert!(s.arrange(&p, 3.0));
    assert!(s.objects.iter().all(|o| o.transform.translation.is_finite()));
    assert!(s.get(id).unwrap().support_mesh().mesh.is_empty());
}
