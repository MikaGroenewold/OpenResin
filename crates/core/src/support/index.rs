//! Uniform XY grid over triangles for fast vertical ray queries.

use glam::{Vec2, Vec3};

use crate::mesh::Mesh;

#[derive(Debug, Clone, Copy)]
pub struct VerticalHit {
    pub z: f32,
    pub normal: Vec3,
    pub triangle: u32,
}

pub struct VerticalRayIndex<'a> {
    mesh: &'a Mesh,
    origin: Vec2,
    cell: f32,
    nx: usize,
    ny: usize,
    cells: Vec<Vec<u32>>,
}

impl<'a> VerticalRayIndex<'a> {
    pub fn new(mesh: &'a Mesh) -> Self {
        let b = mesh.bounds();
        let size = b.size();
        let n = mesh.triangle_count().max(1) as f32;
        // aim for a handful of triangles per cell
        let cell = ((size.x.max(1e-3) * size.y.max(1e-3)) / n * 4.0).sqrt().max(0.05);
        let nx = ((size.x / cell).ceil() as usize).clamp(1, 4096);
        let ny = ((size.y / cell).ceil() as usize).clamp(1, 4096);
        let cell = (size.x / nx as f32).max(size.y / ny as f32).max(1e-3);
        let origin = Vec2::new(b.min.x, b.min.y);
        let mut cells = vec![Vec::new(); nx * ny];
        for i in 0..mesh.triangle_count() {
            let [a, bb, c] = mesh.tri(i);
            let minx = a.x.min(bb.x).min(c.x);
            let maxx = a.x.max(bb.x).max(c.x);
            let miny = a.y.min(bb.y).min(c.y);
            let maxy = a.y.max(bb.y).max(c.y);
            let cx0 = (((minx - origin.x) / cell).floor().max(0.0) as usize).min(nx - 1);
            let cx1 = (((maxx - origin.x) / cell).floor().max(0.0) as usize).min(nx - 1);
            let cy0 = (((miny - origin.y) / cell).floor().max(0.0) as usize).min(ny - 1);
            let cy1 = (((maxy - origin.y) / cell).floor().max(0.0) as usize).min(ny - 1);
            for cy in cy0..=cy1 {
                for cx in cx0..=cx1 {
                    cells[cy * nx + cx].push(i as u32);
                }
            }
        }
        Self {
            mesh,
            origin,
            cell,
            nx,
            ny,
            cells,
        }
    }

    /// All intersections of the vertical line through `(x, y)` with the mesh,
    /// sorted by ascending z.
    pub fn hits(&self, x: f32, y: f32) -> Vec<VerticalHit> {
        let cx = ((x - self.origin.x) / self.cell).floor();
        let cy = ((y - self.origin.y) / self.cell).floor();
        if cx < 0.0 || cy < 0.0 || cx as usize >= self.nx || cy as usize >= self.ny {
            return Vec::new();
        }
        let mut out = Vec::new();
        let p = Vec2::new(x, y);
        for &ti in &self.cells[cy as usize * self.nx + cx as usize] {
            let [a, b, c] = self.mesh.tri(ti as usize);
            if let Some(z) = vertical_intersection(p, a, b, c) {
                let n = (b - a).cross(c - a).normalize_or_zero();
                out.push(VerticalHit {
                    z,
                    normal: n,
                    triangle: ti,
                });
            }
        }
        out.sort_by(|a, b| a.z.total_cmp(&b.z));
        // remove duplicates produced by rays passing exactly through shared edges
        out.dedup_by(|a, b| (a.z - b.z).abs() < 1e-5 && a.normal.z.signum() == b.normal.z.signum());
        out
    }

    /// Is the point inside the (closed) mesh? Uses parity of hits above it.
    pub fn contains(&self, p: Vec3) -> bool {
        self.hits(p.x, p.y).iter().filter(|h| h.z > p.z).count() % 2 == 1
    }
}

#[inline]
fn vertical_intersection(p: Vec2, a: Vec3, b: Vec3, c: Vec3) -> Option<f32> {
    let v0 = b.truncate() - a.truncate();
    let v1 = c.truncate() - a.truncate();
    let v2 = p - a.truncate();
    let den = v0.x * v1.y - v1.x * v0.y;
    if den.abs() < 1e-12 {
        return None;
    }
    let v = (v2.x * v1.y - v1.x * v2.y) / den;
    let w = (v0.x * v2.y - v2.x * v0.y) / den;
    let u = 1.0 - v - w;
    const E: f32 = -1e-6;
    if u < E || v < E || w < E {
        return None;
    }
    Some(u * a.z + v * b.z + w * c.z)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives;

    #[test]
    fn cube_hits() {
        let m = primitives::cuboid(Vec3::new(0.0, 0.0, 5.0), Vec3::splat(10.0));
        let idx = VerticalRayIndex::new(&m);
        let h = idx.hits(1.3, 2.7);
        assert_eq!(h.len(), 2);
        assert!((h[0].z - 0.0).abs() < 1e-5 && h[0].normal.z < -0.9);
        assert!((h[1].z - 10.0).abs() < 1e-5 && h[1].normal.z > 0.9);
        assert!(idx.contains(Vec3::new(0.0, 0.0, 5.0)));
        assert!(!idx.contains(Vec3::new(0.0, 0.0, 15.0)));
    }
}
