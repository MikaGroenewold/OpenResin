//! Mesh operations, primitives and 2D geometry helpers.

mod common;

use std::f64::consts::PI;

use common::*;
use openresin_core::geometry::{convex_hull, offset_convex, polygon_area, ray_triangle, Aabb};
use openresin_core::mesh::Mesh;
use openresin_core::primitives::{cuboid, extrude_convex, frustum, sphere};
use openresin_core::{Mat4, Quat, Vec2, Vec3};

fn rotation() -> Quat {
    Quat::from_euler(glam::EulerRot::XYZ, 0.4, 0.7, 0.2)
}

/// Every face normal of a convex mesh points away from `c`.
#[track_caller]
fn assert_outward(m: &Mesh, c: Vec3) {
    for i in 0..m.triangle_count() {
        let [a, b, cc] = m.tri(i);
        let d = m.face_normal(i).dot((a + b + cc) / 3.0 - c);
        assert!(d > 0.0, "face {i} points inward ({d})");
    }
}

#[test]
fn transforms_scale_volume_and_keep_outward_normals() {
    let c = Vec3::new(1.0, -2.0, 3.0);
    let m = cuboid(c, Vec3::new(2.0, 3.0, 4.0));
    let cases = [
        (Mat4::from_scale(Vec3::new(-1.0, 1.0, 1.0)), 24.0),
        (Mat4::from_scale(Vec3::new(1.0, -1.0, -1.0)), 24.0),
        (Mat4::from_scale(Vec3::new(-2.0, -1.0, -0.5)), 24.0),
        (Mat4::from_scale(Vec3::new(2.0, 3.0, 4.0)), 24.0 * 24.0),
        (
            Mat4::from_scale_rotation_translation(Vec3::new(1.0, -1.0, 2.0), rotation(), Vec3::new(5.0, 6.0, 7.0)),
            48.0,
        ),
    ];
    for (k, (mat, vol)) in cases.iter().enumerate() {
        let t = m.transformed(mat);
        assert_close(t.signed_volume(), *vol, 1e-3, &format!("case {k} volume"));
        assert_outward(&t, mat.transform_point3(c));
        assert_eq!(t.open_edge_count(), 0);
        // in-place variant gives the same mesh
        let mut p = m.clone();
        p.transform_in_place(mat);
        assert_eq!(p.positions, t.positions);
        assert_eq!(p.triangles, t.triangles);
        // bounds without allocating
        let tb = m.transformed_bounds(mat);
        assert_eq!(tb, t.bounds());
    }
}

#[test]
fn orient_outward_fixes_inverted_meshes() {
    let s = sphere(Vec3::new(3.0, 0.0, 1.0), 4.0, 32, 16);
    let v = s.signed_volume();
    let mut inv = s.clone();
    inv.flip();
    assert_close(inv.signed_volume(), -v, 1e-9, "flipped volume");
    inv.orient_outward();
    assert_close(inv.signed_volume(), v, 1e-9, "re-oriented volume");
    assert_outward(&inv, Vec3::new(3.0, 0.0, 1.0));
    // already outward meshes are left alone
    let mut same = s.clone();
    same.orient_outward();
    assert_eq!(same.triangles, s.triangles);
}

#[test]
fn lay_flat_puts_the_chosen_face_on_the_plate() {
    let box_ = cuboid(Vec3::ZERO, Vec3::new(30.0, 20.0, 10.0)).transformed(&Mat4::from_quat(rotation()));
    for face in 0..box_.triangle_count() {
        let q = box_.lay_flat_rotation(face);
        let r = box_.transformed(&Mat4::from_quat(q));
        assert!(
            (r.face_normal(face) + Vec3::Z).length() < 1e-4,
            "face {face}: {}",
            r.face_normal(face)
        );
        let zmin = r.bounds().min.z;
        for v in r.tri(face) {
            assert!((v.z - zmin).abs() < 1e-3, "face {face} not on the plate");
        }
    }
    // the top face of an unrotated box (antiparallel case)
    let b = cuboid(Vec3::ZERO, Vec3::new(4.0, 6.0, 2.0));
    let top = (0..b.triangle_count()).find(|&i| b.face_normal(i).z > 0.99).unwrap();
    let r = b.transformed(&Mat4::from_quat(b.lay_flat_rotation(top)));
    assert!((r.face_normal(top) + Vec3::Z).length() < 1e-5);
    assert_close(r.bounds().size().z as f64, 2.0, 1e-5, "height");
}

#[test]
fn best_flat_rotation_lays_the_largest_face_exactly_flat() {
    let box_ = cuboid(Vec3::ZERO, Vec3::new(30.0, 20.0, 10.0)).transformed(&Mat4::from_quat(rotation()));
    let r = box_.transformed(&Mat4::from_quat(box_.best_flat_rotation()));
    let b = r.bounds();
    assert_close(b.size().z as f64, 10.0, 1e-3, "height after lay flat");
    assert_close(area_at_z(&r, b.min.z, 1e-3), 600.0, 1e-2, "area on the plate");
    // a cone lies on its base
    let cone = frustum(Vec3::ZERO, Vec3::new(0.0, 0.0, 8.0), 6.0, 0.0, 64).transformed(&Mat4::from_quat(rotation()));
    let r = cone.transformed(&Mat4::from_quat(cone.best_flat_rotation()));
    let b = r.bounds();
    assert_close(b.size().z as f64, 8.0, 1e-3, "cone height");
    assert_close(
        area_at_z(&r, b.min.z, 1e-3),
        ngon_area(64, 6.0),
        1e-2,
        "cone base on the plate",
    );
    assert_eq!(Mesh::default().best_flat_rotation(), Quat::IDENTITY);
}

#[test]
fn open_edges_of_open_closed_and_non_manifold_meshes() {
    let cube = cuboid(Vec3::ZERO, Vec3::ONE);
    assert_eq!(cube.open_edge_count(), 0);
    let mut one = cube.clone();
    one.triangles.remove(0);
    assert_eq!(one.open_edge_count(), 3);
    // removing a whole face (2 triangles) leaves its 4 border edges open
    let mut face = cube.clone();
    face.triangles.drain(0..2);
    assert_eq!(face.open_edge_count(), 4);
    let single = Mesh::new(vec![Vec3::ZERO, Vec3::X, Vec3::Y], vec![[0, 1, 2]]);
    assert_eq!(single.open_edge_count(), 3);
    // a fin on an edge: the edge is used three times, the fin's other edges once
    let mut fin = cube.clone();
    fin.positions.push(Vec3::new(0.0, 0.0, 3.0));
    fin.triangles.push([6, 7, 8]);
    assert_eq!(fin.open_edge_count(), 3);
    for m in [
        sphere(Vec3::ZERO, 1.0, 12, 6),
        frustum(Vec3::ZERO, Vec3::Z, 1.0, 0.0, 9),
    ] {
        assert_eq!(m.open_edge_count(), 0);
    }
}

#[test]
fn primitive_volumes_and_areas_match_analytic_values() {
    // cuboid
    let c = cuboid(Vec3::new(3.0, 4.0, 5.0), Vec3::new(2.0, 3.0, 7.0));
    assert_close(c.signed_volume(), 42.0, 1e-4, "cuboid volume");
    assert_close(c.surface_area(), 2.0 * (6.0 + 21.0 + 14.0), 1e-4, "cuboid area");
    // sphere (inscribed polyhedron converges from below)
    let r = 5.0f64;
    let s = sphere(Vec3::new(1.0, 2.0, 3.0), r as f32, 128, 64);
    let v = 4.0 / 3.0 * PI * r.powi(3);
    let a = 4.0 * PI * r * r;
    assert!(s.signed_volume() < v && s.surface_area() < a);
    assert_rel(s.signed_volume(), v, 2e-3, "sphere volume");
    assert_rel(s.surface_area(), a, 2e-3, "sphere area");
    // frustum of an n-gon pyramid: V = h/3 (A1 + A2 + sqrt(A1 A2)), also along a tilted axis
    for (axis, n) in [(Vec3::new(0.0, 0.0, 6.0), 24u32), (Vec3::new(2.0, -3.0, 6.0), 17)] {
        let h = axis.length() as f64;
        let (a1, a2) = (ngon_area(n, 3.0), ngon_area(n, 1.5));
        let f = frustum(Vec3::new(1.0, 1.0, 1.0), Vec3::new(1.0, 1.0, 1.0) + axis, 3.0, 1.5, n);
        assert_rel(
            f.signed_volume(),
            h / 3.0 * (a1 + a2 + (a1 * a2).sqrt()),
            1e-5,
            "frustum volume",
        );
        let cone = frustum(Vec3::ZERO, axis, 3.0, 0.0, n);
        assert_rel(cone.signed_volume(), h / 3.0 * a1, 1e-5, "cone volume");
        // cylinder: caps plus n rectangular sides
        let cyl = frustum(Vec3::ZERO, axis, 2.0, 2.0, n);
        let side = 2.0 * 2.0 * (PI / n as f64).sin();
        assert_rel(
            cyl.surface_area(),
            2.0 * ngon_area(n, 2.0) + n as f64 * side * h,
            1e-5,
            "cylinder area",
        );
        assert_rel(cyl.signed_volume(), ngon_area(n, 2.0) * h, 1e-5, "cylinder volume");
    }
    // extrusions
    let sq = [
        Vec2::new(0.0, 0.0),
        Vec2::new(4.0, 0.0),
        Vec2::new(4.0, 4.0),
        Vec2::new(0.0, 4.0),
    ];
    assert_close(extrude_convex(&sq, 1.0, 3.0, 0.0).signed_volume(), 32.0, 1e-4, "prism");
    // chamfer 0.5: top 3x3, bottom 4x4, height 1 -> 37/3
    let ch = extrude_convex(&sq, 0.0, 1.0, 0.5);
    assert_close(ch.signed_volume(), 37.0 / 3.0, 1e-4, "chamfered prism");
    let tb = ch.bounds();
    assert_eq!((tb.min.z, tb.max.z), (0.0, 1.0));
    let hex: Vec<Vec2> = (0..6)
        .map(|k| Vec2::from_angle(k as f32 * std::f32::consts::FRAC_PI_3) * 2.0)
        .collect();
    let hp = extrude_convex(&hex, -1.0, 1.5, 0.0);
    assert_close(hp.signed_volume(), ngon_area(6, 2.0) * 2.5, 1e-4, "hex prism");
    assert_eq!(hp.open_edge_count(), 0);
    // invalid input
    assert!(frustum(Vec3::ONE, Vec3::ONE, 1.0, 1.0, 8).is_empty());
    assert!(extrude_convex(&sq, 1.0, 1.0, 0.0).is_empty());
    assert!(extrude_convex(&sq[..2], 0.0, 1.0, 0.0).is_empty());
}

#[test]
fn soup_welding_cleanup_and_append() {
    let cube = cuboid(Vec3::ZERO, Vec3::splat(2.0));
    let s = soup(&cube);
    assert_eq!(s.positions.len(), 36);
    assert_eq!(s.open_edge_count(), 36);
    let tris: Vec<[Vec3; 3]> = (0..s.triangle_count()).map(|i| s.tri(i)).collect();
    let mut with_degenerate = tris.clone();
    with_degenerate.push([Vec3::ZERO, Vec3::ZERO, Vec3::ONE]); // repeated vertex: dropped while welding
    let w = Mesh::from_soup(&with_degenerate);
    assert_eq!((w.positions.len(), w.triangle_count(), w.open_edge_count()), (9, 12, 0));
    // cleanup removes zero-area triangles and the vertices only they used
    let mut m = cube.clone();
    m.positions.push(Vec3::new(5.0, 0.0, 0.0));
    m.positions.push(Vec3::new(6.0, 0.0, 0.0));
    m.positions.push(Vec3::new(9.0, 9.0, 9.0)); // unreferenced
    m.triangles.push([8, 9, 0]); // (5,0,0), (6,0,0), (-1,-1,-1): a proper triangle, kept
    m.triangles.push([8, 9, 9]); // repeated index: zero area
    m.cleanup();
    assert_eq!(m.triangle_count(), 13);
    assert_eq!(m.positions.len(), 10);
    let mut flat = cube.clone();
    flat.positions.push(Vec3::new(1.0, 1.0, 1.0) * 3.0);
    flat.triangles.push([0, 6, 8]); // (-1,-1,-1), (1,1,1), (3,3,3): collinear
    flat.cleanup();
    assert_eq!((flat.triangle_count(), flat.positions.len()), (12, 8));
    // append offsets indices
    let mut a = cuboid(Vec3::ZERO, Vec3::ONE);
    a.append(&cuboid(Vec3::new(5.0, 0.0, 0.0), Vec3::ONE));
    assert_eq!((a.positions.len(), a.triangle_count()), (16, 24));
    assert_close(a.signed_volume(), 2.0, 1e-5, "appended volume");
    assert!(a.triangles[12..].iter().flatten().all(|&i| i >= 8));
    // flat render buffer
    let flat_pos = a.to_flat_positions();
    assert_eq!(flat_pos.len(), 24 * 9);
    let t = a.tri(13);
    assert_eq!(&flat_pos[13 * 9..13 * 9 + 3], &[t[0].x, t[0].y, t[0].z]);
}

#[test]
fn aabb_helpers() {
    assert!(Aabb::EMPTY.is_empty());
    assert_eq!(Aabb::EMPTY.size(), Vec3::ZERO);
    assert!(Mesh::default().bounds().is_empty());
    let a = Aabb::from_points([Vec3::new(0.0, 0.0, 0.0), Vec3::new(2.0, 1.0, 3.0)].iter());
    let b = Aabb::from_points([Vec3::new(1.5, 0.5, -1.0), Vec3::new(4.0, 4.0, 0.0)].iter());
    let c = Aabb::from_points([Vec3::new(2.5, 0.0, 0.0), Vec3::new(3.0, 1.0, 1.0)].iter());
    assert_eq!(a.union(&b).min, Vec3::new(0.0, 0.0, -1.0));
    assert_eq!(a.union(&b).max, Vec3::new(4.0, 4.0, 3.0));
    assert_eq!(a.center(), Vec3::new(1.0, 0.5, 1.5));
    assert!(a.intersects_xy(&b) && !a.intersects_xy(&c) && b.intersects_xy(&c));
    assert_eq!(Aabb::EMPTY.union(&a), a);
}

#[test]
fn convex_hull_offset_and_area() {
    // points on a circle plus interior points: hull = circle points, CCW
    let mut pts: Vec<Vec2> = (0..40)
        .map(|k| Vec2::from_angle(k as f32 / 40.0 * std::f32::consts::TAU) * 10.0)
        .collect();
    for k in 0..200 {
        let f = k as f32;
        pts.push(Vec2::new((f * 0.731).sin(), (f * 1.37).cos()) * 6.0);
    }
    pts.push(pts[3]); // duplicate
    let h = convex_hull(&pts);
    assert_eq!(h.len(), 40);
    assert!(polygon_area(&h) > 0.0);
    assert_close(polygon_area(&h) as f64, ngon_area(40, 10.0), 1e-2, "hull area");
    let mut cw = h.clone();
    cw.reverse();
    assert_close(polygon_area(&cw) as f64, -ngon_area(40, 10.0), 1e-2, "cw area");
    // every input point is inside or on the hull
    for p in &pts {
        for i in 0..h.len() {
            let (a, b) = (h[i], h[(i + 1) % h.len()]);
            assert!(openresin_core::geometry::cross2(b - a, *p - a) >= -1e-3);
        }
    }
    // collinear and tiny inputs
    assert_eq!(convex_hull(&[Vec2::ZERO, Vec2::X, Vec2::X * 2.0]).len(), 2);
    assert!(convex_hull(&[]).is_empty());
    // offsetting a square by d grows every side by 2d
    let sq = [
        Vec2::new(0.0, 0.0),
        Vec2::new(3.0, 0.0),
        Vec2::new(3.0, 2.0),
        Vec2::new(0.0, 2.0),
    ];
    assert_close(
        polygon_area(&offset_convex(&sq, 0.5)) as f64,
        4.0 * 3.0,
        1e-4,
        "grown square",
    );
    assert_close(
        polygon_area(&offset_convex(&sq, -0.5)) as f64,
        2.0 * 1.0,
        1e-4,
        "shrunk square",
    );
}

#[test]
fn ray_triangle_hits_and_misses() {
    let (a, b, c) = (
        Vec3::new(0.0, 0.0, 2.0),
        Vec3::new(4.0, 0.0, 2.0),
        Vec3::new(0.0, 4.0, 2.0),
    );
    let d = ray_triangle(Vec3::new(1.0, 1.0, -3.0), Vec3::Z, a, b, c).unwrap();
    assert_close(d as f64, 5.0, 1e-5, "distance");
    // scaled direction scales the parameter
    let d2 = ray_triangle(Vec3::new(1.0, 1.0, -3.0), Vec3::Z * 2.0, a, b, c).unwrap();
    assert_close(d2 as f64, 2.5, 1e-5, "parameter");
    assert!(
        ray_triangle(Vec3::new(3.0, 3.0, -3.0), Vec3::Z, a, b, c).is_none(),
        "outside"
    );
    assert!(
        ray_triangle(Vec3::new(1.0, 1.0, 5.0), Vec3::Z, a, b, c).is_none(),
        "behind"
    );
    assert!(
        ray_triangle(Vec3::new(1.0, 1.0, 0.0), Vec3::X, a, b, c).is_none(),
        "parallel"
    );
}
