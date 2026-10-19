//! Bounding volume hierarchy over mesh triangles for closest-point and segment
//! queries (internal).

use glam::Vec3;

use crate::mesh::Mesh;

const LEAF: usize = 4;

#[derive(Clone, Copy, Default)]
struct Node {
    min: Vec3,
    max: Vec3,
    /// first child (internal) or first triangle slot (leaf)
    start: u32,
    /// triangle count for leaves, 0 for internal nodes
    count: u32,
}

pub(crate) struct Bvh {
    nodes: Vec<Node>,
    order: Vec<u32>,
    tris: Vec<[Vec3; 3]>,
}

#[inline]
fn box_dist2(min: Vec3, max: Vec3, p: Vec3) -> f32 {
    let d = (min - p).max(p - max).max(Vec3::ZERO);
    d.length_squared()
}

impl Bvh {
    pub fn new(mesh: &Mesh) -> Self {
        let tris: Vec<[Vec3; 3]> = (0..mesh.triangle_count()).map(|i| mesh.tri(i)).collect();
        let mut b = Bvh {
            nodes: Vec::with_capacity(tris.len() / 2 + 1),
            order: (0..tris.len() as u32).collect(),
            tris,
        };
        if !b.tris.is_empty() {
            let centroids: Vec<Vec3> = b.tris.iter().map(|t| (t[0] + t[1] + t[2]) / 3.0).collect();
            b.nodes.push(Node::default());
            let n = b.tris.len();
            b.build(0, 0, n, &centroids);
        }
        b
    }

    fn build(&mut self, node: usize, s: usize, e: usize, c: &[Vec3]) {
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        let mut cmin = min;
        let mut cmax = max;
        for &t in &self.order[s..e] {
            for v in &self.tris[t as usize] {
                min = min.min(*v);
                max = max.max(*v);
            }
            cmin = cmin.min(c[t as usize]);
            cmax = cmax.max(c[t as usize]);
        }
        self.nodes[node].min = min;
        self.nodes[node].max = max;
        if e - s <= LEAF {
            self.nodes[node].start = s as u32;
            self.nodes[node].count = (e - s) as u32;
            return;
        }
        let ext = cmax - cmin;
        let axis = if ext.x >= ext.y && ext.x >= ext.z {
            0
        } else if ext.y >= ext.z {
            1
        } else {
            2
        };
        let mid = (s + e) / 2;
        self.order[s..e].select_nth_unstable_by(mid - s, |&a, &b| c[a as usize][axis].total_cmp(&c[b as usize][axis]));
        let left = self.nodes.len();
        self.nodes.push(Node::default());
        self.nodes.push(Node::default());
        self.nodes[node].start = left as u32;
        self.nodes[node].count = 0;
        self.build(left, s, mid, c);
        self.build(left + 1, mid, e, c);
    }

    /// Closest point on the mesh to `p` within `max_dist`. Returns the point and
    /// its distance.
    pub fn closest_point(&self, p: Vec3, max_dist: f32) -> Option<(Vec3, f32)> {
        if self.nodes.is_empty() {
            return None;
        }
        let mut best = max_dist * max_dist;
        let mut best_pt = None;
        let mut stack: Vec<u32> = Vec::with_capacity(64);
        stack.push(0);
        while let Some(n) = stack.pop() {
            let node = self.nodes[n as usize];
            if box_dist2(node.min, node.max, p) > best {
                continue;
            }
            if node.count > 0 {
                for &t in &self.order[node.start as usize..(node.start + node.count) as usize] {
                    let [a, b, c] = self.tris[t as usize];
                    let q = closest_point_triangle(p, a, b, c);
                    let d = (q - p).length_squared();
                    if d <= best {
                        best = d;
                        best_pt = Some(q);
                    }
                }
            } else {
                let (l, r) = (node.start, node.start + 1);
                let dl = box_dist2(self.nodes[l as usize].min, self.nodes[l as usize].max, p);
                let dr = box_dist2(self.nodes[r as usize].min, self.nodes[r as usize].max, p);
                if dl < dr {
                    stack.push(r);
                    stack.push(l);
                } else {
                    stack.push(l);
                    stack.push(r);
                }
            }
        }
        best_pt.map(|q| (q, best.sqrt()))
    }

    /// Does the segment `a -> b` come closer than `radius` to the mesh surface?
    /// (Conservative sampling test: checks points every `radius` along it.)
    pub fn segment_near(&self, a: Vec3, b: Vec3, radius: f32) -> bool {
        let len = (b - a).length();
        let steps = ((len / radius.max(1e-3)).ceil() as usize).max(1);
        (0..=steps).any(|i| {
            let p = a + (b - a) * (i as f32 / steps as f32);
            self.closest_point(p, radius).is_some()
        })
    }
}

/// Closest point on triangle `abc` to `p` (Ericson, Real-Time Collision Detection).
pub(crate) fn closest_point_triangle(p: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Vec3 {
    let ab = b - a;
    let ac = c - a;
    let ap = p - a;
    let d1 = ab.dot(ap);
    let d2 = ac.dot(ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = p - b;
    let d3 = ab.dot(bp);
    let d4 = ac.dot(bp);
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return a + ab * (d1 / (d1 - d3));
    }
    let cp = p - c;
    let d5 = ab.dot(cp);
    let d6 = ac.dot(cp);
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return a + ac * (d2 / (d2 - d6));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        return b + (c - b) * ((d4 - d3) / ((d4 - d3) + (d5 - d6)));
    }
    let denom = 1.0 / (va + vb + vc);
    let q = a + ab * (vb * denom) + ac * (vc * denom);
    if q.is_finite() {
        q
    } else {
        // degenerate triangle: nearest vertex
        [a, b, c]
            .into_iter()
            .min_by(|x, y| (*x - p).length_squared().total_cmp(&(*y - p).length_squared()))
            .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::sphere;

    #[test]
    fn closest_point_matches_brute_force() {
        let m = sphere(Vec3::new(1.0, 2.0, 3.0), 5.0, 32, 16);
        let bvh = Bvh::new(&m);
        for i in 0..200 {
            let f = i as f32;
            let p = Vec3::new((f * 0.37).sin() * 8.0, (f * 0.91).cos() * 8.0, (f * 0.13).sin() * 8.0)
                + Vec3::new(1.0, 2.0, 3.0);
            let brute = (0..m.triangle_count())
                .map(|t| {
                    let [a, b, c] = m.tri(t);
                    (closest_point_triangle(p, a, b, c) - p).length()
                })
                .fold(f32::MAX, f32::min);
            let (_, d) = bvh.closest_point(p, 100.0).unwrap();
            assert!((d - brute).abs() < 1e-5, "{d} vs {brute}");
            assert!(bvh.closest_point(p, brute * 0.5).is_none() || brute == 0.0);
        }
        assert!(bvh.segment_near(Vec3::new(1.0, 2.0, -10.0), Vec3::new(1.0, 2.0, 10.0), 0.5));
        assert!(!bvh.segment_near(Vec3::new(10.0, 2.0, -10.0), Vec3::new(10.0, 2.0, 10.0), 0.5));
    }
}
