//! Hollowing: computes the inner "cavity" solid of a model for a given wall
//! thickness (to be subtracted by the slicer, see
//! [`Slicer::with_negatives`](crate::slicer::Slicer::with_negatives)), and drain
//! hole geometry.
//!
//! Pipeline:
//! 1. voxelize inside/outside with vertical rays (nonzero winding, so
//!    overlapping shells are handled like the slicer handles them),
//! 2. separable Felzenszwalb–Huttenlocher feature transform: every inside voxel
//!    learns its nearest boundary voxel, whose exact closest point on the mesh
//!    gives a near-exact Euclidean distance to the surface,
//! 3. iso-surface `distance = wall thickness` via marching cubes with a per-face
//!    asymptotic decider (always closed and consistently oriented),
//! 4. light Taubin smoothing and removal of tiny cavity fragments.

use std::collections::HashMap;

use glam::Vec3;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::bvh::{closest_point_triangle, Bvh};
use crate::error::{Error, Result};
use crate::mesh::Mesh;
use crate::primitives::frustum;
use crate::support::VerticalRayIndex;
use crate::topo::{components_by_vertex, signed_volume_of};

/// Options for [`hollow`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HollowOptions {
    /// Minimum wall thickness in mm.
    pub wall_thickness: f32,
    /// Voxel edge length in mm; `0` chooses it automatically (wall / 5, coarser
    /// only when the grid would exceed `max_voxels`).
    pub voxel_size: f32,
    /// Upper bound for the number of voxels.
    pub max_voxels: usize,
    /// Taubin smoothing iterations applied to the cavity surface.
    pub smoothing_iterations: u32,
    /// Cavity fragments smaller than this volume (mm³) are dropped.
    pub min_cavity_volume: f32,
}

impl Default for HollowOptions {
    fn default() -> Self {
        Self {
            wall_thickness: 2.0,
            voxel_size: 0.0,
            max_voxels: 30_000_000,
            smoothing_iterations: 3,
            min_cavity_volume: 1.0,
        }
    }
}

/// Result of [`hollow_with_info`].
#[derive(Debug, Clone, Default)]
pub struct HollowResult {
    /// Closed, outward oriented cavity solid (empty when the model is too thin).
    pub cavity: Mesh,
    /// Voxel size that was used (mm).
    pub voxel_size: f32,
    /// Grid dimensions `[nx, ny, nz]`.
    pub grid: [usize; 3],
    /// Volume of the cavity in mm³.
    pub cavity_volume: f64,
}

/// Computes the cavity of `mesh` (a closed model in any coordinates) for the
/// given wall thickness. The returned mesh is closed and oriented outward;
/// subtract it with [`Slicer::with_negatives`](crate::slicer::Slicer::with_negatives).
pub fn hollow(mesh: &Mesh, opts: &HollowOptions) -> Result<Mesh> {
    Ok(hollow_with_info(mesh, opts)?.cavity)
}

/// Closed cylinder for a drain hole. `position` is the point on the model
/// surface, `direction` points into the model (e.g. the negated surface
/// normal). The cylinder starts `diameter / 2` outside the surface (so the
/// opening is clean on curved surfaces) and reaches `depth` mm into the model.
/// Returns an empty mesh for invalid parameters.
pub fn drain_hole(position: Vec3, direction: Vec3, diameter: f32, depth: f32) -> Mesh {
    let d = direction.normalize_or_zero();
    if d == Vec3::ZERO || !positive(diameter) || !positive(depth) || !position.is_finite() {
        return Mesh::default();
    }
    let r = diameter * 0.5;
    let start = position - d * r;
    let end = position + d * depth;
    let segments = ((diameter * std::f32::consts::PI / 0.25).ceil() as u32).clamp(16, 96);
    frustum(start, end, r, r, segments)
}

/// Finite and strictly positive (rejects NaN).
fn positive(x: f32) -> bool {
    x.is_finite() && x > 0.0
}

const NONE: u32 = u32::MAX;

#[derive(Clone, Copy)]
struct SyncPtr(*mut u32);
// SAFETY: only used to write disjoint lines of one buffer from parallel tasks.
unsafe impl Send for SyncPtr {}
unsafe impl Sync for SyncPtr {}
impl SyncPtr {
    #[inline]
    fn get(self) -> *mut u32 {
        self.0
    }
}

struct Grid {
    origin: Vec3,
    h: f32,
    nx: usize,
    ny: usize,
    nz: usize,
}

impl Grid {
    #[inline]
    fn lin(&self, i: usize, j: usize, k: usize) -> usize {
        (i * self.ny + j) * self.nz + k
    }
    #[inline]
    fn center(&self, i: usize, j: usize, k: usize) -> Vec3 {
        self.origin + Vec3::new(i as f32 + 0.5, j as f32 + 0.5, k as f32 + 0.5) * self.h
    }
    #[inline]
    fn decode(&self, l: usize) -> (usize, usize, usize) {
        let k = l % self.nz;
        let r = l / self.nz;
        (r / self.ny, r % self.ny, k)
    }
    fn len(&self) -> usize {
        self.nx * self.ny * self.nz
    }
}

/// Like [`hollow`], with additional information about the voxel grid.
pub fn hollow_with_info(mesh: &Mesh, opts: &HollowOptions) -> Result<HollowResult> {
    let wall = opts.wall_thickness;
    if !positive(wall) {
        return Err(Error::Invalid("wall thickness must be positive".into()));
    }
    if mesh.is_empty() {
        return Err(Error::Invalid("cannot hollow an empty mesh".into()));
    }
    if mesh.positions.iter().any(|p| !p.is_finite())
        || mesh
            .triangles
            .iter()
            .flatten()
            .any(|&i| i as usize >= mesh.positions.len())
    {
        return Err(Error::Invalid("mesh contains invalid vertices or indices".into()));
    }
    let b = mesh.bounds();
    let size = b.size();
    let pad = 2usize;
    let dims = |h: f32| -> [usize; 3] {
        [
            (size.x / h).ceil() as usize + 2 * pad,
            (size.y / h).ceil() as usize + 2 * pad,
            (size.z / h).ceil() as usize + 2 * pad,
        ]
    };
    let budget = opts.max_voxels.max(1000);
    let mut h = if opts.voxel_size > 0.0 {
        opts.voxel_size
    } else {
        wall / 5.0
    };
    let need = |h: f32| {
        let d = dims(h);
        d[0] as f64 * d[1] as f64 * d[2] as f64
    };
    if need(h) > budget as f64 {
        h = h.max(((size.x as f64 * size.y as f64 * size.z as f64) / budget as f64).cbrt() as f32);
        while need(h) > budget as f64 {
            h *= 1.05;
        }
    }
    let [nx, ny, nz] = dims(h);
    let grid = Grid {
        origin: b.min - Vec3::splat(pad as f32 * h),
        h,
        nx,
        ny,
        nz,
    };
    let mut result = HollowResult {
        voxel_size: h,
        grid: [nx, ny, nz],
        ..Default::default()
    };
    // Too thin for any cavity?
    if size.min_element() <= 2.0 * wall {
        return Ok(result);
    }

    // 1) inside classification: 0 = outside, 1 = inside, 2 = boundary site
    let idx = VerticalRayIndex::new(mesh);
    let mut class = classify(&idx, &grid);
    let sites = find_sites(&grid, &mut class);
    if sites.is_empty() {
        return Ok(result);
    }
    let cps = closest_points(mesh, &grid, &sites);

    // 2) feature transform
    let mut feat = vec![NONE; grid.len()];
    feature_transform(&grid, &class, &mut feat);

    // 3) distance field (stored as f32 bits in the same buffer). The feature
    // transform value is an upper bound of the true distance (within about one
    // voxel); grid points near the iso level get the exact distance.
    let boundary = boundary_surface(mesh, &idx, h * 1e-2);
    let bvh = Bvh::new(boundary.as_ref().unwrap_or(mesh));
    let (band_lo, band_hi) = (wall - 1.5 * h, wall + 3.0 * h);
    let field = &mut feat;
    field.par_chunks_mut(ny * nz).enumerate().for_each(|(i, slab)| {
        for j in 0..ny {
            for k in 0..nz {
                let l = j * nz + k;
                let gl = grid.lin(i, j, k);
                let d = if class[gl] == 1 && slab[l] != NONE {
                    let site = slab[l];
                    let cp = match sites.binary_search(&site) {
                        Ok(s) => cps[s],
                        Err(_) => {
                            let (a, b2, c) = grid.decode(site as usize);
                            grid.center(a, b2, c)
                        }
                    };
                    let p = grid.center(i, j, k);
                    let d = (p - cp).length();
                    if d >= band_lo && d <= band_hi {
                        bvh.closest_point(p, d * 1.0001 + 1e-4).map_or(d, |r| r.1)
                    } else {
                        d
                    }
                } else {
                    0.0
                };
                slab[l] = d.to_bits();
            }
        }
    });
    drop(class);

    // 4) iso-surface
    let mut cavity = marching_cubes(&grid, field, wall);
    drop(feat);
    if cavity.is_empty() {
        return Ok(result);
    }
    taubin_smooth(&mut cavity, opts.smoothing_iterations);
    // 5) drop tiny fragments
    let (comp, count) = components_by_vertex(&cavity);
    if count > 1 || opts.min_cavity_volume > 0.0 {
        let mut groups: Vec<Vec<[u32; 3]>> = vec![Vec::new(); count];
        for (t, &c) in cavity.triangles.iter().zip(&comp) {
            groups[c as usize].push(*t);
        }
        let keep: Vec<bool> = groups
            .iter()
            .map(|g| signed_volume_of(&cavity.positions, g).abs() >= opts.min_cavity_volume as f64)
            .collect();
        let tris: Vec<[u32; 3]> = cavity
            .triangles
            .iter()
            .zip(&comp)
            .filter(|(_, &c)| keep[c as usize])
            .map(|(t, _)| *t)
            .collect();
        cavity = crate::topo::submesh(&cavity.positions, tris);
    }
    if cavity.signed_volume() < 0.0 {
        cavity.flip();
    }
    result.cavity_volume = cavity.signed_volume();
    result.cavity = cavity;
    Ok(result)
}

/// Removes triangles that lie inside the solid (internal faces of overlapping
/// shells): the point just outside such a triangle has a non-zero winding
/// number. Returns `None` when every triangle is on the boundary.
fn boundary_surface(mesh: &Mesh, idx: &VerticalRayIndex, eps: f32) -> Option<Mesh> {
    let keep: Vec<bool> = (0..mesh.triangle_count())
        .into_par_iter()
        .map(|i| {
            let [a, b, c] = mesh.tri(i);
            let n = (b - a).cross(c - a).normalize_or_zero();
            if n == Vec3::ZERO {
                return true;
            }
            let q = (a + b + c) / 3.0 + n * eps;
            let w: i32 = idx
                .hits(q.x, q.y)
                .iter()
                .filter(|h| h.z > q.z)
                .map(|h| if h.normal.z > 0.0 { 1 } else { -1 })
                .sum();
            w == 0
        })
        .collect();
    if keep.iter().all(|&k| k) {
        return None;
    }
    let tris = mesh.triangles.iter().zip(&keep).filter(|(_, &k)| k).map(|(t, _)| *t);
    Some(crate::topo::submesh(&mesh.positions, tris))
}

/// Column-wise nonzero winding classification of voxel centres.
fn classify(idx: &VerticalRayIndex, g: &Grid) -> Vec<u8> {
    let mut class = vec![0u8; g.len()];
    // tiny irrational offset so rays avoid vertices/edges of grid-aligned models
    let jx = g.h * 1.37e-3;
    let jy = g.h * 0.71e-3;
    class.par_chunks_mut(g.nz).enumerate().for_each(|(col, out)| {
        let i = col / g.ny;
        let j = col % g.ny;
        let c = g.center(i, j, 0);
        let hits = idx.hits(c.x + jx, c.y + jy);
        if hits.is_empty() {
            return;
        }
        let total: i32 = hits.iter().map(|h| if h.normal.z < 0.0 { 1 } else { -1 }).sum();
        if total != 0 {
            return; // open surface along this column: leave it solid (no cavity)
        }
        let mut w = 0i32;
        let mut hi = 0;
        for (k, o) in out.iter_mut().enumerate() {
            let z = g.origin.z + (k as f32 + 0.5) * g.h;
            while hi < hits.len() && hits[hi].z < z {
                w += if hits[hi].normal.z < 0.0 { 1 } else { -1 };
                hi += 1;
            }
            *o = (w != 0) as u8;
        }
    });
    class
}

/// Outside voxels with an inside voxel among their 26 neighbours. They are the
/// only possible nearest outside voxels of inside voxels. Marks them with 2 and
/// returns their (sorted) linear indices.
fn find_sites(g: &Grid, class: &mut [u8]) -> Vec<u32> {
    let (ny, nz) = (g.ny, g.nz);
    let cl: &[u8] = class;
    let per_slab: Vec<Vec<u32>> = (0..g.nx)
        .into_par_iter()
        .map(|i| {
            let mut out = Vec::new();
            for j in 0..ny {
                for k in 0..nz {
                    if cl[g.lin(i, j, k)] != 0 {
                        continue;
                    }
                    let mut site = false;
                    'n: for di in -1i64..=1 {
                        let ii = i as i64 + di;
                        if ii < 0 || ii >= g.nx as i64 {
                            continue;
                        }
                        for dj in -1i64..=1 {
                            let jj = j as i64 + dj;
                            if jj < 0 || jj >= ny as i64 {
                                continue;
                            }
                            for dk in -1i64..=1 {
                                let kk = k as i64 + dk;
                                if kk < 0 || kk >= nz as i64 {
                                    continue;
                                }
                                if cl[g.lin(ii as usize, jj as usize, kk as usize)] == 1 {
                                    site = true;
                                    break 'n;
                                }
                            }
                        }
                    }
                    if site {
                        out.push(g.lin(i, j, k) as u32);
                    }
                }
            }
            out
        })
        .collect();
    let sites: Vec<u32> = per_slab.concat();
    for &s in &sites {
        class[s as usize] = 2;
    }
    sites
}

/// Exact closest points on the mesh for every site (voxel centre).
fn closest_points(mesh: &Mesh, g: &Grid, sites: &[u32]) -> Vec<Vec3> {
    let cs = 2.0 * g.h;
    let inv = 1.0 / cs;
    let cell_of = |p: Vec3| -> (i64, i64, i64) {
        (
            ((p.x - g.origin.x) * inv).floor() as i64,
            ((p.y - g.origin.y) * inv).floor() as i64,
            ((p.z - g.origin.z) * inv).floor() as i64,
        )
    };
    let key = |c: (i64, i64, i64)| -> u64 {
        ((c.0 as u64 & 0x1f_ffff) << 42) | ((c.1 as u64 & 0x1f_ffff) << 21) | (c.2 as u64 & 0x1f_ffff)
    };
    let half_diag = cs * 0.8660254;
    let mut pairs: Vec<(u64, u32)> = (0..mesh.triangle_count())
        .into_par_iter()
        .flat_map_iter(|ti| {
            let [a, b, c] = mesh.tri(ti);
            let n = (b - a).cross(c - a).normalize_or_zero();
            let lo = cell_of(a.min(b).min(c));
            let hi = cell_of(a.max(b).max(c));
            let mut out = Vec::new();
            for x in lo.0..=hi.0 {
                for y in lo.1..=hi.1 {
                    for z in lo.2..=hi.2 {
                        let centre = g.origin + Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5) * cs;
                        if n == Vec3::ZERO || (centre - a).dot(n).abs() <= half_diag {
                            out.push((key((x, y, z)), ti as u32));
                        }
                    }
                }
            }
            out
        })
        .collect();
    pairs.par_sort_unstable();
    sites
        .par_iter()
        .map(|&s| {
            let (i, j, k) = g.decode(s as usize);
            let p = g.center(i, j, k);
            let c = cell_of(p);
            let mut best = (f32::INFINITY, p);
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        let kk = key((c.0 + dx, c.1 + dy, c.2 + dz));
                        let start = pairs.partition_point(|e| e.0 < kk);
                        for e in pairs[start..].iter().take_while(|e| e.0 == kk) {
                            let [a, b, cc] = mesh.tri(e.1 as usize);
                            let q = closest_point_triangle(p, a, b, cc);
                            let d = (q - p).length_squared();
                            if d < best.0 {
                                best = (d, q);
                            }
                        }
                    }
                }
            }
            best.1
        })
        .collect()
}

/// Lower envelope of parabolas `(x - q)^2 + f(q)` (Felzenszwalb & Huttenlocher);
/// writes the minimizing site for every position.
struct Envelope {
    v: Vec<usize>,
    fv: Vec<f64>,
    z: Vec<f64>,
}

impl Envelope {
    fn new() -> Self {
        Self {
            v: Vec::new(),
            fv: Vec::new(),
            z: Vec::new(),
        }
    }

    fn argmin(&mut self, f: &[f64], out: &mut [usize]) -> bool {
        self.v.clear();
        self.fv.clear();
        self.z.clear();
        for (q, &fq) in f.iter().enumerate() {
            if !fq.is_finite() {
                continue;
            }
            let qf = q as f64;
            loop {
                let Some(&p) = self.v.last() else {
                    self.v.push(q);
                    self.fv.push(fq);
                    self.z.push(f64::NEG_INFINITY);
                    break;
                };
                let pf = p as f64;
                let fp = *self.fv.last().unwrap();
                let s = ((fq + qf * qf) - (fp + pf * pf)) / (2.0 * (qf - pf));
                if s <= *self.z.last().unwrap() {
                    self.v.pop();
                    self.fv.pop();
                    self.z.pop();
                    continue;
                }
                self.v.push(q);
                self.fv.push(fq);
                self.z.push(s);
                break;
            }
        }
        if self.v.is_empty() {
            return false;
        }
        let mut j = 0;
        for (q, o) in out.iter_mut().enumerate() {
            while j + 1 < self.v.len() && self.z[j + 1] <= q as f64 {
                j += 1;
            }
            *o = self.v[j];
        }
        true
    }
}

/// Separable exact Euclidean feature transform: afterwards `feat[p]` holds the
/// linear index of the nearest site voxel of `p` (or `NONE`).
fn feature_transform(g: &Grid, class: &[u8], feat: &mut [u32]) {
    let (nx, ny, nz) = (g.nx, g.ny, g.nz);
    // pass 1: along z (contiguous columns), nearest site by two sweeps
    feat.par_chunks_mut(nz).enumerate().for_each(|(col, out)| {
        let base = col * nz;
        let mut last = NONE;
        for k in 0..nz {
            if class[base + k] == 2 {
                last = (base + k) as u32;
            }
            out[k] = last;
        }
        let mut next = NONE;
        for k in (0..nz).rev() {
            if class[base + k] == 2 {
                next = (base + k) as u32;
            }
            if next != NONE {
                let dn = (next as usize - (base + k)) as i64;
                let dl = if out[k] == NONE {
                    i64::MAX
                } else {
                    ((base + k) - out[k] as usize) as i64
                };
                if dn < dl {
                    out[k] = next;
                }
            }
        }
    });
    // pass 2: along y within each x slab
    feat.par_chunks_mut(ny * nz).for_each(|slab| {
        let mut env = Envelope::new();
        let mut f = vec![0f64; ny];
        let mut arg = vec![0usize; ny];
        let mut tmp = vec![0u32; ny];
        for k in 0..nz {
            for j in 0..ny {
                let s = slab[j * nz + k];
                tmp[j] = s;
                f[j] = if s == NONE {
                    f64::INFINITY
                } else {
                    let ks = s as usize % nz;
                    let d = ks as f64 - k as f64;
                    d * d
                };
            }
            if env.argmin(&f, &mut arg) {
                for j in 0..ny {
                    slab[j * nz + k] = tmp[arg[j]];
                }
            }
        }
    });
    // pass 3: along x; lines (j, k) are disjoint index sets of the buffer
    let ptr = SyncPtr(feat.as_mut_ptr());
    let stride = ny * nz;
    (0..ny).into_par_iter().for_each(|j| {
        let base = ptr.get();
        let mut env = Envelope::new();
        let mut f = vec![0f64; nx];
        let mut arg = vec![0usize; nx];
        let mut tmp = vec![0u32; nx];
        for k in 0..nz {
            let l = j * nz + k;
            for i in 0..nx {
                // SAFETY: index i * stride + l belongs to line (j, k) only.
                let s = unsafe { *base.add(i * stride + l) };
                tmp[i] = s;
                f[i] = if s == NONE {
                    f64::INFINITY
                } else {
                    let (_, js, ks) = g.decode(s as usize);
                    let dj = js as f64 - j as f64;
                    let dk = ks as f64 - k as f64;
                    dj * dj + dk * dk
                };
            }
            if env.argmin(&f, &mut arg) {
                for i in 0..nx {
                    // SAFETY: see above.
                    unsafe { *base.add(i * stride + l) = tmp[arg[i]] };
                }
            }
        }
    });
}

/// Cube corners `(dx, dy, dz)` indexed by `dx | dy << 1 | dz << 2`.
const FACES: [[usize; 4]; 6] = [
    [0, 4, 6, 2], // -x
    [1, 3, 7, 5], // +x
    [0, 1, 5, 4], // -y
    [2, 6, 7, 3], // +y
    [0, 2, 3, 1], // -z
    [4, 5, 7, 6], // +z
];

/// Marching cubes on `field - iso` (cavity where the field is larger than
/// `iso`). Every cube face connects its crossing points with a rule that only
/// depends on the face's own corner values (asymptotic decider), so adjacent
/// cubes always agree and the surface is closed. Output is oriented outward.
fn marching_cubes(g: &Grid, field: &[u32], iso: f32) -> Mesh {
    let (nx, ny, nz) = (g.nx, g.ny, g.nz);
    let val = |i: usize, j: usize, k: usize| f32::from_bits(field[g.lin(i, j, k)]) - iso;
    type SlabOut = (Vec<(u64, Vec3)>, Vec<[u64; 3]>);
    let slabs: Vec<SlabOut> = (0..nx.saturating_sub(1))
        .into_par_iter()
        .map(|i| {
            let mut verts: Vec<(u64, Vec3)> = Vec::new();
            let mut tris: Vec<[u64; 3]> = Vec::new();
            let mut segs: Vec<(u64, u64)> = Vec::with_capacity(16);
            let mut lp: Vec<u64> = Vec::with_capacity(16);
            for j in 0..ny - 1 {
                for k in 0..nz - 1 {
                    let mut gv = [0f32; 8];
                    let mut any_in = false;
                    let mut any_out = false;
                    for (c, v) in gv.iter_mut().enumerate() {
                        *v = val(i + (c & 1), j + ((c >> 1) & 1), k + ((c >> 2) & 1));
                        if *v > 0.0 {
                            any_in = true;
                        } else {
                            any_out = true;
                        }
                    }
                    if !(any_in && any_out) {
                        continue;
                    }
                    let edge_key = |a: usize, b: usize| -> u64 {
                        let (a, b) = if a < b { (a, b) } else { (b, a) };
                        let axis = (a ^ b).trailing_zeros() as u64;
                        let base = g.lin(i + (a & 1), j + ((a >> 1) & 1), k + ((a >> 2) & 1)) as u64;
                        base * 3 + axis
                    };
                    let add_vertex = |a: usize, b: usize, verts: &mut Vec<(u64, Vec3)>| -> u64 {
                        let key = edge_key(a, b);
                        let (ga, gb) = (gv[a], gv[b]);
                        let t = (ga / (ga - gb)).clamp(1e-3, 1.0 - 1e-3);
                        let pa = g.center(i + (a & 1), j + ((a >> 1) & 1), k + ((a >> 2) & 1));
                        let pb = g.center(i + (b & 1), j + ((b >> 1) & 1), k + ((b >> 2) & 1));
                        verts.push((key, pa + (pb - pa) * t));
                        key
                    };
                    segs.clear();
                    for face in FACES {
                        let s = face.map(|c| gv[c] > 0.0);
                        let n_in = s.iter().filter(|&&x| x).count();
                        if n_in == 0 || n_in == 4 {
                            continue;
                        }
                        // exits (inside -> outside) and entries along the CCW walk
                        let mut exits = [(0usize, 0usize); 2];
                        let mut entries = [(0usize, 0usize); 2];
                        let (mut ne, mut nn) = (0, 0);
                        for e in 0..4 {
                            let (a, b) = (e, (e + 1) % 4);
                            if s[a] && !s[b] {
                                exits[ne] = (a, b);
                                ne += 1;
                            } else if !s[a] && s[b] {
                                entries[nn] = (a, b);
                                nn += 1;
                            }
                        }
                        if ne == 1 {
                            let x = add_vertex(face[exits[0].0], face[exits[0].1], &mut verts);
                            let y = add_vertex(face[entries[0].0], face[entries[0].1], &mut verts);
                            segs.push((x, y));
                        } else {
                            // ambiguous face: rotate so that corner 0 is inside
                            let r = if s[0] { 0 } else { 1 };
                            let q = [face[r], face[(r + 1) % 4], face[(r + 2) % 4], face[(r + 3) % 4]];
                            let (a, b, c, d) = (gv[q[0]], gv[q[1]], gv[q[2]], gv[q[3]]);
                            let saddle = (a * c - b * d) / (a + c - b - d);
                            let x1 = add_vertex(q[0], q[1], &mut verts);
                            let y1 = add_vertex(q[1], q[2], &mut verts);
                            let x2 = add_vertex(q[2], q[3], &mut verts);
                            let y2 = add_vertex(q[3], q[0], &mut verts);
                            if saddle > 0.0 {
                                segs.push((x1, y1));
                                segs.push((x2, y2));
                            } else {
                                segs.push((x1, y2));
                                segs.push((x2, y1));
                            }
                        }
                    }
                    // chain segments into loops and fan triangulate (reversed: outward)
                    while let Some((start, mut cur)) = segs.pop() {
                        lp.clear();
                        lp.push(start);
                        let mut guard = 0;
                        while cur != start && guard < 16 {
                            lp.push(cur);
                            match segs.iter().position(|s| s.0 == cur) {
                                Some(p) => cur = segs.swap_remove(p).1,
                                None => break,
                            }
                            guard += 1;
                        }
                        for t in 1..lp.len().saturating_sub(1) {
                            tris.push([lp[0], lp[t + 1], lp[t]]);
                        }
                    }
                }
            }
            (verts, tris)
        })
        .collect();
    let total_v: usize = slabs.iter().map(|s| s.0.len()).sum();
    let mut map: HashMap<u64, u32> = HashMap::with_capacity(total_v / 3 + 16);
    let mut mesh = Mesh::default();
    for (verts, _) in &slabs {
        for &(key, p) in verts {
            map.entry(key).or_insert_with(|| {
                mesh.positions.push(p);
                (mesh.positions.len() - 1) as u32
            });
        }
    }
    for (_, tris) in &slabs {
        for t in tris {
            let tri = [map[&t[0]], map[&t[1]], map[&t[2]]];
            if tri[0] != tri[1] && tri[1] != tri[2] && tri[0] != tri[2] {
                mesh.triangles.push(tri);
            }
        }
    }
    mesh
}

/// Taubin (lambda/mu) smoothing with uniform weights; shrinks far less than
/// plain Laplacian smoothing.
fn taubin_smooth(mesh: &mut Mesh, iterations: u32) {
    if iterations == 0 || mesh.is_empty() {
        return;
    }
    let n = mesh.positions.len();
    let mut edges: Vec<(u32, u32)> = Vec::with_capacity(mesh.triangles.len() * 6);
    for t in &mesh.triangles {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            edges.push((a, b));
            edges.push((b, a));
        }
    }
    edges.par_sort_unstable();
    edges.dedup();
    let mut start = vec![0u32; n + 1];
    for &(a, _) in &edges {
        start[a as usize + 1] += 1;
    }
    for i in 0..n {
        start[i + 1] += start[i];
    }
    let nb: Vec<u32> = edges.iter().map(|e| e.1).collect();
    let step = |pos: &[Vec3], f: f32| -> Vec<Vec3> {
        (0..n)
            .into_par_iter()
            .map(|i| {
                let (s, e) = (start[i] as usize, start[i + 1] as usize);
                if e == s {
                    return pos[i];
                }
                let avg = nb[s..e].iter().fold(Vec3::ZERO, |a, &j| a + pos[j as usize]) / (e - s) as f32;
                pos[i] + (avg - pos[i]) * f
            })
            .collect()
    };
    for _ in 0..iterations {
        let p = step(&mesh.positions, 0.5);
        mesh.positions = step(&p, -0.53);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::{cuboid, sphere};
    use crate::repair::analyze;
    use crate::slicer::{RasterOptions, Slicer};

    fn check_closed(m: &Mesh) {
        let r = analyze(m);
        assert!(r.watertight, "{r:?}");
        assert_eq!(r.non_manifold_edges, 0);
        assert!(m.signed_volume() > 0.0);
    }

    #[test]
    fn hollow_cube() {
        let m = cuboid(Vec3::new(0.0, 0.0, 10.0), Vec3::splat(20.0));
        let r = hollow_with_info(&m, &HollowOptions::default()).unwrap();
        check_closed(&r.cavity);
        let b = r.cavity.bounds();
        // inner offset of a cube is a cube 4 mm smaller
        for (lo, hi, want_lo, want_hi) in [
            (b.min.x, b.max.x, -8.0, 8.0),
            (b.min.y, b.max.y, -8.0, 8.0),
            (b.min.z, b.max.z, 2.0, 18.0),
        ] {
            assert!((lo - want_lo).abs() < 0.25 && (hi - want_hi).abs() < 0.25, "{b:?}");
        }
        let v = r.cavity_volume;
        assert!((v - 4096.0).abs() < 0.05 * 4096.0, "volume {v}");
        assert!(r.voxel_size <= 2.0 / 3.0);
    }

    #[test]
    fn hollow_sphere_wall_is_uniform() {
        let c = Vec3::new(3.0, -2.0, 12.0);
        let m = sphere(c, 10.0, 64, 32);
        let cav = hollow(
            &m,
            &HollowOptions {
                wall_thickness: 1.5,
                ..Default::default()
            },
        )
        .unwrap();
        check_closed(&cav);
        let rs: Vec<f32> = cav.positions.iter().map(|p| (*p - c).length()).collect();
        let (lo, hi) = rs.iter().fold((f32::MAX, 0f32), |a, &r| (a.0.min(r), a.1.max(r)));
        // the polygonal sphere's faces lie up to 0.05 mm inside the ideal sphere
        assert!(lo > 8.45 - 0.08 && hi < 8.5 + 0.06, "radius range {lo}..{hi}");
        let expect = 4.0 / 3.0 * std::f64::consts::PI * 8.45f64.powi(3);
        assert!((cav.signed_volume() - expect).abs() < 0.05 * expect);
        // true wall thickness at every cavity vertex
        let bvh = Bvh::new(&m);
        for p in &cav.positions {
            let (_, d) = bvh.closest_point(*p, 10.0).unwrap();
            assert!((d - 1.5).abs() < 0.06, "wall {d} at {p:?}");
        }
    }

    /// Realistic size: `cargo test --release -p openresin-core -- --ignored hollow_large`
    #[test]
    #[ignore]
    fn hollow_large_model_timing() {
        let m = sphere(Vec3::new(0.0, 0.0, 50.0), 50.0, 360, 180);
        let t = std::time::Instant::now();
        let r = hollow_with_info(&m, &HollowOptions::default()).unwrap();
        let el = t.elapsed();
        eprintln!(
            "hollow: {} tris in, grid {:?} (h = {:.3}), {} tris out, {:.2?}",
            m.triangle_count(),
            r.grid,
            r.voxel_size,
            r.cavity.triangle_count(),
            el
        );
        check_closed(&r.cavity);
        let expect = 4.0 / 3.0 * std::f64::consts::PI * 48.0f64.powi(3);
        assert!((r.cavity_volume - expect).abs() < 0.02 * expect);
    }

    #[test]
    fn thin_models_and_bad_input() {
        let plate = cuboid(Vec3::ZERO, Vec3::new(30.0, 30.0, 3.0));
        assert!(hollow(&plate, &HollowOptions::default()).unwrap().is_empty());
        assert!(hollow(&Mesh::default(), &HollowOptions::default()).is_err());
        let o = HollowOptions {
            wall_thickness: 0.0,
            ..Default::default()
        };
        assert!(hollow(&plate, &o).is_err());
        // a voxel budget forces a coarser grid but still gives a closed cavity
        let big = cuboid(Vec3::ZERO, Vec3::splat(60.0));
        let r = hollow_with_info(
            &big,
            &HollowOptions {
                max_voxels: 200_000,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(r.grid.iter().product::<usize>() <= 200_000);
        check_closed(&r.cavity);
    }

    #[test]
    fn nested_and_overlapping_shells() {
        // two overlapping cubes form one solid; the cavity follows the union
        let mut m = cuboid(Vec3::new(0.0, 0.0, 10.0), Vec3::splat(20.0));
        m.append(&cuboid(Vec3::new(12.0, 0.0, 10.0), Vec3::splat(20.0)));
        let cav = hollow(
            &m,
            &HollowOptions {
                wall_thickness: 2.0,
                voxel_size: 0.5,
                ..Default::default()
            },
        )
        .unwrap();
        check_closed(&cav);
        let expect = 28.0 * 16.0 * 16.0;
        assert!(
            (cav.signed_volume() - expect).abs() < 0.06 * expect,
            "{}",
            cav.signed_volume()
        );
        assert_eq!(crate::split::split_shells(&cav).len(), 1);
    }

    #[test]
    fn sliced_hollow_cube_with_drain_hole() {
        let m = cuboid(Vec3::new(0.0, 0.0, 10.0), Vec3::splat(20.0));
        let cav = hollow(&m, &HollowOptions::default()).unwrap();
        let hole = drain_hole(Vec3::new(0.0, 0.0, 0.0), Vec3::Z, 3.0, 4.0);
        check_closed(&hole);
        let opts = RasterOptions {
            width: 200,
            height: 200,
            size_x: 100.0,
            size_y: 100.0,
            mirror_x: false,
            mirror_y: false,
            anti_aliasing: 4,
        };
        let s = Slicer::with_negatives(&[&m], &[&cav, &hole], 0.5, opts);
        let px_area = 0.25; // mm² per pixel
        let mid = s.rasterize(20).lit_area_px() * px_area;
        assert!((mid - (400.0 - 256.0)).abs() < 8.0, "mid layer area {mid}");
        // the bottom layer has the drain hole
        let bottom = s.rasterize(0).lit_area_px() * px_area;
        let hole_area = std::f64::consts::PI * 1.5 * 1.5;
        assert!((bottom - (400.0 - hole_area)).abs() < 0.6, "bottom area {bottom}");
        assert!(drain_hole(Vec3::ZERO, Vec3::ZERO, 2.0, 3.0).is_empty());
    }
}
