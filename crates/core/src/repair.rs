//! Mesh analysis and repair.
//!
//! [`analyze`] reports topological problems of a mesh; [`repair`] fixes the
//! common ones found in STL files: unwelded vertices, degenerate (zero area) and
//! duplicate faces, inconsistent or inverted orientation, holes and tiny
//! floating shells. Cavities (closed shells enclosed by another shell) are kept
//! oriented inward so that they stay hollow when sliced.

use std::collections::HashMap;

use glam::Vec3;
use serde::{Deserialize, Serialize};

use crate::mesh::Mesh;
use crate::topo::{
    components_by_vertex, directed, edge_key, half_edges, nesting, orientation_patches, signed_volume_of, submesh,
    UnionFind,
};
use crate::triangulate::{newell_normal, triangulate_loop_3d};

/// Topological statistics of a mesh.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MeshReport {
    pub vertices: usize,
    pub triangles: usize,
    /// Triangles referencing missing vertices or non-finite positions.
    pub invalid_triangles: usize,
    /// Connected components (through shared vertices).
    pub shells: usize,
    /// Edges used by exactly one triangle.
    pub boundary_edges: usize,
    /// Number of holes (connected chains of boundary edges).
    pub holes: usize,
    /// Edges shared by more than two triangles.
    pub non_manifold_edges: usize,
    /// Edges shared by two triangles that traverse it in the same direction.
    pub inconsistent_edges: usize,
    /// Zero-area triangles (repeated vertices or collinear corners).
    pub degenerate_triangles: usize,
    /// Triangles using the same three vertices as an earlier triangle.
    pub duplicate_triangles: usize,
    /// Closed, consistently oriented surfaces that face the wrong way: outer
    /// surfaces that are inside out (and the surfaces nested in them).
    pub inverted_shells: usize,
    /// Closed surfaces enclosed by another surface and facing the opposite way,
    /// i.e. internal voids that stay hollow when sliced.
    pub cavities: usize,
    /// Signed volume in mm³ (positive for outward orientation).
    pub volume: f64,
    /// Surface area in mm².
    pub area: f64,
    /// No boundary, non-manifold or inconsistent edges.
    pub watertight: bool,
}

impl MeshReport {
    /// True when the mesh can be sliced reliably without repair.
    pub fn is_clean(&self) -> bool {
        self.watertight
            && self.invalid_triangles == 0
            && self.degenerate_triangles == 0
            && self.duplicate_triangles == 0
            && self.inverted_shells == 0
    }
}

/// Options for [`repair`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RepairOptions {
    /// Vertices closer than this distance (mm) are merged. `0` merges only
    /// vertices with identical coordinates.
    pub weld_tolerance: f32,
    /// Remove zero-area triangles (T-junctions are split so no hole is left).
    pub remove_degenerate: bool,
    /// Remove duplicated triangles (opposite-facing pairs cancel out).
    pub remove_duplicates: bool,
    /// Make orientation consistent and point every outer closed shell outward.
    /// Shells enclosed by another shell keep their orientation relative to it,
    /// so cavities stay cavities (and enclosed solids stay solid).
    pub fix_orientation: bool,
    /// Close holes by triangulating boundary loops.
    pub fill_holes: bool,
    /// Holes with more boundary edges than this are left open (0 = no limit).
    pub max_hole_edges: usize,
    /// Shells with an absolute volume below this value (mm³) are removed
    /// (0 disables).
    pub min_shell_volume: f64,
}

impl Default for RepairOptions {
    fn default() -> Self {
        Self {
            weld_tolerance: 1e-4,
            remove_degenerate: true,
            remove_duplicates: true,
            fix_orientation: true,
            fill_holes: true,
            max_hole_edges: 0,
            min_shell_volume: 0.0,
        }
    }
}

/// What [`repair`] changed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RepairReport {
    pub before: MeshReport,
    pub after: MeshReport,
    pub invalid_removed: usize,
    pub vertices_welded: usize,
    pub degenerate_removed: usize,
    /// Triangles split to remove T-junction slivers without opening holes.
    pub triangles_split: usize,
    pub duplicates_removed: usize,
    pub triangles_flipped: usize,
    /// Shells flipped as a whole (inside out or wrongly oriented cavities).
    pub shells_reoriented: usize,
    pub holes_filled: usize,
    pub hole_triangles_added: usize,
    /// Holes that could not be closed (open chains or over `max_hole_edges`).
    pub holes_unfilled: usize,
    pub shells_removed: usize,
}

/// Height tolerance below which a triangle counts as degenerate.
fn degenerate_tol(diag: f32) -> f32 {
    (diag * 1e-6).max(1e-7)
}

/// Returns `(longest edge start, end, opposite vertex)` when the triangle has
/// (almost) zero area.
#[inline]
fn degenerate_info(p: &[Vec3], t: &[u32; 3], htol: f32) -> Option<(u32, u32, u32)> {
    let v = [p[t[0] as usize], p[t[1] as usize], p[t[2] as usize]];
    let l = [
        (v[1] - v[0]).length_squared(),
        (v[2] - v[1]).length_squared(),
        (v[0] - v[2]).length_squared(),
    ];
    let k = if l[0] >= l[1] && l[0] >= l[2] {
        0
    } else if l[1] >= l[2] {
        1
    } else {
        2
    };
    let longest = l[k].sqrt();
    let area2 = (v[1] - v[0]).cross(v[2] - v[0]).length();
    if longest <= 0.0 || area2 / longest < htol {
        Some((t[k], t[(k + 1) % 3], t[(k + 2) % 3]))
    } else {
        None
    }
}

fn finite_diag(p: &[Vec3]) -> f32 {
    let mut b = crate::geometry::Aabb::EMPTY;
    for v in p.iter().filter(|v| v.is_finite()) {
        b.extend(*v);
    }
    b.size().length()
}

/// Analyzes the topology of `mesh`. Never panics, also not on invalid indices.
pub fn analyze(mesh: &Mesh) -> MeshReport {
    let p = &mesh.positions;
    let nv = p.len();
    let finite: Vec<bool> = p.iter().map(|v| v.is_finite()).collect();
    let htol = degenerate_tol(finite_diag(p));
    let mut r = MeshReport {
        vertices: nv,
        triangles: mesh.triangles.len(),
        ..Default::default()
    };
    let mut good: Vec<[u32; 3]> = Vec::with_capacity(mesh.triangles.len());
    for t in &mesh.triangles {
        if t.iter().any(|&i| i as usize >= nv || !finite[i as usize]) {
            r.invalid_triangles += 1;
            continue;
        }
        if t[0] == t[1] || t[1] == t[2] || t[0] == t[2] {
            r.degenerate_triangles += 1;
            continue;
        }
        if degenerate_info(p, t, htol).is_some() {
            r.degenerate_triangles += 1;
        }
        good.push(*t);
    }
    let mut seen: HashMap<[u32; 3], u32> = HashMap::with_capacity(good.len());
    for t in &good {
        let mut s = *t;
        s.sort_unstable();
        let c = seen.entry(s).or_insert(0);
        if *c > 0 {
            r.duplicate_triangles += 1;
        }
        *c += 1;
    }
    drop(seen);

    let he = half_edges(&good);
    let mut boundary_uf: Option<UnionFind> = None;
    let mut boundary_vertices: Vec<u32> = Vec::new();
    for g in he.chunk_by(|a, b| a.0 == b.0) {
        match g.len() {
            1 => {
                r.boundary_edges += 1;
                let (a, b) = directed(&good, g[0].1, false);
                let uf = boundary_uf.get_or_insert_with(|| UnionFind::new(nv));
                uf.union(a, b);
                boundary_vertices.push(a);
            }
            2 => {
                let (a, _) = directed(&good, g[0].1, false);
                let (c, _) = directed(&good, g[1].1, false);
                if a == c {
                    r.inconsistent_edges += 1;
                }
            }
            _ => r.non_manifold_edges += 1,
        }
    }
    if let Some(mut uf) = boundary_uf {
        let mut roots: Vec<u32> = boundary_vertices.iter().map(|&v| uf.find(v)).collect();
        roots.sort_unstable();
        roots.dedup();
        r.holes = roots.len();
    }
    let gm = Mesh::new(p.clone(), good);
    r.shells = components_by_vertex(&gm).1;
    r.volume = gm.signed_volume();
    r.area = gm.surface_area();
    r.watertight =
        !gm.triangles.is_empty() && r.boundary_edges == 0 && r.non_manifold_edges == 0 && r.inconsistent_edges == 0;

    // inverted closed surfaces (taking cavities into account)
    let patches = orientation_patches(&gm.triangles, &he);
    let groups = group_by_patch(&gm.triangles, &patches.patch_of, patches.count);
    let usable: Vec<bool> = (0..patches.count)
        .map(|i| patches.closed[i] && patches.consistent[i])
        .collect();
    let nest = nesting(p, &groups, &usable);
    let vols: Vec<f64> = groups.iter().map(|g| signed_volume_of(p, g)).collect();
    let flips = patch_flips(&vols, &usable, &nest);
    for i in 0..patches.count {
        if usable[i] {
            r.inverted_shells += flips[i] as usize;
            if let Some(par) = nest[i].parent {
                if (vols[i] > 0.0) != (vols[par] > 0.0) {
                    r.cavities += 1;
                }
            }
        }
    }
    r
}

/// Decides which closed surfaces to flip: outer surfaces are made outward, a
/// nested surface is flipped together with its container (keeping the relative
/// orientation, so cavities stay cavities). Open surfaces are judged by their
/// own volume.
fn patch_flips(vols: &[f64], closed: &[bool], nest: &[crate::topo::Nest]) -> Vec<bool> {
    let mut order: Vec<usize> = (0..vols.len()).collect();
    order.sort_by_key(|&i| nest[i].depth);
    let mut flip = vec![false; vols.len()];
    for i in order {
        flip[i] = match (closed[i], nest[i].parent) {
            (true, Some(par)) => flip[par],
            _ => vols[i] < 0.0,
        };
    }
    flip
}

fn group_by_patch(tris: &[[u32; 3]], patch_of: &[u32], count: usize) -> Vec<Vec<[u32; 3]>> {
    let mut groups = vec![Vec::new(); count];
    for (t, &p) in tris.iter().zip(patch_of) {
        groups[p as usize].push(*t);
    }
    groups
}

/// Repairs `mesh` according to `opts`. Returns the repaired mesh (compacted,
/// unreferenced vertices removed) and a report of what was changed.
pub fn repair(mesh: &Mesh, opts: &RepairOptions) -> (Mesh, RepairReport) {
    let mut rep = RepairReport {
        before: analyze(mesh),
        ..Default::default()
    };
    let nv = mesh.positions.len();
    let finite: Vec<bool> = mesh.positions.iter().map(|v| v.is_finite()).collect();
    let mut tris: Vec<[u32; 3]> = Vec::with_capacity(mesh.triangles.len());
    for t in &mesh.triangles {
        if t.iter().any(|&i| i as usize >= nv || !finite[i as usize]) {
            rep.invalid_removed += 1;
        } else {
            tris.push(*t);
        }
    }

    // 1) weld
    let (mut positions, remap, merged) = weld(&mesh.positions, &finite, opts.weld_tolerance.max(0.0));
    rep.vertices_welded = merged;
    for t in &mut tris {
        for i in t.iter_mut() {
            *i = remap[*i as usize];
        }
    }
    let before = tris.len();
    tris.retain(|t| t[0] != t[1] && t[1] != t[2] && t[0] != t[2]);
    rep.degenerate_removed += before - tris.len();

    // 2) zero-area triangles
    if opts.remove_degenerate {
        let htol = degenerate_tol(finite_diag(&positions));
        let (removed, split) = fix_degenerate(&positions, &mut tris, htol);
        rep.degenerate_removed += removed;
        rep.triangles_split += split;
    }

    // 3) duplicates
    if opts.remove_duplicates {
        rep.duplicates_removed = remove_duplicates(&mut tris);
    }

    // 4) consistent orientation per patch
    let mut flipped = vec![false; tris.len()];
    if opts.fix_orientation {
        let he = half_edges(&tris);
        let patches = orientation_patches(&tris, &he);
        for (i, t) in tris.iter_mut().enumerate() {
            if patches.flip[i] {
                t.swap(1, 2);
                flipped[i] = true;
            }
        }
    }

    // 5) holes
    if opts.fill_holes {
        let (filled, added, unfilled) = fill_holes(&mut positions, &mut tris, opts.max_hole_edges);
        rep.holes_filled = filled;
        rep.hole_triangles_added = added;
        rep.holes_unfilled = unfilled;
        flipped.resize(tris.len(), false);
    }

    // 6) outward orientation (cavities inward)
    if opts.fix_orientation {
        let he = half_edges(&tris);
        let patches = orientation_patches(&tris, &he);
        for (i, t) in tris.iter_mut().enumerate() {
            if patches.flip[i] {
                t.swap(1, 2);
                flipped[i] = !flipped[i];
            }
        }
        let groups = group_by_patch(&tris, &patches.patch_of, patches.count);
        let nest = nesting(&positions, &groups, &patches.closed);
        let vols: Vec<f64> = (0..patches.count)
            .map(|p| {
                if patches.closed[p] {
                    return signed_volume_of(&positions, &groups[p]);
                }
                // open surface: measure relative to its own centroid
                let mut c = glam::DVec3::ZERO;
                let mut n = 0usize;
                for t in &groups[p] {
                    for &i in t {
                        c += positions[i as usize].as_dvec3();
                        n += 1;
                    }
                }
                let c = c / n.max(1) as f64;
                groups[p]
                    .iter()
                    .map(|t| {
                        let a = positions[t[0] as usize].as_dvec3() - c;
                        let b = positions[t[1] as usize].as_dvec3() - c;
                        let d = positions[t[2] as usize].as_dvec3() - c;
                        a.dot(b.cross(d))
                    })
                    .sum::<f64>()
            })
            .collect();
        let flip_patch = patch_flips(&vols, &patches.closed, &nest);
        rep.shells_reoriented = flip_patch.iter().filter(|&&f| f).count();
        for (i, t) in tris.iter_mut().enumerate() {
            if flip_patch[patches.patch_of[i] as usize] {
                t.swap(1, 2);
                flipped[i] = !flipped[i];
            }
        }
        // triangles added by hole filling are new, not flipped
        let original = tris.len() - rep.hole_triangles_added;
        rep.triangles_flipped = flipped[..original].iter().filter(|&&f| f).count();
    }

    // 7) tiny shells
    if opts.min_shell_volume > 0.0 {
        let tmp = Mesh::new(positions.clone(), tris.clone());
        let (comp, count) = components_by_vertex(&tmp);
        let mut vol = vec![0f64; count];
        for (t, &c) in tris.iter().zip(&comp) {
            let a = positions[t[0] as usize].as_dvec3();
            let b = positions[t[1] as usize].as_dvec3();
            let d = positions[t[2] as usize].as_dvec3();
            vol[c as usize] += a.dot(b.cross(d)) / 6.0;
        }
        let keep: Vec<bool> = vol.iter().map(|v| v.abs() >= opts.min_shell_volume).collect();
        rep.shells_removed = keep.iter().filter(|k| !**k).count();
        let mut i = 0;
        tris.retain(|_| {
            let k = keep[comp[i] as usize];
            i += 1;
            k
        });
    }

    let out = submesh(&positions, tris);
    rep.after = analyze(&out);
    (out, rep)
}

/// Merges vertices within `tol`. Returns new positions, the old->new index map
/// and the number of merged vertices. Non-finite vertices map to themselves but
/// are never referenced.
fn weld(pos: &[Vec3], finite: &[bool], tol: f32) -> (Vec<Vec3>, Vec<u32>, usize) {
    let mut out: Vec<Vec3> = Vec::with_capacity(pos.len());
    let mut remap = vec![0u32; pos.len()];
    let mut merged = 0;
    if tol <= 0.0 {
        let mut map: HashMap<[u32; 3], u32> = HashMap::with_capacity(pos.len());
        for (i, p) in pos.iter().enumerate() {
            if !finite[i] {
                remap[i] = out.len() as u32;
                out.push(Vec3::ZERO);
                continue;
            }
            let key = [(p.x + 0.0).to_bits(), (p.y + 0.0).to_bits(), (p.z + 0.0).to_bits()];
            match map.get(&key) {
                Some(&j) => {
                    remap[i] = j;
                    merged += 1;
                }
                None => {
                    let j = out.len() as u32;
                    map.insert(key, j);
                    out.push(*p);
                    remap[i] = j;
                }
            }
        }
        return (out, remap, merged);
    }
    let inv = 1.0 / tol as f64;
    let cell = |p: Vec3| -> (i64, i64, i64) {
        (
            (p.x as f64 * inv).floor() as i64,
            (p.y as f64 * inv).floor() as i64,
            (p.z as f64 * inv).floor() as i64,
        )
    };
    let tol2 = tol * tol;
    let mut grid: HashMap<(i64, i64, i64), Vec<u32>> = HashMap::with_capacity(pos.len());
    for (i, p) in pos.iter().enumerate() {
        if !finite[i] {
            remap[i] = out.len() as u32;
            out.push(Vec3::ZERO);
            continue;
        }
        let (cx, cy, cz) = cell(*p);
        let mut found = None;
        'search: for dz in -1..=1 {
            for dy in -1..=1 {
                for dx in -1..=1 {
                    if let Some(list) = grid.get(&(cx + dx, cy + dy, cz + dz)) {
                        for &j in list {
                            if (out[j as usize] - *p).length_squared() <= tol2 {
                                found = Some(j);
                                break 'search;
                            }
                        }
                    }
                }
            }
        }
        match found {
            Some(j) => {
                remap[i] = j;
                merged += 1;
            }
            None => {
                let j = out.len() as u32;
                out.push(*p);
                grid.entry((cx, cy, cz)).or_default().push(j);
                remap[i] = j;
            }
        }
    }
    (out, remap, merged)
}

/// Removes zero-area triangles. A collinear triangle `(a, c, b)` with `c` on the
/// long edge `ab` is removed after splitting the triangles on the other side of
/// `ab` at `c`, which keeps the surface closed. Returns `(removed, split)`.
fn fix_degenerate(p: &[Vec3], tris: &mut Vec<[u32; 3]>, htol: f32) -> (usize, usize) {
    let mut removed = 0;
    let mut split = 0;
    for _round in 0..8 {
        let degen: Vec<(usize, (u32, u32, u32))> = tris
            .iter()
            .enumerate()
            .filter_map(|(i, t)| degenerate_info(p, t, htol).map(|d| (i, d)))
            .collect();
        if degen.is_empty() {
            return (removed, split);
        }
        let mut remove = vec![false; tris.len()];
        let mut req: HashMap<u64, (u32, usize)> = HashMap::with_capacity(degen.len());
        for &(i, (a, b, c)) in &degen {
            let key = edge_key(a, b);
            if let std::collections::hash_map::Entry::Vacant(e) = req.entry(key) {
                e.insert((c, i));
                remove[i] = true;
            }
        }
        let mut out = Vec::with_capacity(tris.len() + degen.len());
        for (i, t) in tris.iter().enumerate() {
            if remove[i] {
                removed += 1;
                continue;
            }
            let mut done = false;
            for k in 0..3 {
                let (pp, q, r) = (t[k], t[(k + 1) % 3], t[(k + 2) % 3]);
                if let Some(&(c, d)) = req.get(&edge_key(pp, q)) {
                    if d != i && c != r {
                        out.push([pp, c, r]);
                        out.push([c, q, r]);
                        split += 1;
                        done = true;
                        break;
                    }
                }
            }
            if !done {
                out.push(*t);
            }
        }
        *tris = out;
    }
    // give up on whatever is left (hole filling closes the gaps)
    let before = tris.len();
    tris.retain(|t| degenerate_info(p, t, htol).is_none());
    removed += before - tris.len();
    (removed, split)
}

/// Removes duplicated triangles. Copies with the same orientation collapse into
/// one; opposite orientations cancel each other (internal faces of touching
/// shells). Returns the number of removed triangles.
fn remove_duplicates(tris: &mut Vec<[u32; 3]>) -> usize {
    // (net orientation count, total count, index of first even, index of first odd)
    let mut groups: HashMap<[u32; 3], (i32, u32, usize, usize)> = HashMap::with_capacity(tris.len());
    let parity = |t: &[u32; 3]| -> ([u32; 3], bool) {
        let mut s = *t;
        s.sort_unstable();
        let even = *t == s || *t == [s[1], s[2], s[0]] || *t == [s[2], s[0], s[1]];
        (s, even)
    };
    for (i, t) in tris.iter().enumerate() {
        let (s, even) = parity(t);
        let e = groups.entry(s).or_insert((0, 0, usize::MAX, usize::MAX));
        e.0 += if even { 1 } else { -1 };
        e.1 += 1;
        if even && e.2 == usize::MAX {
            e.2 = i;
        }
        if !even && e.3 == usize::MAX {
            e.3 = i;
        }
    }
    if groups.len() == tris.len() {
        return 0;
    }
    let before = tris.len();
    let mut out = Vec::with_capacity(groups.len());
    for (i, t) in tris.iter().enumerate() {
        let (s, _) = parity(t);
        let g = groups[&s];
        if g.1 == 1 {
            out.push(*t);
            continue;
        }
        let keep = if g.0 > 0 {
            i == g.2
        } else if g.0 < 0 {
            i == g.3
        } else {
            false
        };
        if keep {
            out.push(*t);
        }
    }
    *tris = out;
    before - tris.len()
}

/// Fills boundary loops of a consistently oriented mesh. Returns
/// `(holes filled, triangles added, holes left open)`.
fn fill_holes(positions: &mut Vec<Vec3>, tris: &mut Vec<[u32; 3]>, max_edges: usize) -> (usize, usize, usize) {
    let he = half_edges(tris);
    let mut bnd: Vec<(u32, u32)> = Vec::new();
    for g in he.chunk_by(|a, b| a.0 == b.0) {
        if g.len() == 2 {
            let (a, _) = directed(tris, g[0].1, false);
            let (c, _) = directed(tris, g[1].1, false);
            if a != c {
                continue;
            }
        }
        let mut fwd = 0i32;
        let mut lo_hi = (0u32, 0u32);
        for &(_, h) in g {
            let (a, b) = directed(tris, h, false);
            lo_hi = (a.min(b), a.max(b));
            fwd += if a < b { 1 } else { -1 };
        }
        let (lo, hi) = lo_hi;
        for _ in 0..fwd.unsigned_abs() {
            bnd.push(if fwd > 0 { (lo, hi) } else { (hi, lo) });
        }
    }
    if bnd.is_empty() {
        return (0, 0, 0);
    }
    let mut out_edges: HashMap<u32, Vec<u32>> = HashMap::new();
    for (i, &(a, _)) in bnd.iter().enumerate() {
        out_edges.entry(a).or_default().push(i as u32);
    }
    let mut used = vec![false; bnd.len()];
    let mut loops: Vec<Vec<u32>> = Vec::new();
    let mut open = 0;
    for e0 in 0..bnd.len() {
        if used[e0] {
            continue;
        }
        used[e0] = true;
        let (a, b) = bnd[e0];
        let mut path = vec![a];
        let mut at: HashMap<u32, usize> = HashMap::from([(a, 0)]);
        let mut cur = b;
        loop {
            if let Some(&k) = at.get(&cur) {
                let lp = path.split_off(k);
                for v in &lp {
                    at.remove(v);
                }
                if lp.len() >= 3 {
                    loops.push(lp);
                }
                if path.is_empty() {
                    break;
                }
            }
            at.insert(cur, path.len());
            path.push(cur);
            let next = out_edges
                .get(&cur)
                .and_then(|l| l.iter().copied().find(|&e| !used[e as usize]));
            match next {
                Some(e) => {
                    used[e as usize] = true;
                    cur = bnd[e as usize].1;
                }
                None => {
                    open += 1;
                    break;
                }
            }
        }
    }
    let mut filled = 0;
    let mut added = 0;
    for lp in loops {
        let n = lp.len();
        if max_edges > 0 && n > max_edges {
            open += 1;
            continue;
        }
        let before = tris.len();
        if n == 3 {
            tris.push([lp[2], lp[1], lp[0]]);
        } else {
            // the cap runs against the boundary direction
            let rev: Vec<u32> = lp.iter().rev().copied().collect();
            let pts: Vec<Vec3> = rev.iter().map(|&i| positions[i as usize]).collect();
            let t = triangulate_loop_3d(&pts);
            if t.clean && is_planar_enough(&pts) {
                tris.extend(
                    t.triangles
                        .iter()
                        .map(|tr| [rev[tr[0] as usize], rev[tr[1] as usize], rev[tr[2] as usize]]),
                );
            } else {
                // centroid fan: always a valid closed patch
                let c = pts.iter().fold(Vec3::ZERO, |a, p| a + *p) / n as f32;
                let ci = positions.len() as u32;
                positions.push(c);
                for i in 0..n {
                    tris.push([lp[(i + 1) % n], lp[i], ci]);
                }
            }
        }
        filled += 1;
        added += tris.len() - before;
    }
    (filled, added, open)
}

/// The loop deviates from its best-fit plane by less than 20 % of its size.
fn is_planar_enough(pts: &[Vec3]) -> bool {
    let n = newell_normal(pts);
    let len = n.length();
    if len <= 0.0 {
        return false;
    }
    let nn = n / len;
    let c = pts.iter().fold(glam::DVec3::ZERO, |a, p| a + p.as_dvec3()) / pts.len() as f64;
    let dev = pts.iter().map(|p| (p.as_dvec3() - c).dot(nn).abs()).fold(0.0, f64::max);
    dev <= 0.2 * (len * 0.5).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::{cuboid, frustum, sphere};

    fn soup(m: &Mesh) -> Mesh {
        // every triangle gets its own vertices (like an unwelded STL)
        let mut out = Mesh::default();
        for i in 0..m.triangle_count() {
            let base = out.positions.len() as u32;
            out.positions.extend_from_slice(&m.tri(i));
            out.triangles.push([base, base + 1, base + 2]);
        }
        out
    }

    #[test]
    fn clean_mesh_report() {
        let m = sphere(Vec3::new(1.0, 2.0, 3.0), 5.0, 24, 12);
        let r = analyze(&m);
        assert!(r.is_clean(), "{r:?}");
        assert_eq!(r.shells, 1);
        assert!(r.volume > 0.0);
        let (fixed, rep) = repair(&m, &RepairOptions::default());
        assert_eq!(fixed.triangle_count(), m.triangle_count());
        assert_eq!(rep.triangles_flipped, 0);
        assert!(rep.after.is_clean());
    }

    #[test]
    fn welds_soup_with_noise() {
        let m = sphere(Vec3::ZERO, 10.0, 32, 16);
        let mut s = soup(&m);
        for (i, p) in s.positions.iter_mut().enumerate() {
            *p += Vec3::splat(((i * 7919) % 13) as f32 * 3e-6);
        }
        let r = analyze(&s);
        assert!(!r.watertight);
        assert_eq!(r.boundary_edges, m.triangle_count() * 3);
        let (fixed, rep) = repair(&s, &RepairOptions::default());
        assert_eq!(fixed.positions.len(), m.positions.len());
        assert!(rep.vertices_welded > 0);
        assert!(rep.after.is_clean(), "{:?}", rep.after);
        assert!((fixed.signed_volume() - m.signed_volume()).abs() < 1e-2);
    }

    #[test]
    fn flips_inconsistent_and_inverted_shells() {
        let mut m = cuboid(Vec3::ZERO, Vec3::splat(2.0));
        // flip a few faces -> inconsistent
        m.triangles[0].swap(1, 2);
        m.triangles[5].swap(1, 2);
        let r = analyze(&m);
        assert!(r.inconsistent_edges > 0);
        let (fixed, rep) = repair(&m, &RepairOptions::default());
        assert!(rep.after.is_clean(), "{:?}", rep.after);
        assert_eq!(rep.triangles_flipped, 2);
        assert!((fixed.signed_volume() - 8.0).abs() < 1e-4);
        // fully inverted
        let mut inv = cuboid(Vec3::ZERO, Vec3::splat(2.0));
        inv.flip();
        let r = analyze(&inv);
        assert!(r.watertight);
        assert_eq!(r.inverted_shells, 1);
        let (fixed, rep) = repair(&inv, &RepairOptions::default());
        assert_eq!(rep.shells_reoriented, 1);
        assert!(fixed.signed_volume() > 0.0);
    }

    #[test]
    fn keeps_cavities_inward() {
        let mut m = cuboid(Vec3::ZERO, Vec3::splat(10.0));
        let mut inner = sphere(Vec3::ZERO, 3.0, 16, 8);
        inner.flip(); // correct cavity
        m.append(&inner);
        let r = analyze(&m);
        assert_eq!((r.inverted_shells, r.cavities), (0, 1));
        let (fixed, rep) = repair(&m, &RepairOptions::default());
        assert_eq!(rep.shells_reoriented, 0);
        assert!((fixed.signed_volume() - m.signed_volume()).abs() < 1e-6);
        // the whole model inside out: outer surface and cavity are flipped together
        let mut inv = m.clone();
        inv.flip();
        let r = analyze(&inv);
        assert_eq!((r.inverted_shells, r.cavities), (2, 1));
        let (fixed, rep) = repair(&inv, &RepairOptions::default());
        assert_eq!(rep.shells_reoriented, 2);
        assert!((fixed.signed_volume() - m.signed_volume()).abs() < 1e-3);
        assert_eq!(analyze(&fixed).cavities, 1);
        // an enclosed outward shell is an enclosed solid, not a broken cavity
        let mut nested = cuboid(Vec3::ZERO, Vec3::splat(10.0));
        nested.append(&cuboid(Vec3::new(0.2, 0.2, 0.0), Vec3::splat(9.0)));
        let r = analyze(&nested);
        assert_eq!((r.inverted_shells, r.cavities), (0, 0));
        // overlapping cubes are independent outer shells
        let mut overlap = cuboid(Vec3::ZERO, Vec3::splat(10.0));
        let mut b = cuboid(Vec3::new(6.0, 0.0, 0.0), Vec3::splat(9.0));
        b.flip();
        overlap.append(&b);
        let r = analyze(&overlap);
        assert_eq!((r.inverted_shells, r.cavities), (1, 0));
        let (fixed, _) = repair(&overlap, &RepairOptions::default());
        assert!(fixed.signed_volume() > 1000.0);
    }

    #[test]
    fn fills_holes() {
        // remove a vertex fan from a sphere (one hole) and two faces from a cube
        let s = sphere(Vec3::ZERO, 10.0, 32, 16);
        let mut holed = s.clone();
        let top = (holed.positions.len() - 1) as u32;
        holed.triangles.retain(|t| !t.contains(&top));
        let r = analyze(&holed);
        assert_eq!(r.holes, 1);
        assert_eq!(r.boundary_edges, 32);
        let (fixed, rep) = repair(&holed, &RepairOptions::default());
        assert_eq!(rep.holes_filled, 1);
        assert!(rep.after.is_clean(), "{:?}", rep.after);
        assert!(fixed.signed_volume() > 0.0 && fixed.signed_volume() < s.signed_volume());

        let mut c = cuboid(Vec3::ZERO, Vec3::splat(4.0));
        c.triangles.remove(2);
        c.triangles.remove(2); // whole top face
        let (fixed, rep) = repair(&c, &RepairOptions::default());
        assert_eq!(rep.holes_filled, 1);
        assert_eq!(fixed.open_edge_count(), 0);
        assert!((fixed.signed_volume() - 64.0).abs() < 1e-3);

        // a non-planar hole: cut a band out of a cylinder side
        let cyl = frustum(Vec3::ZERO, Vec3::new(0.0, 0.0, 10.0), 5.0, 5.0, 24);
        let mut band = cyl.clone();
        band.triangles.drain(0..10); // several side triangles
        let (fixed, rep) = repair(&band, &RepairOptions::default());
        assert!(rep.holes_filled >= 1);
        assert!(rep.after.watertight, "{:?}", rep.after);
        assert!(fixed.signed_volume() > 0.0);
    }

    #[test]
    fn removes_degenerate_and_duplicates() {
        // cube whose top face has a T-junction sliver
        let mut m = cuboid(Vec3::ZERO, Vec3::splat(2.0));
        let mid = m.positions.len() as u32;
        // midpoint of top edge 4-5 (front top edge)
        m.positions.push((m.positions[4] + m.positions[5]) * 0.5);
        // replace front face (0,1,5),(0,5,4) by (0,1,5),(0,5,mid)... keep it simple:
        // split top triangle [4,5,6] into [4,mid,6],[mid,5,6] and add degenerate [4,5,mid]
        let top = m.triangles.iter().position(|t| *t == [4, 5, 6]).unwrap();
        m.triangles[top] = [4, mid, 6];
        m.triangles.push([mid, 5, 6]);
        // the front face still uses edge 5-4 -> T-junction; add zero-area sliver 5,4,mid?
        // the sliver (4,5,mid) is degenerate; with it the mesh is closed but has slivers
        m.triangles.push([4, 5, mid]);
        let r = analyze(&m);
        assert_eq!(r.degenerate_triangles, 1);
        let (fixed, rep) = repair(&m, &RepairOptions::default());
        assert_eq!(rep.degenerate_removed, 1);
        assert!(rep.after.is_clean(), "{:?}", rep.after);
        assert!((fixed.signed_volume() - 8.0).abs() < 1e-4);

        // duplicates
        let mut d = cuboid(Vec3::ZERO, Vec3::splat(2.0));
        let t0 = d.triangles[0];
        d.triangles.push(t0);
        d.triangles.push([t0[1], t0[2], t0[0]]);
        assert_eq!(analyze(&d).duplicate_triangles, 2);
        let (fixed, rep) = repair(&d, &RepairOptions::default());
        assert_eq!(rep.duplicates_removed, 2);
        assert!(rep.after.is_clean());
        assert_eq!(fixed.triangle_count(), 12);

        // touching cubes sharing an internal face pair are merged
        let a = cuboid(Vec3::ZERO, Vec3::splat(2.0));
        // mirrored copy: the shared face uses the same diagonal, facing the other way
        let b = a.transformed(
            &(glam::Mat4::from_translation(Vec3::new(2.0, 0.0, 0.0))
                * glam::Mat4::from_scale(Vec3::new(-1.0, 1.0, 1.0))),
        );
        let mut ab = a.clone();
        ab.append(&b);
        let (fixed, rep) = repair(&ab, &RepairOptions::default());
        assert_eq!(rep.duplicates_removed, 4);
        assert!(rep.after.is_clean(), "{:?}", rep.after);
        assert!((fixed.signed_volume() - 16.0).abs() < 1e-4);
    }

    #[test]
    fn removes_tiny_shells_and_survives_garbage() {
        let mut m = cuboid(Vec3::ZERO, Vec3::splat(10.0));
        m.append(&cuboid(Vec3::new(20.0, 0.0, 0.0), Vec3::splat(0.2)));
        let opts = RepairOptions {
            min_shell_volume: 0.1,
            ..Default::default()
        };
        let (fixed, rep) = repair(&m, &opts);
        assert_eq!(rep.shells_removed, 1);
        assert_eq!(fixed.triangle_count(), 12);

        let garbage = Mesh::new(
            vec![Vec3::ZERO, Vec3::new(f32::NAN, 0.0, 0.0), Vec3::X, Vec3::Y],
            vec![[0, 1, 2], [0, 2, 9], [0, 0, 2], [0, 2, 3], [0, 2, 3]],
        );
        let r = analyze(&garbage);
        assert_eq!(r.invalid_triangles, 2);
        let (fixed, rep) = repair(&garbage, &RepairOptions::default());
        assert_eq!(rep.invalid_removed, 2);
        assert!(fixed.positions.iter().all(|p| p.is_finite()));
        let _ = repair(&Mesh::default(), &RepairOptions::default());
        let json = serde_json::to_string(&rep).unwrap();
        let back: RepairReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back, rep);
    }
}
