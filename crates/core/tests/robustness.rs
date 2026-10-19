//! Degenerate, huge and empty inputs: no panics and sensible results.

mod common;

use common::*;
use glam::DVec2;
use openresin_core::cut::{cut, CutPlane};
use openresin_core::hollow::{hollow, HollowOptions};
use openresin_core::io::{parse_stl_bytes, write_binary_stl};
use openresin_core::mesh::Mesh;
use openresin_core::orient::{auto_orient, OrientOptions};
use openresin_core::primitives::{base_plate_under, cuboid, sphere, BasePlateOptions};
use openresin_core::profile::PrinterProfile;
use openresin_core::repair::{analyze, repair, RepairOptions};
use openresin_core::scene::Scene;
use openresin_core::slicer::Slicer;
use openresin_core::split::split_shells;
use openresin_core::support::{auto_points, build, SupportSettings};
use openresin_core::triangulate::{triangulate_loop_3d, triangulate_rings};
use openresin_core::{Quat, Vec3};

/// Only zero-area triangles: repeated vertices and collinear corners.
fn degenerate_mesh() -> Mesh {
    Mesh::new(
        vec![
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::new(1.0, 0.0, 2.0),
            Vec3::new(2.0, 0.0, 3.0),
            Vec3::new(5.0, 5.0, 5.0),
        ],
        vec![[0, 1, 2], [3, 3, 3], [0, 0, 1], [2, 1, 0]],
    )
}

#[test]
fn degenerate_triangles_everywhere() {
    let m = degenerate_mesh();
    let o = raster(64, 64, 64.0, 64.0, 4);
    let s = Slicer::new(&[&m], 0.1, o);
    assert_eq!(s.layer_count(), 50);
    for i in 0..s.layer_count() {
        assert!(s.rasterize(i).is_blank(), "layer {i}");
    }
    let r = analyze(&m);
    assert_eq!((r.degenerate_triangles, r.volume, r.area), (4, 0.0, 0.0));
    let (fixed, rep) = repair(&m, &RepairOptions::default());
    assert!(fixed.is_empty(), "{:?}", rep);
    assert!(split_shells(&fixed).is_empty());
    assert!(auto_points(&m, &SupportSettings::default()).is_empty());
    let mut c = m.clone();
    c.cleanup();
    assert!(c.is_empty() && c.positions.is_empty());
    let res = cut(&m, &CutPlane::horizontal(2.0)).unwrap();
    assert!((res.below.signed_volume() + res.above.signed_volume()).abs() < 1e-9);
    assert!(hollow(&m, &HollowOptions::default())
        .map(|c| c.is_empty())
        .unwrap_or(true));
    assert_eq!(auto_orient(&m, &OrientOptions::default()), Quat::IDENTITY);
    // the STL writer copes with zero normals
    let mut buf = Vec::new();
    write_binary_stl(&m, &mut buf).unwrap();
    assert_eq!(buf.len(), 84 + 4 * 50);
    assert!(parse_stl_bytes(&buf).unwrap().triangle_count() <= 2);
}

#[test]
fn flat_zero_volume_meshes() {
    // a single square lying on the plate and one floating in the air
    let quad = |z: f32| {
        Mesh::new(
            vec![
                Vec3::new(-5.0, -5.0, z),
                Vec3::new(5.0, -5.0, z),
                Vec3::new(5.0, 5.0, z),
                Vec3::new(-5.0, 5.0, z),
            ],
            vec![[0, 1, 2], [0, 2, 3]],
        )
    };
    let o = raster(64, 64, 64.0, 64.0, 4);
    assert_eq!(Slicer::new(&[&quad(0.0)], 0.1, o).layer_count(), 0);
    let s = Slicer::new(&[&quad(3.0)], 0.1, o);
    assert_eq!(s.layer_count(), 30);
    assert!((0..30).all(|i| s.rasterize(i).is_blank()));
    let r = analyze(&quad(3.0));
    assert_eq!((r.boundary_edges, r.holes, r.watertight), (4, 1, false));
    assert_eq!(r.area, 100.0);
    // a downward facing floating square still gets supports
    let mut down = quad(3.0);
    down.flip();
    let pts = auto_points(&down, &SupportSettings::default());
    assert!(!pts.is_empty());
    let g = build(&down, &pts, &SupportSettings::default());
    assert!(g.failed.is_empty() && g.mesh.bounds().min.z == 0.0);
    // pad under a flat footprint
    let pad = base_plate_under(&quad(0.0), &BasePlateOptions::default());
    assert_eq!(pad.open_edge_count(), 0);
}

#[test]
fn huge_coordinates() {
    // a 10 mm cube a kilometre away from the origin
    let far = cuboid(Vec3::new(1.0e6, -2.0e6, 5.0e5), Vec3::splat(10.0));
    let o = raster(64, 64, 64.0, 64.0, 4);
    let s = Slicer::new(&[&far], 1.0, o);
    assert_eq!(s.layer_count(), 500_005);
    // the layer through the middle of the cube is empty: it is far outside the plate
    assert!(s.rasterize(500_000).is_blank());
    assert!(!s.section_mm(500_000).is_empty());
    // adding it to a scene brings it to the plate (f32 spacing at 2e6 is 0.25 mm)
    let mut scene = Scene::default();
    let id = scene.add("far", far.clone());
    let b = scene.get(id).unwrap().world_bounds();
    assert!((b.size() - Vec3::splat(10.0)).abs().max_element() < 0.5, "{b:?}");
    assert!(b.min.z.abs() < 1e-3 && b.center().truncate().length() < 0.5);
    let p = PrinterProfile::generic();
    assert!(scene.out_of_bounds(&p).is_empty());
    // geometry tools keep working
    let r = analyze(&far);
    assert!(r.watertight && r.shells == 1);
    // 10 m away the volume is still accurate
    let off = cuboid(Vec3::new(1.0e4, -1.0e4, 5.0e3), Vec3::splat(10.0));
    assert!((analyze(&off).volume - 1000.0).abs() < 1e-2);
    let res = cut(&far, &CutPlane::new(Vec3::new(1.0e6, -2.0e6, 5.0e5), Vec3::X)).unwrap();
    assert!(res.below.triangle_count() > 0 && res.above.triangle_count() > 0);
    assert_eq!(split_shells(&far).len(), 1);
    let huge = cuboid(Vec3::ZERO, Vec3::splat(1.0e7));
    assert!((analyze(&huge).volume - 1.0e21).abs() < 1.0e15);
    let mut big_scene = Scene::default();
    big_scene.add("huge", huge);
    assert_eq!(big_scene.out_of_bounds(&p).len(), 1);
    assert!(!big_scene.arrange(&p, 1.0));
}

#[test]
fn non_finite_input_does_not_panic() {
    let mut m = sphere(Vec3::new(0.0, 0.0, 10.0), 5.0, 16, 8);
    m.positions[3] = Vec3::new(f32::NAN, 0.0, 10.0);
    m.positions[7] = Vec3::new(0.0, f32::INFINITY, 10.0);
    let o = raster(64, 64, 64.0, 64.0, 4);
    let s = Slicer::new(&[&m], 0.5, o);
    for i in 0..s.layer_count() {
        let r = s.rasterize(i);
        assert_eq!(r.pixel_count(), 64 * 64);
        assert!(r.lit_area_px().is_finite());
    }
    let (fixed, _) = repair(&m, &RepairOptions::default());
    assert!(fixed.positions.iter().all(|p| p.is_finite()));
    assert!(cut(&m, &CutPlane::horizontal(10.0)).is_err());
    assert!(hollow(&m, &HollowOptions::default()).is_err());
    let _ = auto_orient(&m, &OrientOptions::default());
    let pad = base_plate_under(&m, &BasePlateOptions::default());
    assert!(pad.positions.iter().all(|p| p.is_finite()));
}

#[test]
fn empty_inputs() {
    let empty = Mesh::default();
    let o = raster(16, 16, 16.0, 16.0, 4);
    let s = Slicer::with_negatives(&[&empty], &[&empty], 0.05, o);
    assert_eq!(s.layer_count(), 0);
    assert!(s.rasterize(0).is_blank());
    assert_eq!(s.rasterize(0).pixel_count(), 256);
    assert!(s.section_mm(0).is_empty());
    assert!(empty.bounds().is_empty());
    assert_eq!(empty.signed_volume(), 0.0);
    assert_eq!(empty.open_edge_count(), 0);
    assert!(split_shells(&empty).is_empty());
    assert!(repair(&empty, &RepairOptions::default()).0.is_empty());
    assert!(hollow(&empty, &HollowOptions::default()).is_err());
    assert!(base_plate_under(&empty, &BasePlateOptions::default()).is_empty());
    assert!(build(&empty, &[], &SupportSettings::default()).mesh.is_empty());
    let mut scene = Scene::default();
    assert!(scene.arrange(&PrinterProfile::generic(), 2.0));
    assert!(scene.slice_meshes().is_empty());
}

#[test]
fn triangulation_preserves_area_of_star_polygons_with_holes() {
    for spikes in [3usize, 5, 8, 13] {
        // star outline (concave) with a square hole in the middle
        let n = spikes * 2;
        let outer: Vec<DVec2> = (0..n)
            .map(|k| {
                let a = k as f64 / n as f64 * std::f64::consts::TAU;
                let r = if k % 2 == 0 { 10.0 } else { 4.0 };
                DVec2::new(a.cos() * r, a.sin() * r)
            })
            .collect();
        let hole = vec![
            DVec2::new(-1.0, -1.0),
            DVec2::new(1.0, -1.0),
            DVec2::new(1.0, 1.0),
            DVec2::new(-1.0, 1.0),
        ];
        let shoelace = |r: &[DVec2]| (0..r.len()).map(|i| r[i].perp_dot(r[(i + 1) % r.len()])).sum::<f64>() * 0.5;
        let want = shoelace(&outer) - shoelace(&hole).abs();
        let t = triangulate_rings(&[&outer, &hole]);
        assert!(t.clean, "{spikes} spikes");
        let pts: Vec<DVec2> = outer.iter().chain(&hole).copied().collect();
        assert_eq!(
            t.triangles.len(),
            pts.len(),
            "triangles of a polygon with one hole = vertices"
        );
        let mut area = 0.0;
        for tr in &t.triangles {
            let (a, b, c) = (pts[tr[0] as usize], pts[tr[1] as usize], pts[tr[2] as usize]);
            let ta = (b - a).perp_dot(c - a) * 0.5;
            assert!(ta > 0.0, "{spikes} spikes: inverted triangle");
            area += ta;
        }
        assert!((area - want).abs() < 1e-9, "{spikes} spikes: {area} vs {want}");
    }
    // a planar 3D loop is triangulated in its own plane, facing its CCW side
    let q = Quat::from_euler(glam::EulerRot::XYZ, 0.3, 1.1, -0.4);
    let lp: Vec<Vec3> = (0..12)
        .map(|k| {
            let a = k as f32 / 12.0 * std::f32::consts::TAU;
            let r = if k % 2 == 0 { 3.0 } else { 1.5 };
            q * Vec3::new(a.cos() * r, a.sin() * r, 0.0)
        })
        .collect();
    let t = triangulate_loop_3d(&lp);
    assert!(t.clean);
    assert_eq!(t.triangles.len(), 10);
    let normal = q * Vec3::Z;
    let mut area = 0.0f32;
    for tr in &t.triangles {
        let (a, b, c) = (lp[tr[0] as usize], lp[tr[1] as usize], lp[tr[2] as usize]);
        let cr = (b - a).cross(c - a);
        assert!(cr.dot(normal) > 0.0);
        area += cr.length() * 0.5;
    }
    let star_area = 12.0 * 0.5 * 3.0 * 1.5 * (std::f32::consts::TAU / 12.0).sin();
    assert!((area - star_area).abs() < 1e-4, "{area} vs {star_area}");
}
