//! Automatic orientation for resin printing.
//!
//! Many candidate "down" directions (a Fibonacci sphere plus the dominant face
//! normals of the model and the current orientation) are scored by:
//! * **supports** – projected area of downward facing surfaces that need
//!   support (normal within the overhang angle of straight down, not resting on
//!   the plate) converted to a support count, plus local minima (islands),
//! * **height** – print height (print time is proportional to the layer count),
//! * **cross-section** – the largest layer area (peel force),
//! * **flat area** – nearly horizontal downward facing area, used when
//!   [`OrientOptions::prefer_tilt`] is set (large flat surfaces parallel to the
//!   LCD cause suction and visible layer steps),
//! * **contact** – area resting on the plate, rewarded by `FewestSupports` so
//!   that among support-free orientations the largest flat face goes down.
//!
//! Each metric is normalised by its maximum over all candidates and combined
//! with weights that depend on [`OrientGoal`]. A tiny penalty on the rotation
//! angle keeps the current orientation when candidates are equivalent.
//!
//! Plate contact: `FewestSupports` and `LowestHeight` assume the model may rest
//! directly on the plate (or on a pad, see
//! [`primitives::base_plate`](crate::primitives::base_plate)), so faces lying on
//! the plate need no supports. `Balanced` assumes the usual resin workflow of a
//! model lifted on supports, so every downward facing surface counts, including
//! the bottom.
//!
//! Resulting behaviour on simple shapes (see the tests):
//! * `FewestSupports` puts the largest flat face on the plate (a cube rests on a
//!   face and needs no supports).
//! * `LowestHeight` minimises the height (a plate lies flat).
//! * `Balanced` tilts the model so that as little area as possible faces down
//!   flatter than the overhang angle: a cube stands on a corner, a flat plate is
//!   tilted just past the overhang angle (about 45–60°). `prefer_tilt`
//!   additionally penalises surfaces parallel to the plate.

use std::collections::HashMap;

use glam::{Quat, Vec3};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::mesh::Mesh;

/// What the orientation optimises for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OrientGoal {
    /// Compromise between supports, height and peel forces.
    #[default]
    Balanced,
    /// Minimise the number of supports.
    FewestSupports,
    /// Minimise the print height (time).
    LowestHeight,
}

/// Options for [`auto_orient`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OrientOptions {
    pub goal: OrientGoal,
    /// Surfaces facing down that are flatter than this angle (degrees from
    /// horizontal) need supports (same meaning as
    /// [`SupportSettings::overhang_angle`](crate::support::SupportSettings)).
    pub overhang_angle: f32,
    /// Penalise large flat surfaces parallel to the plate (applies to
    /// `Balanced`).
    pub prefer_tilt: bool,
    /// Number of candidate directions on the sphere (dominant face normals and
    /// the current orientation are added).
    pub candidates: usize,
    /// Support spacing (mm) used to turn overhang area into a support count.
    pub support_spacing: f32,
}

impl Default for OrientOptions {
    fn default() -> Self {
        Self {
            goal: OrientGoal::Balanced,
            overhang_angle: 45.0,
            prefer_tilt: true,
            candidates: 400,
            support_spacing: 3.0,
        }
    }
}

/// Score of one candidate orientation (lower `score` is better).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OrientScore {
    /// Direction in model coordinates that ends up pointing down (-Z).
    pub down: Vec3,
    /// Rotation to apply to the model (then drop it onto the plate).
    pub rotation: Quat,
    pub score: f32,
    /// Estimated number of supports.
    pub supports: f32,
    /// Projected area that needs supports (mm²).
    pub overhang_area: f32,
    /// Local minima (islands) that need a support.
    pub minima: u32,
    /// Print height (mm).
    pub height: f32,
    /// Largest cross-section (mm²).
    pub max_section: f32,
    /// Area of nearly horizontal, downward facing surfaces (mm²).
    pub flat_area: f32,
    /// Area resting on the plate (mm²).
    pub contact_area: f32,
}

/// Returns the rotation that puts `mesh` into the best printing orientation.
/// The caller should drop the rotated model onto the plate (or lift it onto
/// supports) afterwards.
pub fn auto_orient(mesh: &Mesh, opts: &OrientOptions) -> Quat {
    rank_orientations(mesh, opts)
        .first()
        .map(|s| s.rotation)
        .unwrap_or(Quat::IDENTITY)
}

struct Face {
    n: Vec3,
    area: f32,
    v: [Vec3; 3],
}

struct Proxy {
    pos: Vec<Vec3>,
    start: Vec<u32>,
    nb: Vec<u32>,
}

struct Prepared {
    faces: Vec<Face>,
    /// sampling weight of every face (stride when the mesh is subsampled)
    weight: f32,
    verts: Vec<Vec3>,
    proxy: Proxy,
    plate_tol: f32,
}

#[derive(Clone, Copy)]
struct Raw {
    supports: f32,
    overhang: f32,
    minima: u32,
    height: f32,
    section: f32,
    flat: f32,
    contact: f32,
}

/// Scores all candidate orientations, best first.
pub fn rank_orientations(mesh: &Mesh, opts: &OrientOptions) -> Vec<OrientScore> {
    let Some(prep) = prepare(mesh) else {
        return Vec::new();
    };
    let mut dirs = fibonacci_sphere(opts.candidates.max(16));
    dirs.extend(dominant_normals(&prep.faces, 24));
    dirs.push(-Vec3::Z);
    let lifted = opts.goal == OrientGoal::Balanced;
    let raws: Vec<Raw> = dirs.par_iter().map(|&d| evaluate(&prep, d, opts, lifted)).collect();
    let max = Raw {
        supports: raws.iter().map(|r| r.supports).fold(0.0, f32::max),
        overhang: 0.0,
        minima: 0,
        height: raws.iter().map(|r| r.height).fold(0.0, f32::max),
        section: raws.iter().map(|r| r.section).fold(0.0, f32::max),
        flat: raws.iter().map(|r| r.flat).fold(0.0, f32::max),
        contact: raws.iter().map(|r| r.contact).fold(0.0, f32::max),
    };
    let mut out: Vec<OrientScore> = dirs
        .iter()
        .zip(&raws)
        .map(|(&d, r)| make_score(d, r, &max, opts))
        .collect();
    out.sort_by(|a, b| a.score.total_cmp(&b.score));
    // local refinement around the winner
    if let Some(best) = out.first().copied() {
        let refined: Vec<OrientScore> = refine_dirs(best.down)
            .into_par_iter()
            .map(|d| make_score(d, &evaluate(&prep, d, opts, lifted), &max, opts))
            .collect();
        out.extend(refined);
        out.sort_by(|a, b| a.score.total_cmp(&b.score));
    }
    out
}

fn make_score(d: Vec3, r: &Raw, max: &Raw, opts: &OrientOptions) -> OrientScore {
    let n = |v: f32, m: f32| if m > 1e-9 { v / m } else { 0.0 };
    // (supports, height, cross-section, flat area, plate contact reward)
    let (ws, wh, wx, wf, wc) = match opts.goal {
        OrientGoal::FewestSupports => (1.0, 0.1, 0.05, 0.0, 0.05),
        OrientGoal::LowestHeight => (0.1, 1.0, 0.05, 0.0, 0.0),
        OrientGoal::Balanced => (0.5, 0.3, 0.2, if opts.prefer_tilt { 0.3 } else { 0.0 }, 0.0),
    };
    let angle = d.dot(-Vec3::Z).clamp(-1.0, 1.0).acos() / std::f32::consts::PI;
    let score = ws * n(r.supports, max.supports)
        + wh * n(r.height, max.height)
        + wx * n(r.section, max.section)
        // relative to the model size, so a tiny flat spot does not dominate
        + wf * n(r.flat, max.section)
        - wc * n(r.contact, max.contact)
        + 0.01 * angle;
    OrientScore {
        down: d,
        rotation: Quat::from_rotation_arc(d, -Vec3::Z),
        score,
        supports: r.supports,
        overhang_area: r.overhang,
        minima: r.minima,
        height: r.height,
        max_section: r.section,
        flat_area: r.flat,
        contact_area: r.contact,
    }
}

fn prepare(mesh: &Mesh) -> Option<Prepared> {
    let nv = mesh.positions.len();
    let valid: Vec<usize> = (0..mesh.triangle_count())
        .filter(|&i| {
            let t = mesh.triangles[i];
            t.iter()
                .all(|&v| (v as usize) < nv && mesh.positions[v as usize].is_finite())
        })
        .collect();
    if valid.is_empty() {
        return None;
    }
    const MAX_FACES: usize = 250_000;
    let stride = valid.len().div_ceil(MAX_FACES).max(1);
    let faces: Vec<Face> = valid
        .iter()
        .step_by(stride)
        .filter_map(|&i| {
            let v = mesh.tri(i);
            let c = (v[1] - v[0]).cross(v[2] - v[0]);
            let l = c.length();
            (l > 0.0).then(|| Face {
                n: c / l,
                area: l * 0.5,
                v,
            })
        })
        .collect();
    if faces.is_empty() {
        return None;
    }
    let verts: Vec<Vec3> = mesh.positions.iter().copied().filter(|p| p.is_finite()).collect();
    let b = mesh.bounds();
    let plate_tol = (b.size().length() * 0.002).max(0.05);
    let total_area: f32 = faces.iter().map(|f| f.area).sum::<f32>() * stride as f32;
    let proxy = build_proxy(mesh, &valid, total_area);
    Some(Prepared {
        faces,
        weight: stride as f32,
        verts,
        proxy,
        plate_tol,
    })
}

/// Vertex-clustered graph (~20k nodes) for local minimum detection.
fn build_proxy(mesh: &Mesh, valid: &[usize], total_area: f32) -> Proxy {
    const TARGET: f32 = 20_000.0;
    let cell = (total_area / TARGET).sqrt().max(1e-4);
    let inv = 1.0 / cell;
    let mut cluster_of = vec![u32::MAX; mesh.positions.len()];
    let mut sums: Vec<(Vec3, u32)> = Vec::new();
    let mut map: HashMap<(i32, i32, i32), u32> = HashMap::new();
    let use_vertices = mesh.positions.len() as f32 <= TARGET * 1.5;
    for &ti in valid {
        for &v in &mesh.triangles[ti] {
            let v = v as usize;
            if cluster_of[v] != u32::MAX {
                continue;
            }
            let p = mesh.positions[v];
            let id = if use_vertices {
                sums.push((Vec3::ZERO, 0));
                (sums.len() - 1) as u32
            } else {
                let key = (
                    (p.x * inv).floor() as i32,
                    (p.y * inv).floor() as i32,
                    (p.z * inv).floor() as i32,
                );
                *map.entry(key).or_insert_with(|| {
                    sums.push((Vec3::ZERO, 0));
                    (sums.len() - 1) as u32
                })
            };
            cluster_of[v] = id;
            sums[id as usize].0 += p;
            sums[id as usize].1 += 1;
        }
    }
    let pos: Vec<Vec3> = sums.iter().map(|(s, c)| *s / (*c).max(1) as f32).collect();
    let mut edges: Vec<(u32, u32)> = Vec::new();
    for &ti in valid {
        let t = mesh.triangles[ti];
        for k in 0..3 {
            let a = cluster_of[t[k] as usize];
            let b = cluster_of[t[(k + 1) % 3] as usize];
            if a != b {
                edges.push((a, b));
                edges.push((b, a));
            }
        }
    }
    edges.sort_unstable();
    edges.dedup();
    let n = pos.len();
    let mut start = vec![0u32; n + 1];
    for &(a, _) in &edges {
        start[a as usize + 1] += 1;
    }
    for i in 0..n {
        start[i + 1] += start[i];
    }
    Proxy {
        pos,
        start,
        nb: edges.iter().map(|e| e.1).collect(),
    }
}

fn evaluate(p: &Prepared, d: Vec3, opts: &OrientOptions, lifted: bool) -> Raw {
    let up = -d;
    let (mut zmin, mut zmax) = (f32::INFINITY, f32::NEG_INFINITY);
    for v in &p.verts {
        let h = v.dot(up);
        zmin = zmin.min(h);
        zmax = zmax.max(h);
    }
    let height = (zmax - zmin).max(0.0);
    let cos_t = opts.overhang_angle.to_radians().cos();
    let cos_flat = 15f32.to_radians().cos();
    const BINS: usize = 64;
    let mut bins = [0f32; BINS];
    let mut overhang = 0f32;
    let mut flat = 0f32;
    let mut contact = 0f32;
    let inv_h = if height > 0.0 { BINS as f32 / height } else { 0.0 };
    for f in &p.faces {
        let nz = f.n.dot(up);
        let h0 = f.v[0].dot(up) - zmin;
        let h1 = f.v[1].dot(up) - zmin;
        let h2 = f.v[2].dot(up) - zmin;
        let on_plate = !lifted && h0.max(h1).max(h2) <= p.plate_tol;
        let proj = f.area * -nz;
        if on_plate {
            if nz < 0.0 {
                contact += proj;
            }
        } else if -nz > cos_t {
            overhang += proj;
        }
        if -nz > cos_flat {
            flat += proj;
        }
        let hc = (h0 + h1 + h2) / 3.0;
        let b = ((hc * inv_h) as usize).min(BINS - 1);
        bins[b] += nz * f.area;
    }
    let w = p.weight;
    let mut section = 0f32;
    let mut acc = 0f32;
    for b in (0..BINS).rev() {
        acc += bins[b];
        section = section.max(acc);
    }
    // local minima on the proxy graph
    let pr = &p.proxy;
    let mut minima_above = 0u32;
    let mut minima_bottom = 0u32;
    for i in 0..pr.pos.len() {
        let (s, e) = (pr.start[i] as usize, pr.start[i + 1] as usize);
        if s == e {
            continue;
        }
        let h = pr.pos[i].dot(up);
        if pr.nb[s..e].iter().all(|&j| pr.pos[j as usize].dot(up) > h) {
            if h - zmin > p.plate_tol {
                minima_above += 1;
            } else {
                minima_bottom += 1;
            }
        }
    }
    let contact = contact * w;
    // a model that does not rest on a face needs a support at its lowest point
    let resting = !lifted && contact > 4.0;
    let minima = minima_above + if resting { 0 } else { minima_bottom.max(1) };
    let overhang = overhang * w;
    let cell = opts.support_spacing.max(0.3).powi(2) * 0.866;
    Raw {
        supports: overhang / cell + minima as f32,
        overhang,
        minima,
        height,
        section: section * w,
        flat: flat * w,
        contact,
    }
}

fn fibonacci_sphere(n: usize) -> Vec<Vec3> {
    let golden = std::f32::consts::PI * (3.0 - 5f32.sqrt());
    (0..n)
        .map(|i| {
            let z = 1.0 - (2.0 * i as f32 + 1.0) / n as f32;
            let r = (1.0 - z * z).max(0.0).sqrt();
            let phi = i as f32 * golden;
            Vec3::new(r * phi.cos(), r * phi.sin(), z)
        })
        .collect()
}

/// Area-weighted dominant face normals (each one is a candidate "down" direction
/// that lays the corresponding flat region onto the plate).
fn dominant_normals(faces: &[Face], count: usize) -> Vec<Vec3> {
    let mut buckets: HashMap<(i32, i32, i32), (Vec3, f32)> = HashMap::new();
    for f in faces {
        let key = (
            (f.n.x * 40.0).round() as i32,
            (f.n.y * 40.0).round() as i32,
            (f.n.z * 40.0).round() as i32,
        );
        let e = buckets.entry(key).or_insert((Vec3::ZERO, 0.0));
        e.0 += f.n * f.area;
        e.1 += f.area;
    }
    let mut v: Vec<(Vec3, f32)> = buckets.into_values().collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1));
    v.into_iter()
        .take(count)
        .filter_map(|(n, _)| {
            let n = n.normalize_or_zero();
            (n != Vec3::ZERO).then_some(n)
        })
        .collect()
}

/// Directions within ~8 degrees around `d`.
fn refine_dirs(d: Vec3) -> Vec<Vec3> {
    let (u, v) = d.any_orthonormal_pair();
    let mut out = Vec::new();
    for ring in 1..=3 {
        let a = (ring as f32 * 2.7f32).to_radians();
        for k in 0..8 {
            let phi = k as f32 / 8.0 * std::f32::consts::TAU + ring as f32 * 0.4;
            out.push((d * a.cos() + (u * phi.cos() + v * phi.sin()) * a.sin()).normalize());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::{cuboid, frustum, sphere};

    fn rotated_height(m: &Mesh, q: Quat) -> f32 {
        m.transformed(&glam::Mat4::from_quat(q)).bounds().size().z
    }

    /// cargo test --release -p openresin-core -- --ignored orient_timing --nocapture
    #[test]
    #[ignore]
    fn orient_timing() {
        let mut m = sphere(Vec3::ZERO, 30.0, 512, 256);
        m.append(&cuboid(Vec3::new(0.0, 0.0, -30.0), Vec3::new(40.0, 40.0, 4.0)));
        let t = std::time::Instant::now();
        let r = rank_orientations(&m, &OrientOptions::default());
        eprintln!(
            "orient: {} tris, {} candidates, {:.2?}",
            m.triangle_count(),
            r.len(),
            t.elapsed()
        );
    }

    #[test]
    fn plate_lies_flat_for_lowest_height() {
        // a plate standing on its thin edge
        let plate = cuboid(Vec3::ZERO, Vec3::new(40.0, 2.0, 30.0));
        let o = OrientOptions {
            goal: OrientGoal::LowestHeight,
            ..Default::default()
        };
        let q = auto_orient(&plate, &o);
        assert!(
            (rotated_height(&plate, q) - 2.0).abs() < 0.05,
            "{}",
            rotated_height(&plate, q)
        );
    }

    #[test]
    fn cube_rests_on_face_for_fewest_supports() {
        let cube = cuboid(Vec3::ZERO, Vec3::splat(20.0));
        // start from an arbitrary rotation
        let start = Quat::from_euler(glam::EulerRot::XYZ, 0.4, 0.7, 0.2);
        let m = cube.transformed(&glam::Mat4::from_quat(start));
        let o = OrientOptions {
            goal: OrientGoal::FewestSupports,
            ..Default::default()
        };
        let best = rank_orientations(&m, &o)[0];
        assert!((rotated_height(&m, best.rotation) - 20.0).abs() < 0.05);
        assert!(best.supports < 0.5, "{best:?}");
        assert!(best.contact_area > 399.0);
        let o = OrientOptions {
            goal: OrientGoal::LowestHeight,
            ..Default::default()
        };
        assert!((rotated_height(&m, auto_orient(&m, &o)) - 20.0).abs() < 0.05);
        // lifted on supports (balanced), resting on a face would need a fully supported bottom
        for prefer_tilt in [false, true] {
            let o = OrientOptions {
                prefer_tilt,
                ..Default::default()
            };
            let best = rank_orientations(&m, &o)[0];
            assert!(best.height > 25.0, "{best:?}");
            assert!(best.supports < 5.0, "{best:?}");
        }
    }

    #[test]
    fn balanced_with_tilt_avoids_flat_faces() {
        let cube = cuboid(Vec3::ZERO, Vec3::splat(20.0));
        let best = rank_orientations(&cube, &OrientOptions::default())[0];
        // no face parallel to the plate: standing on a corner or an edge
        assert!(best.flat_area < 1.0, "{best:?}");
        assert!(best.height > 25.0);
        let plate = cuboid(Vec3::ZERO, Vec3::new(40.0, 40.0, 2.0));
        let best = rank_orientations(&plate, &OrientOptions::default())[0];
        let tilt = best.down.dot(-Vec3::Z).abs().acos().to_degrees();
        // the big face is tilted, steep enough to need (almost) no overhang supports
        assert!((40.0..=75.0).contains(&tilt), "tilt {tilt} {best:?}");
        assert!(best.overhang_area < 100.0, "{best:?}");
    }

    #[test]
    fn already_good_orientation_is_kept_and_degenerate_input() {
        // sphere: all orientations are (nearly) equivalent
        let s = sphere(Vec3::ZERO, 10.0, 48, 24);
        let ranked = rank_orientations(&s, &OrientOptions::default());
        let ident = ranked.iter().find(|r| r.down == -Vec3::Z).unwrap();
        assert!(ident.score - ranked[0].score < 0.03, "{ident:?} vs {:?}", ranked[0]);
        assert_eq!(auto_orient(&Mesh::default(), &OrientOptions::default()), Quat::IDENTITY);
        // cone standing on its base keeps standing for fewest supports
        let cone = frustum(Vec3::ZERO, Vec3::new(0.0, 0.0, 20.0), 10.0, 0.0, 48);
        let o = OrientOptions {
            goal: OrientGoal::FewestSupports,
            ..Default::default()
        };
        let q = auto_orient(&cone, &o);
        let r = cone.transformed(&glam::Mat4::from_quat(q));
        assert!((r.bounds().size().z - 20.0).abs() < 0.1);
        let c = r.bounds().center();
        // base down: most volume near the bottom
        let low = r.positions.iter().filter(|p| p.z < c.z).count();
        assert!(low as f32 > r.positions.len() as f32 * 0.4);
    }
}
