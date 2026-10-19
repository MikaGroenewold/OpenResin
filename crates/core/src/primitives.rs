//! Closed, outward oriented primitive meshes used for supports, rafts and tests.

use std::f32::consts::TAU;

use glam::{Quat, Vec2, Vec3};
use serde::{Deserialize, Serialize};

use crate::mesh::Mesh;

/// Axis aligned box centred at `center` with full extents `size`.
pub fn cuboid(center: Vec3, size: Vec3) -> Mesh {
    let h = size * 0.5;
    let p = |x: f32, y: f32, z: f32| center + Vec3::new(x * h.x, y * h.y, z * h.z);
    let positions = vec![
        p(-1.0, -1.0, -1.0),
        p(1.0, -1.0, -1.0),
        p(1.0, 1.0, -1.0),
        p(-1.0, 1.0, -1.0),
        p(-1.0, -1.0, 1.0),
        p(1.0, -1.0, 1.0),
        p(1.0, 1.0, 1.0),
        p(-1.0, 1.0, 1.0),
    ];
    let triangles = vec![
        [0, 2, 1],
        [0, 3, 2], // bottom
        [4, 5, 6],
        [4, 6, 7], // top
        [0, 1, 5],
        [0, 5, 4], // front (-y)
        [1, 2, 6],
        [1, 6, 5], // right
        [2, 3, 7],
        [2, 7, 6], // back
        [3, 0, 4],
        [3, 4, 7], // left
    ];
    Mesh::new(positions, triangles)
}

/// Truncated cone (frustum) between `a` (radius `ra`) and `b` (radius `rb`).
/// Either radius may be zero, producing a proper cone tip.
pub fn frustum(a: Vec3, b: Vec3, ra: f32, rb: f32, segments: u32) -> Mesh {
    let segments = segments.max(3);
    let axis = b - a;
    let len = axis.length();
    if len <= 1e-6 {
        return Mesh::default();
    }
    let rot = Quat::from_rotation_arc(Vec3::Z, axis / len);
    let ring = |center: Vec3, r: f32| -> Vec<Vec3> {
        (0..segments)
            .map(|i| {
                let t = i as f32 / segments as f32 * TAU;
                center + rot * Vec3::new(t.cos() * r, t.sin() * r, 0.0)
            })
            .collect()
    };
    let mut positions = Vec::new();
    let mut triangles = Vec::new();
    let n = segments;
    // bottom ring/apex
    let bottom_start = 0u32;
    if ra > 0.0 {
        positions.extend(ring(a, ra));
    } else {
        positions.push(a);
    }
    let top_start = positions.len() as u32;
    if rb > 0.0 {
        positions.extend(ring(b, rb));
    } else {
        positions.push(b);
    }
    let bi = |i: u32| if ra > 0.0 { bottom_start + i % n } else { bottom_start };
    let ti = |i: u32| if rb > 0.0 { top_start + i % n } else { top_start };
    for i in 0..n {
        let (b0, b1, t0, t1) = (bi(i), bi(i + 1), ti(i), ti(i + 1));
        if ra > 0.0 {
            triangles.push([b0, b1, t0]);
        }
        if rb > 0.0 {
            triangles.push([b1, t1, t0]);
        }
    }
    // caps
    if ra > 0.0 {
        let c = positions.len() as u32;
        positions.push(a);
        for i in 0..n {
            triangles.push([c, bi(i + 1), bi(i)]);
        }
    }
    if rb > 0.0 {
        let c = positions.len() as u32;
        positions.push(b);
        for i in 0..n {
            triangles.push([c, ti(i), ti(i + 1)]);
        }
    }
    Mesh::new(positions, triangles)
}

/// UV sphere.
pub fn sphere(center: Vec3, r: f32, segments: u32, rings: u32) -> Mesh {
    let segments = segments.max(3);
    let rings = rings.max(2);
    let mut positions = vec![center + Vec3::new(0.0, 0.0, -r)];
    for j in 1..rings {
        let phi = std::f32::consts::PI * j as f32 / rings as f32 - std::f32::consts::FRAC_PI_2;
        for i in 0..segments {
            let t = i as f32 / segments as f32 * TAU;
            positions.push(center + Vec3::new(phi.cos() * t.cos() * r, phi.cos() * t.sin() * r, phi.sin() * r));
        }
    }
    positions.push(center + Vec3::new(0.0, 0.0, r));
    let top = positions.len() as u32 - 1;
    let ring = |j: u32, i: u32| 1 + (j - 1) * segments + (i % segments);
    let mut triangles = Vec::new();
    for i in 0..segments {
        triangles.push([0, ring(1, i + 1), ring(1, i)]);
        triangles.push([top, ring(rings - 1, i), ring(rings - 1, i + 1)]);
    }
    for j in 1..rings - 1 {
        for i in 0..segments {
            let (a, b, c, d) = (ring(j, i), ring(j, i + 1), ring(j + 1, i), ring(j + 1, i + 1));
            triangles.push([a, b, d]);
            triangles.push([a, d, c]);
        }
    }
    Mesh::new(positions, triangles)
}

/// Extrudes a CCW polygon between `z0` and `z1`. When `chamfer > 0` the top face is
/// inset by `chamfer` (convex polygons only), giving a raft with sloped edges.
pub fn extrude_convex(poly: &[Vec2], z0: f32, z1: f32, chamfer: f32) -> Mesh {
    let n = poly.len() as u32;
    if n < 3 || z1 <= z0 {
        return Mesh::default();
    }
    let top: Vec<Vec2> = if chamfer > 0.0 {
        crate::geometry::offset_convex(poly, -chamfer)
    } else {
        poly.to_vec()
    };
    let mut positions: Vec<Vec3> = poly.iter().map(|p| p.extend(z0)).collect();
    positions.extend(top.iter().map(|p| p.extend(z1)));
    let mut triangles = Vec::new();
    for i in 0..n {
        let j = (i + 1) % n;
        triangles.push([i, j, n + j]);
        triangles.push([i, n + j, n + i]);
    }
    // caps (fan; valid for convex polygons)
    for i in 1..n - 1 {
        triangles.push([0, i + 1, i]);
        triangles.push([n, n + i, n + i + 1]);
    }
    Mesh::new(positions, triangles)
}

/// Options for [`base_plate`] / [`base_plate_under`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BasePlateOptions {
    /// How far the top of the pad extends beyond the model footprint (mm).
    pub margin: f32,
    /// Pad thickness (mm).
    pub thickness: f32,
    /// Width of the sloped edge (mm): the bottom outline is this much larger
    /// than the top, which eases removal from the plate.
    pub chamfer: f32,
    /// Only model vertices up to this height above the lowest point define the
    /// footprint; `None` uses the projection of the whole model.
    pub footprint_height: Option<f32>,
}

impl Default for BasePlateOptions {
    fn default() -> Self {
        Self {
            margin: 2.0,
            thickness: 1.5,
            chamfer: 1.0,
            footprint_height: None,
        }
    }
}

/// Pad ("platform") under a footprint given by plate-space points: their convex
/// hull grown by `margin` forms the top outline, the bottom outline is grown by
/// another `chamfer`. The pad spans `z = 0 ..= thickness`; it is closed and
/// oriented outward. Returns an empty mesh for invalid options or no points.
pub fn base_plate(points: &[Vec2], opts: &BasePlateOptions) -> Mesh {
    let pts: Vec<Vec2> = points.iter().copied().filter(|p| p.is_finite()).collect();
    if pts.is_empty() || !opts.thickness.is_finite() || opts.thickness <= 0.0 {
        return Mesh::default();
    }
    let margin = opts.margin.max(0.0);
    let chamfer = opts.chamfer.max(0.0);
    let grow = margin + chamfer;
    let hull = crate::geometry::convex_hull(&pts);
    let hull_ok = hull.len() >= 3 && crate::geometry::polygon_area(&hull) > 1e-6;
    let outline = if hull_ok {
        crate::geometry::offset_convex(&hull, grow)
    } else {
        // degenerate footprint (a point or a line): use its bounding box
        let min = pts.iter().fold(Vec2::splat(f32::MAX), |a, b| a.min(*b)) - Vec2::splat(grow.max(0.5));
        let max = pts.iter().fold(Vec2::splat(f32::MIN), |a, b| a.max(*b)) + Vec2::splat(grow.max(0.5));
        vec![min, Vec2::new(max.x, min.y), max, Vec2::new(min.x, max.y)]
    };
    extrude_convex(&outline, 0.0, opts.thickness, chamfer)
}

/// Pad under `mesh` (plate coordinates), see [`base_plate`]. The model itself is
/// not moved: lift it so that it rests on the pad top (`z = thickness`), e.g.
/// with a small overlap so the two fuse.
pub fn base_plate_under(mesh: &Mesh, opts: &BasePlateOptions) -> Mesh {
    let zmin = mesh
        .positions
        .iter()
        .filter(|p| p.is_finite())
        .map(|p| p.z)
        .fold(f32::INFINITY, f32::min);
    let limit = opts.footprint_height.map_or(f32::INFINITY, |h| zmin + h.max(0.0));
    let pts: Vec<Vec2> = mesh
        .positions
        .iter()
        .filter(|p| p.is_finite() && p.z <= limit)
        .map(|p| p.truncate())
        .collect();
    base_plate(&pts, opts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitives_are_closed_and_outward() {
        let meshes = [
            cuboid(Vec3::ZERO, Vec3::ONE),
            frustum(Vec3::ZERO, Vec3::new(0.0, 0.0, 2.0), 1.0, 0.5, 16),
            frustum(Vec3::ZERO, Vec3::new(1.0, 0.0, 2.0), 0.0, 0.5, 16),
            frustum(Vec3::ZERO, Vec3::new(0.0, 0.0, 2.0), 1.0, 0.0, 16),
            sphere(Vec3::ZERO, 1.0, 16, 8),
            extrude_convex(
                &[
                    Vec2::new(0.0, 0.0),
                    Vec2::new(4.0, 0.0),
                    Vec2::new(4.0, 4.0),
                    Vec2::new(0.0, 4.0),
                ],
                0.0,
                1.0,
                0.5,
            ),
        ];
        for (i, m) in meshes.iter().enumerate() {
            assert_eq!(m.open_edge_count(), 0, "mesh {i} not closed");
            assert!(m.signed_volume() > 0.0, "mesh {i} inside out");
        }
        let cyl = frustum(Vec3::ZERO, Vec3::new(0.0, 0.0, 1.0), 1.0, 1.0, 256);
        assert!((cyl.signed_volume() - std::f64::consts::PI).abs() < 1e-3);
    }

    #[test]
    fn base_plates() {
        let o = BasePlateOptions::default();
        let cube = cuboid(Vec3::new(3.0, -2.0, 5.0), Vec3::splat(10.0));
        let pad = base_plate_under(&cube, &o);
        assert_eq!(pad.open_edge_count(), 0);
        assert!(pad.signed_volume() > 0.0);
        let b = pad.bounds();
        assert!((b.min.z - 0.0).abs() < 1e-6 && (b.max.z - o.thickness).abs() < 1e-6);
        // bottom = footprint + margin + chamfer, top = footprint + margin
        assert!(
            (b.size().x - (10.0 + 2.0 * (o.margin + o.chamfer))).abs() < 1e-3,
            "{b:?}"
        );
        let top = 14.0f64 * 14.0;
        let bottom = 16.0f64 * 16.0;
        let expect = o.thickness as f64 / 3.0 * (top + bottom + (top * bottom).sqrt());
        assert!(
            (pad.signed_volume() - expect).abs() < 1.0,
            "{} vs {expect}",
            pad.signed_volume()
        );
        // only the bottom of a sphere defines a small pad
        let s = sphere(Vec3::new(0.0, 0.0, 10.0), 10.0, 32, 16);
        let small = base_plate_under(
            &s,
            &BasePlateOptions {
                footprint_height: Some(1.0),
                ..o.clone()
            },
        );
        let full = base_plate_under(&s, &o);
        assert!(small.bounds().size().x < full.bounds().size().x * 0.6);
        assert_eq!(small.open_edge_count(), 0);
        // degenerate footprints still give a closed pad
        let point = base_plate(&[Vec2::ZERO], &o);
        assert_eq!(point.open_edge_count(), 0);
        assert!(point.signed_volume() > 0.0);
        assert!(base_plate(&[], &o).is_empty());
        assert!(base_plate(&[Vec2::ZERO], &BasePlateOptions { thickness: 0.0, ..o }).is_empty());
    }
}
