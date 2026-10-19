//! Polygon triangulation: ear clipping with hole bridging.
//!
//! Unlike most ear clippers this one never drops vertices (collinear or
//! duplicate points are kept), so a cap triangulated from a mesh boundary loop
//! shares every edge with the surrounding surface and the result stays
//! watertight. When the input is not a simple polygon the algorithm degrades
//! gracefully: it still emits a topologically valid disk (every ring edge is
//! used exactly once), it just may contain inverted or zero-area triangles.

use glam::{DVec2, DVec3, Vec3};

/// Result of [`triangulate_rings`].
#[derive(Debug, Clone, Default)]
pub struct Triangulation {
    /// Triangles as indices into the concatenated ring points (ring 0 first),
    /// counter-clockwise for a counter-clockwise outer ring.
    pub triangles: Vec<[u32; 3]>,
    /// `false` when a fallback was needed (self intersecting or degenerate input,
    /// or a hole that could not be bridged).
    pub clean: bool,
}

#[inline]
fn cross(o: DVec2, a: DVec2, b: DVec2) -> f64 {
    (a - o).perp_dot(b - o)
}

fn signed_area(pts: &[DVec2]) -> f64 {
    let n = pts.len();
    let mut a = 0.0;
    for i in 0..n {
        a += pts[i].perp_dot(pts[(i + 1) % n]);
    }
    a * 0.5
}

/// Triangulates a polygon with holes. `rings[0]` is the outer boundary, all
/// other rings are holes inside it. Orientation of the input rings does not
/// matter; the output is counter-clockwise (positive area). Rings with fewer
/// than three points are ignored.
pub fn triangulate_rings(rings: &[&[DVec2]]) -> Triangulation {
    let mut pts: Vec<DVec2> = Vec::new();
    let mut ranges = Vec::with_capacity(rings.len());
    for r in rings {
        let s = pts.len();
        pts.extend_from_slice(r);
        ranges.push(s..pts.len());
    }
    if ranges.is_empty() || ranges[0].len() < 3 || pts.iter().any(|p| !p.is_finite()) {
        return Triangulation {
            triangles: Vec::new(),
            clean: ranges.first().is_none_or(|r| r.is_empty()),
        };
    }
    let mut l = Linked::new(&pts);
    let outer = l.ring(ranges[0].clone(), true);
    let mut clean = true;
    let mut holes: Vec<u32> = ranges[1..]
        .iter()
        .filter(|r| r.len() >= 3)
        .map(|r| {
            let start = l.ring(r.clone(), false);
            l.rightmost(start)
        })
        .collect();
    holes.sort_by(|&a, &b| l.p(b).x.total_cmp(&l.p(a).x));
    for h in holes {
        match l.find_bridge(h, outer) {
            Some(b) => l.split(b, h),
            None => clean = false,
        }
    }
    let mut out = Vec::with_capacity(pts.len());
    clean &= l.earclip(outer, &mut out);
    Triangulation { triangles: out, clean }
}

/// Newell normal of a closed 3D loop (length = twice the projected area).
pub fn newell_normal(points: &[Vec3]) -> DVec3 {
    let n = points.len();
    if n < 3 {
        return DVec3::ZERO;
    }
    let c = points.iter().fold(DVec3::ZERO, |a, p| a + p.as_dvec3()) / n as f64;
    let mut nn = DVec3::ZERO;
    for i in 0..n {
        let a = points[i].as_dvec3() - c;
        let b = points[(i + 1) % n].as_dvec3() - c;
        nn += a.cross(b);
    }
    nn
}

/// Orthonormal tangent frame `(u, v)` with `u x v = n`.
pub fn plane_frame(n: DVec3) -> (DVec3, DVec3) {
    let n = n.normalize_or(DVec3::Z);
    let helper = if n.x.abs() < 0.6 { DVec3::X } else { DVec3::Y };
    let u = helper.cross(n).normalize();
    let v = n.cross(u);
    (u, v)
}

/// Triangulates a closed, possibly non-planar 3D loop by projecting it onto its
/// best-fit (Newell) plane. Triangles are oriented so that their normals point
/// to the side from which the loop appears counter-clockwise. Returns the
/// triangulation and whether it was clean (see [`Triangulation::clean`]).
pub fn triangulate_loop_3d(points: &[Vec3]) -> Triangulation {
    let n = newell_normal(points);
    if n.length_squared() <= 0.0 || !n.is_finite() {
        return Triangulation {
            triangles: fan(points.len()),
            clean: false,
        };
    }
    let (u, v) = plane_frame(n);
    let p2: Vec<DVec2> = points
        .iter()
        .map(|p| {
            let p = p.as_dvec3();
            DVec2::new(p.dot(u), p.dot(v))
        })
        .collect();
    triangulate_rings(&[&p2])
}

/// Fan triangulation `(0, i, i + 1)` of a ring with `n` vertices.
pub fn fan(n: usize) -> Vec<[u32; 3]> {
    (1..n.saturating_sub(1)).map(|i| [0, i as u32, i as u32 + 1]).collect()
}

/// Circular doubly linked list of polygon nodes. Several nodes may refer to the
/// same point (bridge endpoints are duplicated).
struct Linked<'a> {
    pts: &'a [DVec2],
    pt: Vec<u32>,
    prev: Vec<u32>,
    next: Vec<u32>,
    alive: Vec<bool>,
}

impl<'a> Linked<'a> {
    fn new(pts: &'a [DVec2]) -> Self {
        let cap = pts.len() + 16;
        Self {
            pts,
            pt: Vec::with_capacity(cap),
            prev: Vec::with_capacity(cap),
            next: Vec::with_capacity(cap),
            alive: Vec::with_capacity(cap),
        }
    }

    #[inline]
    fn p(&self, n: u32) -> DVec2 {
        self.pts[self.pt[n as usize] as usize]
    }

    #[inline]
    fn next(&self, n: u32) -> u32 {
        self.next[n as usize]
    }

    #[inline]
    fn prev(&self, n: u32) -> u32 {
        self.prev[n as usize]
    }

    fn new_node(&mut self, pt: u32) -> u32 {
        let i = self.pt.len() as u32;
        self.pt.push(pt);
        self.prev.push(i);
        self.next.push(i);
        self.alive.push(true);
        i
    }

    /// Links points `range` into a ring, counter-clockwise when `ccw` is set,
    /// clockwise otherwise. Returns one node of the ring.
    fn ring(&mut self, range: std::ops::Range<usize>, ccw: bool) -> u32 {
        let area = signed_area(&self.pts[range.clone()]);
        let forward = (area >= 0.0) == ccw;
        let ids: Vec<u32> = if forward {
            range.map(|i| i as u32).collect()
        } else {
            range.rev().map(|i| i as u32).collect()
        };
        let first = self.pt.len() as u32;
        let n = ids.len() as u32;
        for (k, &pid) in ids.iter().enumerate() {
            let node = self.new_node(pid);
            let k = k as u32;
            self.prev[node as usize] = first + (k + n - 1) % n;
            self.next[node as usize] = first + (k + 1) % n;
        }
        first
    }

    fn rightmost(&self, start: u32) -> u32 {
        let mut best = start;
        let mut n = self.next(start);
        while n != start {
            let (p, b) = (self.p(n), self.p(best));
            if p.x > b.x || (p.x == b.x && p.y < b.y) {
                best = n;
            }
            n = self.next(n);
        }
        best
    }

    fn convexity(&self, n: u32) -> f64 {
        cross(self.p(self.prev(n)), self.p(n), self.p(self.next(n)))
    }

    /// Is the direction from node `a` towards point `m` inside the polygon's
    /// interior angle at `a` (ring is counter-clockwise)?
    fn locally_inside(&self, a: u32, m: DVec2) -> bool {
        let pa = self.p(a);
        let u = self.p(self.next(a)) - pa;
        let v = self.p(self.prev(a)) - pa;
        let d = m - pa;
        let c = u.perp_dot(v);
        if c > 0.0 {
            u.perp_dot(d) > 0.0 && d.perp_dot(v) > 0.0
        } else if c < 0.0 {
            u.perp_dot(d) > 0.0 || d.perp_dot(v) > 0.0
        } else if u.dot(v) < 0.0 {
            // straight angle
            u.perp_dot(d) > 0.0
        } else {
            // zero-width spike: everything but the spike direction is inside
            u.perp_dot(d) != 0.0 || u.dot(d) < 0.0
        }
    }

    /// Finds an outer ring node visible from hole node `m` (ray towards +x).
    fn find_bridge(&self, m: u32, outer: u32) -> Option<u32> {
        let mp = self.p(m);
        let mut qx = f64::INFINITY;
        let mut cand = None;
        let mut a = outer;
        loop {
            let b = self.next(a);
            let (pa, pb) = (self.p(a), self.p(b));
            if pa.y <= mp.y && pb.y >= mp.y && pb.y != pa.y {
                let x = pa.x + (mp.y - pa.y) * (pb.x - pa.x) / (pb.y - pa.y);
                if x >= mp.x && x < qx {
                    qx = x;
                    cand = Some(if pa.y == mp.y {
                        a
                    } else if pb.y == mp.y {
                        b
                    } else if pa.x > pb.x {
                        a
                    } else {
                        b
                    });
                }
            }
            a = b;
            if a == outer {
                break;
            }
        }
        let mut best = cand?;
        let bp = self.p(best);
        if qx != mp.x && bp.y != mp.y {
            // Reflex vertices inside the triangle (M, I, P) may block the view;
            // choose the one with the smallest angle to the ray instead.
            let (ta, tb, tc) = (mp, DVec2::new(qx, mp.y), bp);
            let mut tan_min = f64::INFINITY;
            let mut n = outer;
            loop {
                let q = self.p(n);
                if q.x >= mp.x && q.x <= bp.x && q.x != mp.x && point_in_triangle_closed(ta, tb, tc, q) {
                    let tan = (q.y - mp.y).abs() / (q.x - mp.x);
                    if self.locally_inside(n, mp) && (tan < tan_min || (tan == tan_min && q.x < self.p(best).x)) {
                        best = n;
                        tan_min = tan;
                    }
                }
                n = self.next(n);
                if n == outer {
                    break;
                }
            }
        }
        if !self.locally_inside(best, mp) {
            // duplicated bridge points: pick the copy whose sector faces the hole
            let bpos = self.p(best);
            let mut n = outer;
            loop {
                if n != best && self.p(n) == bpos && self.locally_inside(n, mp) {
                    best = n;
                    break;
                }
                n = self.next(n);
                if n == outer {
                    break;
                }
            }
        }
        Some(best)
    }

    /// Connects outer node `p` with hole node `m`, merging the two rings.
    fn split(&mut self, p: u32, m: u32) {
        let pn = self.next(p);
        let mprev = self.prev(m);
        let p2 = self.new_node(self.pt[p as usize]);
        let m2 = self.new_node(self.pt[m as usize]);
        self.next[p as usize] = m;
        self.prev[m as usize] = p;
        self.next[mprev as usize] = m2;
        self.prev[m2 as usize] = mprev;
        self.next[m2 as usize] = p2;
        self.prev[p2 as usize] = m2;
        self.next[p2 as usize] = pn;
        self.prev[pn as usize] = p2;
    }

    fn earclip(&mut self, start: u32, out: &mut Vec<[u32; 3]>) -> bool {
        // collect ring
        let mut ring = vec![start];
        let mut n = self.next(start);
        while n != start {
            ring.push(n);
            n = self.next(n);
            if ring.len() > self.pt.len() {
                return false; // corrupted links; should never happen
            }
        }
        let mut count = ring.len();
        if count < 3 {
            return true;
        }
        let blockers: Vec<u32> = ring.iter().copied().filter(|&n| self.convexity(n) <= 0.0).collect();
        let grid = BlockerGrid::new(&blockers, |n| self.p(n));
        let mut clean = true;
        let mut ear = start;
        let mut stop = ear;
        let mut stage = 0u8;
        while count > 3 {
            let ok = match stage {
                0 => self.is_ear(ear, true, &grid),
                1 => self.is_ear(ear, false, &grid),
                _ => {
                    // last resort: clip the most convex vertex of the remaining ring
                    let mut best = ear;
                    let mut best_c = f64::NEG_INFINITY;
                    let mut n = ear;
                    loop {
                        let c = self.convexity(n);
                        if c > best_c {
                            best_c = c;
                            best = n;
                        }
                        n = self.next(n);
                        if n == ear {
                            break;
                        }
                    }
                    ear = best;
                    true
                }
            };
            if ok {
                let a = self.prev(ear);
                let c = self.next(ear);
                out.push([self.pt[a as usize], self.pt[ear as usize], self.pt[c as usize]]);
                self.next[a as usize] = c;
                self.prev[c as usize] = a;
                self.alive[ear as usize] = false;
                count -= 1;
                ear = c;
                stop = ear;
                stage = 0;
                continue;
            }
            ear = self.next(ear);
            if ear == stop {
                stage += 1;
                clean = false;
            }
        }
        let a = self.prev(ear);
        let c = self.next(ear);
        out.push([self.pt[a as usize], self.pt[ear as usize], self.pt[c as usize]]);
        clean
    }

    fn is_ear(&self, b: u32, strict: bool, grid: &BlockerGrid) -> bool {
        let a = self.prev(b);
        let c = self.next(b);
        let (pa, pb, pc) = (self.p(a), self.p(b), self.p(c));
        if cross(pa, pb, pc) <= 0.0 {
            return false;
        }
        let min = pa.min(pb).min(pc);
        let max = pa.max(pb).max(pc);
        let blocked = grid.any_in(min, max, |q| {
            if !self.alive[q as usize] || q == a || q == b || q == c {
                return false;
            }
            let pq = self.p(q);
            if pq == pa || pq == pb || pq == pc {
                return false;
            }
            if self.convexity(q) > 0.0 {
                return false;
            }
            let d1 = cross(pa, pb, pq);
            let d2 = cross(pb, pc, pq);
            let d3 = cross(pc, pa, pq);
            if strict {
                d1 >= 0.0 && d2 >= 0.0 && d3 >= 0.0
            } else {
                d1 > 0.0 && d2 > 0.0 && d3 > 0.0
            }
        });
        !blocked
    }
}

fn point_in_triangle_closed(a: DVec2, b: DVec2, c: DVec2, p: DVec2) -> bool {
    let d1 = cross(a, b, p);
    let d2 = cross(b, c, p);
    let d3 = cross(c, a, p);
    (d1 >= 0.0 && d2 >= 0.0 && d3 >= 0.0) || (d1 <= 0.0 && d2 <= 0.0 && d3 <= 0.0)
}

/// Uniform grid over the initially reflex vertices (the only ones that can
/// block an ear). Small rings use a plain list.
struct BlockerGrid {
    origin: DVec2,
    inv_cell: f64,
    nx: usize,
    ny: usize,
    cells: Vec<Vec<u32>>,
    list: Vec<(u32, DVec2)>,
}

impl BlockerGrid {
    fn new(nodes: &[u32], pos: impl Fn(u32) -> DVec2) -> Self {
        let list: Vec<(u32, DVec2)> = nodes.iter().map(|&n| (n, pos(n))).collect();
        if list.len() < 64 {
            return Self {
                origin: DVec2::ZERO,
                inv_cell: 0.0,
                nx: 0,
                ny: 0,
                cells: Vec::new(),
                list,
            };
        }
        let mut min = DVec2::splat(f64::INFINITY);
        let mut max = DVec2::splat(f64::NEG_INFINITY);
        for (_, p) in &list {
            min = min.min(*p);
            max = max.max(*p);
        }
        let size = (max - min).max(DVec2::splat(1e-12));
        let side = (list.len() as f64).sqrt().ceil().clamp(1.0, 1024.0);
        let cell = (size.x.max(size.y) / side).max(1e-12);
        let nx = ((size.x / cell).ceil() as usize).clamp(1, 2048);
        let ny = ((size.y / cell).ceil() as usize).clamp(1, 2048);
        let inv_cell = 1.0 / cell;
        let mut cells = vec![Vec::new(); nx * ny];
        for (n, p) in &list {
            let cx = (((p.x - min.x) * inv_cell) as usize).min(nx - 1);
            let cy = (((p.y - min.y) * inv_cell) as usize).min(ny - 1);
            cells[cy * nx + cx].push(*n);
        }
        Self {
            origin: min,
            inv_cell,
            nx,
            ny,
            cells,
            list: Vec::new(),
        }
    }

    fn any_in(&self, min: DVec2, max: DVec2, mut f: impl FnMut(u32) -> bool) -> bool {
        if self.nx == 0 {
            return self
                .list
                .iter()
                .any(|(n, p)| p.x >= min.x && p.x <= max.x && p.y >= min.y && p.y <= max.y && f(*n));
        }
        let to_cell = |v: f64, o: f64, n: usize| -> usize {
            let c = (v - o) * self.inv_cell;
            if c <= 0.0 {
                0
            } else {
                (c as usize).min(n - 1)
            }
        };
        let x0 = to_cell(min.x, self.origin.x, self.nx);
        let x1 = to_cell(max.x, self.origin.x, self.nx);
        let y0 = to_cell(min.y, self.origin.y, self.ny);
        let y1 = to_cell(max.y, self.origin.y, self.ny);
        for cy in y0..=y1 {
            for cx in x0..=x1 {
                for &n in &self.cells[cy * self.nx + cx] {
                    if f(n) {
                        return true;
                    }
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area_of(pts: &[DVec2], tris: &[[u32; 3]]) -> f64 {
        tris.iter()
            .map(|t| cross(pts[t[0] as usize], pts[t[1] as usize], pts[t[2] as usize]) * 0.5)
            .sum()
    }

    fn edges_balanced(n_pts: usize, ring_lens: &[usize], tris: &[[u32; 3]]) {
        // every ring edge must be used exactly once (in one direction) and every
        // interior diagonal exactly twice in opposite directions
        let mut m = std::collections::HashMap::<(u32, u32), i32>::new();
        for t in tris {
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                *m.entry((a, b)).or_default() += 1;
            }
        }
        let _ = n_pts;
        let mut boundary = 0;
        for (&(a, b), &c) in &m {
            let back = m.get(&(b, a)).copied().unwrap_or(0);
            if c != back {
                boundary += (c - back).abs();
            }
        }
        let expected: usize = ring_lens.iter().sum();
        assert_eq!(boundary as usize, expected, "boundary edge count");
    }

    #[test]
    fn square_with_collinear_points_keeps_all_vertices() {
        let pts: Vec<DVec2> = [
            (0., 0.),
            (1., 0.),
            (2., 0.),
            (2., 1.),
            (2., 2.),
            (1., 2.),
            (0., 2.),
            (0., 1.),
        ]
        .iter()
        .map(|&(x, y)| DVec2::new(x, y))
        .collect();
        let t = triangulate_rings(&[&pts]);
        assert!(t.clean);
        assert_eq!(t.triangles.len(), 6);
        assert!((area_of(&pts, &t.triangles) - 4.0).abs() < 1e-12);
        assert!(t
            .triangles
            .iter()
            .all(|tr| cross(pts[tr[0] as usize], pts[tr[1] as usize], pts[tr[2] as usize]) > 0.0));
        edges_balanced(pts.len(), &[8], &t.triangles);
    }

    #[test]
    fn clockwise_input_and_holes() {
        // outer 10x10 (clockwise), two square holes (counter-clockwise)
        let outer: Vec<DVec2> = [(0., 0.), (0., 10.), (10., 10.), (10., 0.)]
            .iter()
            .map(|&(x, y)| DVec2::new(x, y))
            .collect();
        let h1: Vec<DVec2> = [(1., 1.), (3., 1.), (3., 3.), (1., 3.)]
            .iter()
            .map(|&(x, y)| DVec2::new(x, y))
            .collect();
        let h2: Vec<DVec2> = [(6., 5.), (8., 5.), (8., 9.), (6., 9.)]
            .iter()
            .map(|&(x, y)| DVec2::new(x, y))
            .collect();
        let t = triangulate_rings(&[&outer, &h1, &h2]);
        assert!(t.clean);
        let mut all = outer.clone();
        all.extend(&h1);
        all.extend(&h2);
        let a = area_of(&all, &t.triangles);
        assert!((a - (100.0 - 4.0 - 8.0)).abs() < 1e-9, "area {a}");
        assert_eq!(t.triangles.len(), 12 + 2 * 2 - 2);
        assert!(t
            .triangles
            .iter()
            .all(|tr| cross(all[tr[0] as usize], all[tr[1] as usize], all[tr[2] as usize]) > 0.0));
        edges_balanced(all.len(), &[4, 4, 4], &t.triangles);
    }

    #[test]
    fn many_holes_and_large_circle() {
        let n = 2000;
        let outer: Vec<DVec2> = (0..n)
            .map(|i| {
                let a = i as f64 / n as f64 * std::f64::consts::TAU;
                DVec2::new(a.cos() * 50.0, a.sin() * 50.0) * (1.0 + 0.05 * (a * 17.0).sin())
            })
            .collect();
        let mut rings: Vec<Vec<DVec2>> = vec![outer];
        for gx in -3..=3 {
            for gy in -3..=3 {
                let c = DVec2::new(gx as f64 * 9.0, gy as f64 * 9.0);
                rings.push(
                    (0..12)
                        .map(|i| {
                            let a = -(i as f64) / 12.0 * std::f64::consts::TAU;
                            c + DVec2::new(a.cos(), a.sin()) * 2.0
                        })
                        .collect(),
                );
            }
        }
        let refs: Vec<&[DVec2]> = rings.iter().map(|r| r.as_slice()).collect();
        let t = triangulate_rings(&refs);
        assert!(t.clean);
        let all: Vec<DVec2> = rings.concat();
        let expected: f64 = rings.iter().map(|r| signed_area(r)).sum();
        let a = area_of(&all, &t.triangles);
        assert!((a - expected).abs() < 1e-6 * expected.abs(), "{a} vs {expected}");
        let lens: Vec<usize> = rings.iter().map(|r| r.len()).collect();
        edges_balanced(all.len(), &lens, &t.triangles);
    }

    #[test]
    fn self_intersecting_input_still_yields_a_disk() {
        // bow tie
        let pts: Vec<DVec2> = [(0., 0.), (2., 2.), (2., 0.), (0., 2.)]
            .iter()
            .map(|&(x, y)| DVec2::new(x, y))
            .collect();
        let t = triangulate_rings(&[&pts]);
        assert_eq!(t.triangles.len(), 2);
        edges_balanced(pts.len(), &[4], &t.triangles);
        // fully degenerate
        let line: Vec<DVec2> = (0..5).map(|i| DVec2::new(i as f64, 0.0)).collect();
        let t = triangulate_rings(&[&line]);
        assert_eq!(t.triangles.len(), 3);
    }

    #[test]
    fn non_planar_loop() {
        // a saddle-shaped loop
        let pts: Vec<Vec3> = (0..16)
            .map(|i| {
                let a = i as f32 / 16.0 * std::f32::consts::TAU;
                Vec3::new(a.cos() * 5.0, a.sin() * 5.0, (2.0 * a).sin())
            })
            .collect();
        let t = triangulate_loop_3d(&pts);
        assert!(t.clean);
        assert_eq!(t.triangles.len(), 14);
        // triangles must face +Z (the loop is CCW seen from above)
        for tr in &t.triangles {
            let (a, b, c) = (pts[tr[0] as usize], pts[tr[1] as usize], pts[tr[2] as usize]);
            assert!((b - a).cross(c - a).z > 0.0);
        }
    }
}
