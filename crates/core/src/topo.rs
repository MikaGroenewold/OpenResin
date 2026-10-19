//! Internal mesh topology helpers shared by repair, split, cut and hollow.

use glam::Vec3;

use crate::mesh::Mesh;
use crate::support::VerticalRayIndex;

/// Disjoint set forest with path halving and union by size.
pub(crate) struct UnionFind {
    parent: Vec<u32>,
    size: Vec<u32>,
}

impl UnionFind {
    pub fn new(n: usize) -> Self {
        Self {
            parent: (0..n as u32).collect(),
            size: vec![1; n],
        }
    }

    pub fn find(&mut self, mut x: u32) -> u32 {
        while self.parent[x as usize] != x {
            let p = self.parent[x as usize];
            self.parent[x as usize] = self.parent[p as usize];
            x = self.parent[x as usize];
        }
        x
    }

    pub fn union(&mut self, a: u32, b: u32) {
        let (mut a, mut b) = (self.find(a), self.find(b));
        if a == b {
            return;
        }
        if self.size[a as usize] < self.size[b as usize] {
            std::mem::swap(&mut a, &mut b);
        }
        self.parent[b as usize] = a;
        self.size[a as usize] += self.size[b as usize];
    }
}

#[inline]
pub(crate) fn edge_key(a: u32, b: u32) -> u64 {
    let (lo, hi) = if a < b { (a, b) } else { (b, a) };
    ((lo as u64) << 32) | hi as u64
}

/// Triangles that only reference existing vertices and have three distinct indices.
pub(crate) fn valid_triangle(t: &[u32; 3], nverts: usize) -> bool {
    (t[0] as usize) < nverts
        && (t[1] as usize) < nverts
        && (t[2] as usize) < nverts
        && t[0] != t[1]
        && t[1] != t[2]
        && t[0] != t[2]
}

/// Groups triangles into components connected through shared vertices.
/// Returns the component id per triangle and the number of components.
/// Invalid triangles get `u32::MAX`.
pub(crate) fn components_by_vertex(mesh: &Mesh) -> (Vec<u32>, usize) {
    let nv = mesh.positions.len();
    let mut uf = UnionFind::new(nv);
    for t in &mesh.triangles {
        if valid_triangle(t, nv) {
            uf.union(t[0], t[1]);
            uf.union(t[0], t[2]);
        }
    }
    let mut root_id = vec![u32::MAX; nv];
    let mut count = 0usize;
    let comp = mesh
        .triangles
        .iter()
        .map(|t| {
            if !valid_triangle(t, nv) {
                return u32::MAX;
            }
            let r = uf.find(t[0]) as usize;
            if root_id[r] == u32::MAX {
                root_id[r] = count as u32;
                count += 1;
            }
            root_id[r]
        })
        .collect();
    (comp, count)
}

/// Builds a compact mesh from a subset of triangles.
pub(crate) fn submesh(positions: &[Vec3], triangles: impl IntoIterator<Item = [u32; 3]>) -> Mesh {
    let mut remap = vec![u32::MAX; positions.len()];
    let mut out = Mesh::default();
    for t in triangles {
        let mut nt = [0u32; 3];
        for k in 0..3 {
            let v = t[k] as usize;
            if remap[v] == u32::MAX {
                remap[v] = out.positions.len() as u32;
                out.positions.push(positions[v]);
            }
            nt[k] = remap[v];
        }
        out.triangles.push(nt);
    }
    out
}

/// Robust "is `p` inside the closed surface indexed by `idx`" test: majority vote
/// of the vertical ray parity at three slightly jittered positions.
pub(crate) fn contains_point(idx: &VerticalRayIndex, p: Vec3, jitter: f32) -> bool {
    let offsets = [(0.0f32, 0.0f32), (0.618, 0.382), (-0.427, 0.719)];
    let votes = offsets
        .iter()
        .filter(|(dx, dy)| idx.contains(Vec3::new(p.x + dx * jitter, p.y + dy * jitter, p.z)))
        .count();
    votes >= 2
}

/// Half-edges `(undirected edge key, triangle * 3 + local edge)` sorted by key.
pub(crate) fn half_edges(tris: &[[u32; 3]]) -> Vec<(u64, u32)> {
    let mut he: Vec<(u64, u32)> = Vec::with_capacity(tris.len() * 3);
    for (i, t) in tris.iter().enumerate() {
        for k in 0..3 {
            he.push((edge_key(t[k], t[(k + 1) % 3]), (i * 3 + k) as u32));
        }
    }
    he.sort_unstable();
    he
}

/// Directed edge of half-edge `h` (triangle `h / 3`, local edge `h % 3`).
#[inline]
pub(crate) fn directed(tris: &[[u32; 3]], h: u32, flip: bool) -> (u32, u32) {
    let t = &tris[(h / 3) as usize];
    let k = (h % 3) as usize;
    let (a, b) = (t[k], t[(k + 1) % 3]);
    if flip {
        (b, a)
    } else {
        (a, b)
    }
}

/// Orientation patches: components connected through manifold edges, with the
/// per-triangle flips that make every patch consistently oriented.
pub(crate) struct Patches {
    pub patch_of: Vec<u32>,
    pub count: usize,
    pub flip: Vec<bool>,
    /// The patch (after flipping) is a closed, consistently oriented surface.
    pub closed: Vec<bool>,
    /// The patch was already consistently oriented in the input.
    pub consistent: Vec<bool>,
}

pub(crate) fn orientation_patches(tris: &[[u32; 3]], he: &[(u64, u32)]) -> Patches {
    let nt = tris.len();
    let mut partner = vec![u32::MAX; nt * 3];
    for g in he.chunk_by(|a, b| a.0 == b.0) {
        if g.len() == 2 && g[0].1 / 3 != g[1].1 / 3 {
            partner[g[0].1 as usize] = g[1].1;
            partner[g[1].1 as usize] = g[0].1;
        }
    }
    let mut patch_of = vec![u32::MAX; nt];
    let mut flip = vec![false; nt];
    let mut conflict: Vec<bool> = Vec::new();
    let mut flipped_any: Vec<bool> = Vec::new();
    let mut stack = Vec::new();
    let mut count = 0usize;
    for seed in 0..nt {
        if patch_of[seed] != u32::MAX {
            continue;
        }
        let p = count as u32;
        count += 1;
        conflict.push(false);
        flipped_any.push(false);
        patch_of[seed] = p;
        stack.push(seed as u32);
        while let Some(t) = stack.pop() {
            for k in 0..3 {
                let h = t * 3 + k;
                let q = partner[h as usize];
                if q == u32::MAX {
                    continue;
                }
                let (a, b) = directed(tris, h, flip[t as usize]);
                let dt = a < b;
                let n = (q / 3) as usize;
                if patch_of[n] == u32::MAX {
                    patch_of[n] = p;
                    let (c, d) = directed(tris, q, false);
                    // neighbours must traverse the shared edge in opposite directions
                    flip[n] = (c < d) == dt;
                    if flip[n] {
                        flipped_any[p as usize] = true;
                    }
                    stack.push(n as u32);
                } else {
                    let (c, d) = directed(tris, q, flip[n]);
                    if (c < d) == dt {
                        conflict[p as usize] = true;
                    }
                }
            }
        }
    }
    let mut closed: Vec<bool> = conflict.iter().map(|c| !c).collect();
    let mut bal: Vec<(u32, i32)> = Vec::new();
    for g in he.chunk_by(|a, b| a.0 == b.0) {
        match g.len() {
            2 if g[0].1 / 3 != g[1].1 / 3 => {}
            1 => closed[patch_of[(g[0].1 / 3) as usize] as usize] = false,
            _ => {
                bal.clear();
                for &(_, h) in g {
                    let t = (h / 3) as usize;
                    let (a, b) = directed(tris, h, flip[t]);
                    let s = if a < b { 1 } else { -1 };
                    let p = patch_of[t];
                    match bal.iter_mut().find(|e| e.0 == p) {
                        Some(e) => e.1 += s,
                        None => bal.push((p, s)),
                    }
                }
                for &(p, s) in &bal {
                    if s != 0 {
                        closed[p as usize] = false;
                    }
                }
            }
        }
    }
    let consistent = (0..count).map(|p| !flipped_any[p] && !conflict[p]).collect();
    Patches {
        patch_of,
        count,
        flip,
        closed,
        consistent,
    }
}

pub(crate) fn signed_volume_of(positions: &[Vec3], tris: &[[u32; 3]]) -> f64 {
    tris.iter()
        .map(|t| {
            let a = positions[t[0] as usize].as_dvec3();
            let b = positions[t[1] as usize].as_dvec3();
            let c = positions[t[2] as usize].as_dvec3();
            a.dot(b.cross(c))
        })
        .sum::<f64>()
        / 6.0
}

/// Nesting information of a closed surface: how many other closed surfaces
/// fully enclose it and which of them is the innermost one.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Nest {
    pub depth: u32,
    pub parent: Option<usize>,
}

/// Computes the nesting of closed triangle groups (surfaces). A group counts as
/// enclosed by another when its bounding box lies inside the other's box and at
/// least 90 % of sample points on it are inside the other surface, so shells that
/// merely intersect each other are not mistaken for cavities.
pub(crate) fn nesting(positions: &[Vec3], groups: &[Vec<[u32; 3]>], closed: &[bool]) -> Vec<Nest> {
    use rayon::prelude::*;
    let n = groups.len();
    let mut out = vec![Nest::default(); n];
    let ids: Vec<usize> = (0..n).filter(|&i| closed[i] && !groups[i].is_empty()).collect();
    if ids.len() < 2 {
        return out;
    }
    let info: Vec<(crate::geometry::Aabb, f64)> = (0..n)
        .map(|i| {
            let mut b = crate::geometry::Aabb::EMPTY;
            for t in &groups[i] {
                for &v in t {
                    b.extend(positions[v as usize]);
                }
            }
            (b, signed_volume_of(positions, &groups[i]).abs())
        })
        .collect();
    let mut cand: Vec<(usize, usize)> = Vec::new();
    for &a in &ids {
        let ba = info[a].0;
        for &b in &ids {
            let bb = info[b].0;
            if a != b
                && info[b].1 > info[a].1
                && ba.min.x >= bb.min.x
                && ba.min.y >= bb.min.y
                && ba.min.z >= bb.min.z
                && ba.max.x <= bb.max.x
                && ba.max.y <= bb.max.y
                && ba.max.z <= bb.max.z
            {
                cand.push((a, b));
            }
        }
    }
    if cand.is_empty() {
        return out;
    }
    let mut containers: Vec<usize> = cand.iter().map(|c| c.1).collect();
    containers.sort_unstable();
    containers.dedup();
    let meshes: Vec<Mesh> = containers
        .iter()
        .map(|&b| submesh(positions, groups[b].iter().copied()))
        .collect();
    let indices: Vec<VerticalRayIndex> = meshes.iter().map(VerticalRayIndex::new).collect();
    let samples = |a: usize| -> Vec<Vec3> {
        let g = &groups[a];
        let step = (g.len() / 23).max(1);
        let mut s: Vec<Vec3> = g
            .iter()
            .step_by(step)
            .map(|t| (positions[t[0] as usize] + positions[t[1] as usize] + positions[t[2] as usize]) / 3.0)
            .collect();
        s.truncate(24);
        s
    };
    let hits: Vec<(usize, usize)> = cand
        .par_iter()
        .filter(|&&(a, b)| {
            let ci = containers.binary_search(&b).unwrap();
            let idx = &indices[ci];
            let jitter = (info[b].0.size().length() * 1e-5).max(1e-6);
            let s = samples(a);
            let inside = s.iter().filter(|p| contains_point(idx, **p, jitter)).count();
            inside * 10 >= s.len() * 9
        })
        .copied()
        .collect();
    for (a, b) in hits {
        out[a].depth += 1;
        let better = match out[a].parent {
            None => true,
            Some(p) => info[b].1 < info[p].1,
        };
        if better {
            out[a].parent = Some(b);
        }
    }
    out
}
