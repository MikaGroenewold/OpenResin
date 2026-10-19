//! Plane cuts: splits a mesh into two closed parts with triangulated caps.
//!
//! The cut works for arbitrary planes. Cross-section segments are chained into
//! loops, loops are classified into outer boundaries and holes by their
//! orientation (cavities of hollowed models therefore become holes in the cap)
//! and every outer boundary is triangulated together with the holes it
//! contains. Cap triangles reuse the intersection vertices of the side faces,
//! so a watertight input yields two watertight halves.

use std::collections::HashMap;

use glam::{DVec2, Vec3};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::mesh::Mesh;
use crate::topo::{edge_key, valid_triangle};
use crate::triangulate::{plane_frame, triangulate_rings};

/// A cutting plane given by a point on it and its normal. The normal points
/// towards the "above" half.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CutPlane {
    pub point: Vec3,
    pub normal: Vec3,
}

impl CutPlane {
    pub fn new(point: Vec3, normal: Vec3) -> Self {
        Self { point, normal }
    }

    /// Horizontal plane at height `z` (normal +Z).
    pub fn horizontal(z: f32) -> Self {
        Self {
            point: Vec3::new(0.0, 0.0, z),
            normal: Vec3::Z,
        }
    }
}

/// Result of [`cut`].
#[derive(Debug, Clone, Default)]
pub struct CutResult {
    /// Part on the negative side of the plane (opposite to the normal).
    pub below: Mesh,
    /// Part on the positive side of the plane (where the normal points).
    pub above: Mesh,
    /// Number of cross-section loops (outer boundaries and holes).
    pub cap_loops: usize,
    /// Cross-section chains that did not close (the input was not watertight);
    /// they are capped on a best-effort basis.
    pub open_chains: usize,
    /// Offset (mm, along the normal) applied to the plane so that no vertex lies
    /// exactly on it. Usually zero, otherwise a tiny fraction of the model size.
    pub plane_offset: f32,
}

/// Cuts `mesh` by the plane through `point` with normal `normal`.
/// Returns `(below, above)`, both oriented outward.
pub fn cut_mesh(mesh: &Mesh, point: Vec3, normal: Vec3) -> Result<(Mesh, Mesh)> {
    let r = cut(mesh, &CutPlane::new(point, normal))?;
    Ok((r.below, r.above))
}

/// Cuts `mesh` horizontally at height `z`. Returns `(below, above)`.
pub fn cut_z(mesh: &Mesh, z: f32) -> Result<(Mesh, Mesh)> {
    let r = cut(mesh, &CutPlane::horizontal(z))?;
    Ok((r.below, r.above))
}

#[derive(Clone, Copy)]
enum R {
    /// original vertex
    V(u32),
    /// intersection vertex
    X(u32),
}

/// Cuts `mesh` by `plane`. Inward oriented input is flipped first, so both
/// results are always oriented outward. Invalid triangles are ignored.
pub fn cut(mesh: &Mesh, plane: &CutPlane) -> Result<CutResult> {
    let n = plane.normal.as_dvec3();
    if !n.is_finite() || n.length_squared() < 1e-24 || !plane.point.is_finite() {
        return Err(Error::Invalid(
            "cut plane needs a finite point and a non-zero normal".into(),
        ));
    }
    if mesh.positions.iter().any(|p| !p.is_finite()) {
        return Err(Error::Invalid("mesh contains non-finite vertex positions".into()));
    }
    let n = n.normalize();
    let nv = mesh.positions.len();
    let mut tris: Vec<[u32; 3]> = mesh
        .triangles
        .iter()
        .copied()
        .filter(|t| valid_triangle(t, nv))
        .collect();
    let vol: f64 = tris
        .iter()
        .map(|t| {
            let a = mesh.positions[t[0] as usize].as_dvec3();
            let b = mesh.positions[t[1] as usize].as_dvec3();
            let c = mesh.positions[t[2] as usize].as_dvec3();
            a.dot(b.cross(c))
        })
        .sum();
    if vol < 0.0 {
        for t in &mut tris {
            t.swap(1, 2);
        }
    }
    let p0 = plane.point.as_dvec3();
    let d: Vec<f64> = mesh.positions.iter().map(|p| (p.as_dvec3() - p0).dot(n)).collect();
    let diag = mesh.bounds().size().length() as f64;
    let tol = (diag * 1e-6).max(1e-7);
    let delta = choose_offset(&d, &tris, tol);
    let s: Vec<f64> = d.iter().map(|x| x - delta).collect();
    let above: Vec<bool> = s.iter().map(|&x| x >= 0.0).collect();

    let mut xmap: HashMap<u64, u32> = HashMap::new();
    let mut xpts: Vec<Vec3> = Vec::new();
    let pos = &mesh.positions;
    let mut xid = |i: u32, j: u32| -> u32 {
        *xmap.entry(edge_key(i, j)).or_insert_with(|| {
            let (a, b) = if i < j { (i, j) } else { (j, i) };
            let (sa, sb) = (s[a as usize], s[b as usize]);
            let t = (sa / (sa - sb)).clamp(0.0, 1.0);
            let pa = pos[a as usize].as_dvec3();
            let pb = pos[b as usize].as_dvec3();
            xpts.push((pa + (pb - pa) * t).as_vec3());
            (xpts.len() - 1) as u32
        })
    };

    let mut up: Vec<[R; 3]> = Vec::new();
    let mut down: Vec<[R; 3]> = Vec::new();
    // pieces whose diagonal is chosen once all intersection points exist
    let mut quads: Vec<(bool, [R; 4])> = Vec::new();
    let mut segs: Vec<(u32, u32)> = Vec::new();
    for t in &tris {
        let ab = [above[t[0] as usize], above[t[1] as usize], above[t[2] as usize]];
        let na = ab.iter().filter(|&&a| a).count();
        if na == 3 {
            up.push([R::V(t[0]), R::V(t[1]), R::V(t[2])]);
            continue;
        }
        if na == 0 {
            down.push([R::V(t[0]), R::V(t[1]), R::V(t[2])]);
            continue;
        }
        let lone = if na == 1 {
            ab.iter().position(|&a| a).unwrap()
        } else {
            ab.iter().position(|&a| !a).unwrap()
        };
        let a = t[lone];
        let b = t[(lone + 1) % 3];
        let c = t[(lone + 2) % 3];
        let xab = xid(a, b);
        let xca = xid(c, a);
        let lone_above = ab[lone];
        let piece = [R::V(a), R::X(xab), R::X(xca)];
        if lone_above {
            up.push(piece);
        } else {
            down.push(piece);
        }
        quads.push((!lone_above, [R::X(xab), R::V(b), R::V(c), R::X(xca)]));
        // solid on the left when looking against the normal
        segs.push(if lone_above { (xab, xca) } else { (xca, xab) });
    }
    let rp = |r: R| -> Vec3 {
        match r {
            R::V(i) => pos[i as usize],
            R::X(i) => xpts[i as usize],
        }
    };
    for (to_up, q) in &quads {
        let d1 = (rp(q[0]) - rp(q[2])).length_squared();
        let d2 = (rp(q[1]) - rp(q[3])).length_squared();
        let (t1, t2) = if d1 <= d2 {
            ([q[0], q[1], q[2]], [q[0], q[2], q[3]])
        } else {
            ([q[0], q[1], q[3]], [q[1], q[2], q[3]])
        };
        let half = if *to_up { &mut up } else { &mut down };
        half.push(t1);
        half.push(t2);
    }

    // chain segments into loops
    let nx = xpts.len();
    let mut head = vec![u32::MAX; nx];
    let mut next = vec![u32::MAX; segs.len()];
    for (i, &(a, _)) in segs.iter().enumerate() {
        next[i] = head[a as usize];
        head[a as usize] = i as u32;
    }
    let mut used = vec![false; segs.len()];
    let mut loops: Vec<Vec<u32>> = Vec::new();
    let mut open_chains = 0;
    for i in 0..segs.len() {
        if used[i] || segs[i].0 == segs[i].1 {
            continue;
        }
        used[i] = true;
        let start = segs[i].0;
        let mut lp = vec![start];
        let mut cur = segs[i].1;
        let closed = loop {
            if cur == start {
                break true;
            }
            lp.push(cur);
            let mut k = head[cur as usize];
            while k != u32::MAX && used[k as usize] {
                k = next[k as usize];
            }
            if k == u32::MAX {
                break false;
            }
            used[k as usize] = true;
            cur = segs[k as usize].1;
        };
        if !closed {
            open_chains += 1;
        }
        if lp.len() >= 3 {
            loops.push(lp);
        }
    }

    // classify loops and triangulate the caps
    let (u, v) = plane_frame(n);
    let to2 = |p: Vec3| {
        let p = p.as_dvec3();
        DVec2::new(p.dot(u), p.dot(v))
    };
    let rings: Vec<Vec<DVec2>> = loops
        .iter()
        .map(|lp| lp.iter().map(|&x| to2(xpts[x as usize])).collect())
        .collect();
    let areas: Vec<f64> = rings.iter().map(|r| ring_area(r)).collect();
    let cap = cap_triangles(&loops, &rings, &areas);

    let below = assemble(&down, cap.iter().copied(), pos, &xpts);
    let above_mesh = assemble(&up, cap.iter().map(|t| [t[0], t[2], t[1]]), pos, &xpts);
    Ok(CutResult {
        below,
        above: above_mesh,
        cap_loops: loops.len(),
        open_chains,
        plane_offset: delta as f32,
    })
}

/// Triangulates cross-section loops (given as intersection vertex ids and their
/// 2D projections) into cap triangles facing the plane normal.
fn cap_triangles(loops: &[Vec<u32>], rings: &[Vec<DVec2>], areas: &[f64]) -> Vec<[u32; 3]> {
    let bbox: Vec<(DVec2, DVec2)> = rings
        .iter()
        .map(|r| {
            r.iter().fold(
                (DVec2::splat(f64::INFINITY), DVec2::splat(f64::NEG_INFINITY)),
                |(lo, hi), p| (lo.min(*p), hi.max(*p)),
            )
        })
        .collect();
    let outers: Vec<usize> = (0..rings.len()).filter(|&i| areas[i] > 0.0).collect();
    let mut holes_of: Vec<Vec<usize>> = vec![Vec::new(); rings.len()];
    for h in (0..rings.len()).filter(|&i| areas[i] <= 0.0) {
        // representative point: midpoint of the first edge (robust against touching loops)
        let p = (rings[h][0] + rings[h][1]) * 0.5;
        let container = outers
            .iter()
            .copied()
            .filter(|&o| {
                let (lo, hi) = bbox[o];
                p.x >= lo.x && p.x <= hi.x && p.y >= lo.y && p.y <= hi.y && point_in_ring(p, &rings[o])
            })
            .min_by(|&a, &b| areas[a].total_cmp(&areas[b]));
        if let Some(o) = container {
            holes_of[o].push(h);
        }
        // holes outside every outer boundary only happen for broken input; they stay open
    }
    let mut out = Vec::new();
    for &o in &outers {
        let mut ring_refs: Vec<&[DVec2]> = vec![&rings[o]];
        let mut ids: Vec<u32> = loops[o].clone();
        for &h in &holes_of[o] {
            ring_refs.push(&rings[h]);
            ids.extend_from_slice(&loops[h]);
        }
        let t = triangulate_rings(&ring_refs);
        out.extend(
            t.triangles
                .iter()
                .map(|tr| [ids[tr[0] as usize], ids[tr[1] as usize], ids[tr[2] as usize]]),
        );
    }
    out
}

fn assemble(half: &[[R; 3]], cap: impl Iterator<Item = [u32; 3]>, pos: &[Vec3], xpts: &[Vec3]) -> Mesh {
    let mut vmap = vec![u32::MAX; pos.len()];
    let mut xmap = vec![u32::MAX; xpts.len()];
    let mut m = Mesh::default();
    let mut idx = |r: R, m: &mut Mesh| -> u32 {
        let (slot, p) = match r {
            R::V(i) => (&mut vmap[i as usize], pos[i as usize]),
            R::X(i) => (&mut xmap[i as usize], xpts[i as usize]),
        };
        if *slot == u32::MAX {
            *slot = m.positions.len() as u32;
            m.positions.push(p);
        }
        *slot
    };
    for t in half {
        let tri = [idx(t[0], &mut m), idx(t[1], &mut m), idx(t[2], &mut m)];
        m.triangles.push(tri);
    }
    for t in cap {
        let tri = [
            idx(R::X(t[0]), &mut m),
            idx(R::X(t[1]), &mut m),
            idx(R::X(t[2]), &mut m),
        ];
        if tri[0] != tri[1] && tri[1] != tri[2] && tri[0] != tri[2] {
            m.triangles.push(tri);
        }
    }
    m
}

/// Picks a tiny offset for the plane so that no vertex lies (almost) on it,
/// preferring the offset that crosses the fewest triangles (so a cut exactly at
/// a face leaves that face intact instead of producing a sliver).
fn choose_offset(d: &[f64], tris: &[[u32; 3]], tol: f64) -> f64 {
    let near: Vec<f64> = d.iter().copied().filter(|x| x.abs() < tol * 40.0).collect();
    if near.iter().all(|x| x.abs() >= tol) {
        return 0.0;
    }
    let mut best: Option<(f64, usize)> = None;
    for k in 1..=8 {
        for sign in [1.0, -1.0] {
            let delta = sign * k as f64 * 4.0 * tol;
            if near.iter().any(|x| (x - delta).abs() < tol) {
                continue;
            }
            let crossings = tris
                .iter()
                .filter(|t| {
                    let a = d[t[0] as usize] >= delta;
                    let b = d[t[1] as usize] >= delta;
                    let c = d[t[2] as usize] >= delta;
                    !(a == b && b == c)
                })
                .count();
            if best.is_none_or(|(_, c)| crossings < c) {
                best = Some((delta, crossings));
            }
        }
    }
    best.map(|b| b.0).unwrap_or(0.0)
}

fn ring_area(r: &[DVec2]) -> f64 {
    let n = r.len();
    let mut a = 0.0;
    for i in 0..n {
        a += r[i].perp_dot(r[(i + 1) % n]);
    }
    a * 0.5
}

/// Crossing number point-in-polygon test.
pub(crate) fn point_in_ring(p: DVec2, r: &[DVec2]) -> bool {
    let n = r.len();
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (a, b) = (r[i], r[j]);
        if (a.y > p.y) != (b.y > p.y) {
            let x = a.x + (p.y - a.y) * (b.x - a.x) / (b.y - a.y);
            if p.x < x {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::{cuboid, frustum, sphere};

    fn check_pair(orig: &Mesh, below: &Mesh, above: &Mesh, tol: f64) {
        assert_eq!(below.open_edge_count(), 0, "below not closed");
        assert_eq!(above.open_edge_count(), 0, "above not closed");
        let (vb, va, vo) = (below.signed_volume(), above.signed_volume(), orig.signed_volume());
        assert!(vb >= 0.0 && va >= 0.0, "volumes {vb} {va}");
        assert!((vb + va - vo).abs() < tol * vo.abs().max(1.0), "{vb} + {va} != {vo}");
    }

    #[test]
    fn cube_cut_horizontally() {
        let m = cuboid(Vec3::ZERO, Vec3::splat(2.0));
        let (b, a) = cut_z(&m, 0.3).unwrap();
        check_pair(&m, &b, &a, 1e-5);
        assert!((b.signed_volume() - 1.3 * 4.0).abs() < 1e-4);
        assert!((a.signed_volume() - 0.7 * 4.0).abs() < 1e-4);
        assert!(b.bounds().max.z <= 0.3 + 1e-5 && a.bounds().min.z >= 0.3 - 1e-5);
    }

    #[test]
    fn sphere_cut_by_tilted_plane() {
        let m = sphere(Vec3::new(1.0, 2.0, 3.0), 10.0, 48, 24);
        for (pt, nrm) in [
            (Vec3::new(1.5, 2.0, 3.2), Vec3::new(0.3, -0.4, 1.0)),
            (Vec3::new(1.0, 2.0, 3.0), Vec3::new(1.0, 0.0, 0.0)),
            (Vec3::new(-5.0, 2.0, 3.0), Vec3::new(1.0, 1.0, 1.0)),
        ] {
            let r = cut(&m, &CutPlane::new(pt, nrm)).unwrap();
            assert_eq!(r.open_chains, 0);
            assert_eq!(r.cap_loops, 1);
            check_pair(&m, &r.below, &r.above, 1e-4);
            // every vertex of `below` must be on the negative side
            let nn = nrm.normalize();
            assert!(r.below.positions.iter().all(|p| (*p - pt).dot(nn) <= 1e-3));
            assert!(r.above.positions.iter().all(|p| (*p - pt).dot(nn) >= -1e-3));
        }
    }

    #[test]
    fn cut_through_vertex_ring_and_faces() {
        // the equator ring of a UV sphere with an even ring count lies exactly at the centre
        let m = sphere(Vec3::ZERO, 5.0, 32, 16);
        let r = cut(&m, &CutPlane::horizontal(0.0)).unwrap();
        assert_ne!(r.plane_offset, 0.0);
        check_pair(&m, &r.below, &r.above, 1e-4);
        assert!((r.below.signed_volume() - r.above.signed_volume()).abs() < 0.05);
        // cut exactly at the bottom face of a cube: nothing below, cube intact above
        let c = cuboid(Vec3::new(0.0, 0.0, 5.0), Vec3::splat(10.0));
        let (b, a) = cut_z(&c, 0.0).unwrap();
        assert!(b.is_empty());
        assert_eq!(a.triangle_count(), 12);
        let (b, a) = cut_z(&c, 10.0).unwrap();
        assert!(a.is_empty());
        assert_eq!(b.triangle_count(), 12);
    }

    #[test]
    fn hollow_part_gives_cap_with_hole() {
        let mut m = cuboid(Vec3::new(0.0, 0.0, 5.0), Vec3::new(20.0, 20.0, 10.0));
        let mut inner = cuboid(Vec3::new(1.0, 0.0, 5.0), Vec3::new(10.0, 10.0, 6.0));
        inner.flip();
        m.append(&inner);
        let r = cut(&m, &CutPlane::horizontal(4.0)).unwrap();
        assert_eq!(r.cap_loops, 2);
        check_pair(&m, &r.below, &r.above, 1e-5);
        let expect_below = 20.0 * 20.0 * 4.0 - 10.0 * 10.0 * 2.0;
        assert!(
            (r.below.signed_volume() - expect_below).abs() < 1e-3,
            "{}",
            r.below.signed_volume()
        );
    }

    #[test]
    fn multiple_parts_and_inverted_input() {
        let mut m = sphere(Vec3::new(-10.0, 0.0, 0.0), 4.0, 24, 12);
        m.append(&frustum(
            Vec3::new(10.0, 0.0, -5.0),
            Vec3::new(10.0, 0.0, 5.0),
            3.0,
            1.0,
            20,
        ));
        m.flip(); // inside-out input is corrected
        let r = cut(&m, &CutPlane::new(Vec3::new(0.0, 0.0, 0.5), Vec3::new(0.1, 0.0, 1.0))).unwrap();
        assert_eq!(r.cap_loops, 2);
        let mut o = m.clone();
        o.flip();
        check_pair(&o, &r.below, &r.above, 1e-4);
    }

    #[test]
    fn plane_missing_the_mesh_and_bad_input() {
        let m = cuboid(Vec3::ZERO, Vec3::ONE);
        let (b, a) = cut_z(&m, 5.0).unwrap();
        assert_eq!(b.triangle_count(), 12);
        assert!(a.is_empty());
        assert!(cut(&m, &CutPlane::new(Vec3::ZERO, Vec3::ZERO)).is_err());
        // open input does not panic
        let mut open = m.clone();
        open.triangles.pop();
        let r = cut(&open, &CutPlane::horizontal(0.1)).unwrap();
        assert!(!r.below.is_empty() && !r.above.is_empty());
    }
}
