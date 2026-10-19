//! Mesh repair, splitting into parts and plane cuts.

mod common;

use std::f64::consts::PI;

use common::*;
use openresin_core::cut::{cut, cut_mesh, cut_z, CutPlane};
use openresin_core::label::text_mesh;
use openresin_core::mesh::Mesh;
use openresin_core::primitives::{cuboid, frustum, sphere};
use openresin_core::repair::{analyze, repair, RepairOptions};
use openresin_core::split::{part_count, split_shells};
use openresin_core::Vec3;

fn boxed(x0: f32, x1: f32, y0: f32, y1: f32, z0: f32, z1: f32) -> Mesh {
    cuboid(
        Vec3::new((x0 + x1) * 0.5, (y0 + y1) * 0.5, (z0 + z1) * 0.5),
        Vec3::new(x1 - x0, y1 - y0, z1 - z0),
    )
}

#[track_caller]
fn assert_clean_closed(m: &Mesh, what: &str) {
    let r = analyze(m);
    assert!(r.watertight, "{what}: {r:?}");
    assert_eq!(r.inverted_shells, 0, "{what}: {r:?}");
    assert_eq!(m.open_edge_count(), 0, "{what}");
}

// ---------------------------------------------------------------- repair

#[test]
fn analyze_counts_shells_holes_and_problems() {
    let mut m = boxed(0.0, 10.0, 0.0, 10.0, 0.0, 10.0);
    m.append(&boxed(20.0, 25.0, 0.0, 5.0, 0.0, 5.0));
    m.append(&sphere(Vec3::new(-20.0, 0.0, 5.0), 4.0, 16, 8));
    let r = analyze(&m);
    assert_eq!((r.shells, r.holes, r.boundary_edges), (3, 0, 0));
    assert!(r.is_clean(), "{r:?}");
    assert_eq!((r.vertices, r.triangles), (m.positions.len(), m.triangle_count()));
    let ball = sphere(Vec3::new(-20.0, 0.0, 5.0), 4.0, 16, 8).signed_volume();
    assert_close(r.volume, 1000.0 + 125.0 + ball, 1e-6, "volume");
    assert_close(
        r.area,
        600.0 + 150.0 + sphere(Vec3::ZERO, 4.0, 16, 8).surface_area(),
        1e-3,
        "area",
    );
    // remove the top and the bottom face of the first box: two separate holes
    let mut holed = m.clone();
    holed.triangles.drain(0..4);
    let r = analyze(&holed);
    assert_eq!((r.holes, r.boundary_edges), (2, 8));
    assert!(!r.watertight && !r.is_clean());
}

#[test]
fn holes_are_closed_with_the_original_volume() {
    // single missing triangles on a sphere, far apart: refilled exactly
    let s = sphere(Vec3::new(1.0, 2.0, 30.0), 10.0, 32, 16);
    let mut holed = s.clone();
    for i in [400usize, 200, 40] {
        holed.triangles.remove(i);
    }
    let r = analyze(&holed);
    assert_eq!((r.holes, r.boundary_edges), (3, 9));
    let (fixed, rep) = repair(&holed, &RepairOptions::default());
    assert_eq!(
        (rep.holes_filled, rep.hole_triangles_added, rep.holes_unfilled),
        (3, 3, 0)
    );
    assert!(rep.after.is_clean(), "{:?}", rep.after);
    assert_rel(fixed.signed_volume(), s.signed_volume(), 1e-9, "volume");
    // a missing face on a box (planar quad hole)
    let b = boxed(0.0, 4.0, 0.0, 5.0, 0.0, 6.0);
    let mut open = b.clone();
    open.triangles.drain(6..8); // the +x face
    let (fixed, rep) = repair(&open, &RepairOptions::default());
    assert_eq!(rep.holes_filled, 1);
    assert_clean_closed(&fixed, "box");
    assert_close(fixed.signed_volume(), 120.0, 1e-4, "box volume");
    // a large hole: the whole top cap of a sphere (several rings) is missing
    let mut cap = s.clone();
    let zcut = 30.0 + 10.0 * 0.6;
    cap.triangles.retain(|t| t.iter().any(|&i| cap_z(&s, i) < zcut));
    let (fixed, rep) = repair(&cap, &RepairOptions::default());
    assert_eq!(rep.holes_filled, 1);
    assert_clean_closed(&fixed, "capped sphere");
    assert!(fixed.signed_volume() < s.signed_volume() && fixed.signed_volume() > s.signed_volume() * 0.85);
    // holes above the size limit stay open
    let limited = RepairOptions {
        max_hole_edges: 10,
        ..Default::default()
    };
    let (still_open, rep) = repair(&cap, &limited);
    assert_eq!((rep.holes_filled, rep.holes_unfilled), (0, 1));
    assert!(!rep.after.watertight);
    assert_eq!(still_open.triangle_count(), cap.triangle_count());
}

fn cap_z(m: &Mesh, i: u32) -> f32 {
    m.positions[i as usize].z
}

#[test]
fn duplicates_and_flipped_normals_are_fixed() {
    let cube = boxed(-5.0, 5.0, -5.0, 5.0, 0.0, 10.0);
    // every triangle twice
    let mut dup = cube.clone();
    dup.triangles.extend(cube.triangles.clone());
    assert_eq!(analyze(&dup).duplicate_triangles, 12);
    let (fixed, rep) = repair(&dup, &RepairOptions::default());
    assert_eq!(rep.duplicates_removed, 12);
    assert_eq!(fixed.triangle_count(), 12);
    assert_clean_closed(&fixed, "deduplicated");
    assert_close(fixed.signed_volume(), 1000.0, 1e-3, "volume");
    // a few flipped triangles
    let mut flipped = sphere(Vec3::ZERO, 6.0, 24, 12);
    let reference = flipped.signed_volume();
    for i in [0usize, 7, 50, 51, 300, 301, 302] {
        flipped.triangles[i].swap(1, 2);
    }
    let r = analyze(&flipped);
    assert!(r.inconsistent_edges > 0 && !r.watertight);
    let (fixed, rep) = repair(&flipped, &RepairOptions::default());
    assert_eq!(rep.triangles_flipped, 7);
    assert_clean_closed(&fixed, "re-oriented");
    assert_rel(fixed.signed_volume(), reference, 1e-9, "volume");
    // a soup straight from an STL file, inside out
    let mut inv = soup(&cube);
    inv.flip();
    let (fixed, rep) = repair(&inv, &RepairOptions::default());
    assert_eq!(rep.vertices_welded, 36 - 8);
    assert_eq!(rep.triangles_flipped, 12);
    assert_clean_closed(&fixed, "welded soup");
    assert_close(fixed.signed_volume(), 1000.0, 1e-3, "volume");
}

#[test]
fn non_manifold_input_is_closed_with_the_expected_volume() {
    // two cubes sharing one edge (welded): that edge has four triangles
    let a = boxed(0.0, 2.0, 0.0, 2.0, 0.0, 2.0);
    let b = boxed(2.0, 4.0, 2.0, 4.0, 0.0, 2.0);
    let mut m = a.clone();
    m.append(&b);
    let (welded, _) = repair(
        &m,
        &RepairOptions {
            fill_holes: false,
            fix_orientation: false,
            ..Default::default()
        },
    );
    let r = analyze(&welded);
    assert_eq!(r.non_manifold_edges, 1);
    let (fixed, rep) = repair(&welded, &RepairOptions::default());
    assert_eq!(rep.after.boundary_edges, 0);
    assert_close(fixed.signed_volume(), 16.0, 1e-4, "volume");
    assert_eq!(split_shells(&fixed).len(), 1, "connected through the shared edge");
    // a zero-thickness fin hanging off an edge
    let mut fin = boxed(-5.0, 5.0, -5.0, 5.0, 0.0, 10.0);
    let tip = fin.positions.len() as u32;
    fin.positions.push(Vec3::new(0.0, 5.0, 20.0));
    fin.triangles.push([6, 7, tip]);
    assert!(analyze(&fin).non_manifold_edges > 0 || analyze(&fin).boundary_edges > 0);
    let (fixed, rep) = repair(&fin, &RepairOptions::default());
    assert_eq!(rep.after.boundary_edges, 0, "{:?}", rep.after);
    assert_close(fixed.signed_volume(), 1000.0, 1e-3, "fin adds no volume");
    // nothing broken: the fixed meshes slice like the clean cube
    assert!(fixed.positions.iter().all(|p| p.is_finite()));
}

#[test]
fn repair_options_can_be_disabled() {
    let mut m = sphere(Vec3::ZERO, 5.0, 16, 8);
    m.triangles.remove(5);
    m.triangles[20].swap(1, 2);
    let off = RepairOptions {
        weld_tolerance: 0.0,
        remove_degenerate: false,
        remove_duplicates: false,
        fix_orientation: false,
        fill_holes: false,
        max_hole_edges: 0,
        min_shell_volume: 0.0,
    };
    let (same, rep) = repair(&m, &off);
    assert_eq!(same.triangle_count(), m.triangle_count());
    assert_eq!(
        (rep.holes_filled, rep.triangles_flipped, rep.duplicates_removed),
        (0, 0, 0)
    );
    assert_eq!(rep.before, rep.after);
    let (fixed, rep) = repair(&m, &RepairOptions::default());
    assert_eq!((rep.holes_filled, rep.triangles_flipped), (1, 1));
    assert!(rep.after.is_clean());
    assert_eq!(fixed.triangle_count(), m.triangle_count() + 1);
}

// ---------------------------------------------------------------- split

#[test]
fn split_separates_parts_sorted_by_volume() {
    let mut m = boxed(0.0, 5.0, 0.0, 5.0, 0.0, 5.0); // 125
    m.append(&boxed(10.0, 30.0, 0.0, 20.0, 0.0, 20.0)); // 8000
    m.append(&boxed(-20.0, -10.0, 0.0, 10.0, 0.0, 10.0)); // 1000
                                                          // a hollowed part: outer box with an inverted inner box stays one part
    let mut hollow = boxed(40.0, 60.0, 0.0, 20.0, 0.0, 20.0);
    let mut inner = boxed(45.0, 55.0, 5.0, 15.0, 5.0, 15.0);
    inner.flip();
    hollow.append(&inner);
    m.append(&hollow); // 8000 - 1000
    let parts = split_shells(&m);
    assert_eq!(parts.len(), 4);
    assert_eq!(part_count(&m), 4);
    let vols: Vec<f64> = parts.iter().map(|p| p.signed_volume()).collect();
    for (v, want) in vols.iter().zip([8000.0, 7000.0, 1000.0, 125.0]) {
        assert_close(*v, want, 1e-2, "part volume");
    }
    for p in &parts {
        assert_eq!(p.open_edge_count(), 0);
    }
    assert_eq!(
        parts.iter().map(|p| p.triangle_count()).sum::<usize>(),
        m.triangle_count()
    );
    // parts are compact: only their own vertices
    assert_eq!(parts[3].positions.len(), 8);
    assert_eq!(parts[1].positions.len(), 16);
    // coincident but unwelded shells are separate; welded ones share vertices and merge
    let mut touching = boxed(0.0, 1.0, 0.0, 1.0, 0.0, 1.0);
    touching.append(&boxed(1.0, 2.0, 0.0, 1.0, 0.0, 1.0));
    assert_eq!(split_shells(&touching).len(), 2);
    let (welded, _) = repair(&touching, &RepairOptions::default());
    assert_eq!(split_shells(&welded).len(), 1);
}

// ---------------------------------------------------------------- cut

#[test]
fn axis_aligned_cuts_split_the_volume_exactly() {
    let m = boxed(-9.0, 11.0, -3.0, 7.0, 0.0, 6.0); // 20 x 10 x 6 = 1200
    let cases = [
        (Vec3::new(3.0, 0.0, 0.0), Vec3::X, 12.0 * 60.0),
        (Vec3::new(3.0, 0.0, 0.0), -Vec3::X, 8.0 * 60.0),
        (Vec3::new(0.0, 0.0, 0.0), Vec3::Y, 3.0 * 120.0),
        (Vec3::new(0.0, 6.5, 0.0), Vec3::Y, 9.5 * 120.0),
        (Vec3::new(0.0, 0.0, 1.5), Vec3::Z, 1.5 * 200.0),
    ];
    for (pt, n, below) in cases {
        let (b, a) = cut_mesh(&m, pt, n).unwrap();
        assert_clean_closed(&b, "below");
        assert_clean_closed(&a, "above");
        assert_close(b.signed_volume(), below, 1e-3, &format!("below {n}"));
        assert_close(a.signed_volume(), 1200.0 - below, 1e-3, &format!("above {n}"));
        assert!(b.positions.iter().all(|p| (*p - pt).dot(n) <= 1e-4));
        assert!(a.positions.iter().all(|p| (*p - pt).dot(n) >= -1e-4));
    }
    let (b, a) = cut_z(&m, 2.0).unwrap();
    assert_eq!((b.bounds().max.z, a.bounds().min.z), (2.0, 2.0));
    // cutting the lower half again
    let (bb, ba) = cut_z(&b, 1.0).unwrap();
    assert_close(
        bb.signed_volume() + ba.signed_volume() + a.signed_volume(),
        1200.0,
        1e-3,
        "three pieces",
    );
}

#[test]
fn sphere_cap_volumes_for_tilted_planes() {
    let r = 10.0f64;
    let c = Vec3::new(1.0, -2.0, 15.0);
    let s = sphere(c, r as f32, 64, 32);
    let total = s.signed_volume();
    for (n, d) in [
        (Vec3::new(1.0, 2.0, 2.0), 4.0f32),
        (Vec3::new(-0.3, 0.1, 1.0), -6.5),
        (Vec3::new(0.0, 1.0, 0.0), 0.37),
    ] {
        let nn = n.normalize();
        let res = cut(&s, &CutPlane::new(c + nn * d, n)).unwrap();
        assert_eq!((res.cap_loops, res.open_chains), (1, 0));
        assert_clean_closed(&res.below, "below");
        assert_clean_closed(&res.above, "above");
        assert_rel(
            res.below.signed_volume() + res.above.signed_volume(),
            total,
            1e-6,
            "sum",
        );
        let h = r - d as f64;
        let cap = PI * h * h * (3.0 * r - h) / 3.0;
        assert_rel(res.above.signed_volume(), cap, 0.015, "spherical cap");
    }
}

#[test]
fn cylinder_and_tube_caps() {
    let n = 64;
    let cyl = frustum(Vec3::new(2.0, 3.0, 0.0), Vec3::new(2.0, 3.0, 10.0), 5.0, 5.0, n);
    let (b, a) = cut_z(&cyl, 4.0).unwrap();
    let area = ngon_area(n, 5.0);
    assert_close(area_at_z(&b, 4.0, 1e-4), area, 1e-3, "cap below");
    assert_close(area_at_z(&a, 4.0, 1e-4), area, 1e-3, "cap above");
    assert_rel(b.signed_volume(), area * 4.0, 1e-5, "below");
    assert_rel(a.signed_volume(), area * 6.0, 1e-5, "above");
    // a tube (outer cylinder with an inverted inner one): the cap is a ring
    let mut tube = frustum(Vec3::ZERO, Vec3::new(0.0, 0.0, 10.0), 10.0, 10.0, n);
    let mut inner = frustum(Vec3::ZERO, Vec3::new(0.0, 0.0, 10.0), 6.0, 6.0, n);
    inner.flip();
    tube.append(&inner);
    let res = cut(&tube, &CutPlane::horizontal(5.0)).unwrap();
    assert_eq!(res.cap_loops, 2);
    let ring = ngon_area(n, 10.0) - ngon_area(n, 6.0);
    assert_close(area_at_z(&res.below, 5.0, 1e-4), ring, 1e-3, "ring cap");
    assert_rel(res.below.signed_volume(), ring * 5.0, 1e-5, "below");
    assert_rel(res.above.signed_volume(), ring * 5.0, 1e-5, "above");
    assert_eq!(res.below.open_edge_count(), 0);
}

#[test]
fn concave_and_multi_loop_sections() {
    // U: base plus two arms (three closed shells)
    let mut u = boxed(-10.0, 10.0, -3.0, 3.0, 0.0, 4.0);
    u.append(&boxed(-10.0, -6.0, -3.0, 3.0, 4.0, 14.0));
    u.append(&boxed(6.0, 10.0, -3.0, 3.0, 4.0, 14.0));
    let total = u.signed_volume();
    let arms = cut(&u, &CutPlane::horizontal(10.0)).unwrap();
    assert_eq!(arms.cap_loops, 2);
    assert_close(arms.above.signed_volume(), 2.0 * 4.0 * 6.0 * 4.0, 1e-3, "arm tops");
    assert_close(
        arms.below.signed_volume() + arms.above.signed_volume(),
        total,
        1e-3,
        "sum",
    );
    assert_eq!(split_shells(&arms.above).len(), 2);
    let base = cut(&u, &CutPlane::horizontal(2.0)).unwrap();
    assert_eq!(base.cap_loops, 1);
    // text glyphs: concave outlines with holes ("8" has two) and diagonal pinches
    for text in ["L", "8", "%&@"] {
        let m = text_mesh(text, 7.0, 2.0); // 1 mm pixels
        let pixel_area = m.signed_volume() / 2.0;
        let res = cut(&m, &CutPlane::horizontal(0.7)).unwrap();
        assert_clean_closed(&res.below, text);
        assert_clean_closed(&res.above, text);
        assert_close(res.below.signed_volume(), pixel_area * 0.7, 1e-3, text);
        assert_close(res.above.signed_volume(), pixel_area * 1.3, 1e-3, text);
        assert_close(area_at_z(&res.below, 0.7, 1e-4), pixel_area, 1e-3, text);
    }
    assert_close(text_mesh("8", 7.0, 1.0).signed_volume(), 17.0, 1e-4, "'8' pixels");
}

#[test]
fn cuts_that_miss_and_invalid_planes() {
    let m = boxed(0.0, 1.0, 0.0, 1.0, 0.0, 1.0);
    let (b, a) = cut_z(&m, -1.0).unwrap();
    assert!(b.is_empty() && a.triangle_count() == 12);
    let (b, a) = cut_z(&m, 2.0).unwrap();
    assert!(a.is_empty() && b.triangle_count() == 12);
    assert!(cut(&m, &CutPlane::new(Vec3::ZERO, Vec3::new(f32::NAN, 0.0, 1.0))).is_err());
    assert!(cut(&m, &CutPlane::new(Vec3::new(f32::INFINITY, 0.0, 0.0), Vec3::Z)).is_err());
    let mut bad = m.clone();
    bad.positions[0].x = f32::NAN;
    assert!(cut_z(&bad, 0.5).is_err());
    let (b, a) = cut_z(&Mesh::default(), 0.0).unwrap();
    assert!(b.is_empty() && a.is_empty());
}
