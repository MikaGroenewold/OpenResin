use std::collections::HashMap;

use glam::{Mat4, Vec3};

use crate::geometry::Aabb;

/// Indexed triangle mesh.
#[derive(Debug, Clone, Default)]
pub struct Mesh {
    pub positions: Vec<Vec3>,
    pub triangles: Vec<[u32; 3]>,
}

impl Mesh {
    pub fn new(positions: Vec<Vec3>, triangles: Vec<[u32; 3]>) -> Self {
        Self { positions, triangles }
    }

    /// Builds an indexed mesh from a triangle soup, welding identical vertices.
    pub fn from_soup(tris: &[[Vec3; 3]]) -> Self {
        let mut map: HashMap<[u32; 3], u32> = HashMap::with_capacity(tris.len() / 2 + 16);
        let mut positions = Vec::with_capacity(tris.len() / 2 + 16);
        let mut triangles = Vec::with_capacity(tris.len());
        for t in tris {
            let mut idx = [0u32; 3];
            for (k, v) in t.iter().enumerate() {
                // `+ 0.0` turns -0.0 into 0.0 so both weld into one vertex
                let key = [(v.x + 0.0).to_bits(), (v.y + 0.0).to_bits(), (v.z + 0.0).to_bits()];
                idx[k] = *map.entry(key).or_insert_with(|| {
                    positions.push(*v);
                    (positions.len() - 1) as u32
                });
            }
            if idx[0] != idx[1] && idx[1] != idx[2] && idx[0] != idx[2] {
                triangles.push(idx);
            }
        }
        Self { positions, triangles }
    }

    pub fn is_empty(&self) -> bool {
        self.triangles.is_empty()
    }

    pub fn triangle_count(&self) -> usize {
        self.triangles.len()
    }

    #[inline]
    pub fn tri(&self, i: usize) -> [Vec3; 3] {
        let t = self.triangles[i];
        [
            self.positions[t[0] as usize],
            self.positions[t[1] as usize],
            self.positions[t[2] as usize],
        ]
    }

    pub fn bounds(&self) -> Aabb {
        Aabb::from_points(self.positions.iter())
    }

    /// Bounds of the mesh after applying `m`, without allocating a new mesh.
    pub fn transformed_bounds(&self, m: &Mat4) -> Aabb {
        let mut b = Aabb::EMPTY;
        for p in &self.positions {
            b.extend(m.transform_point3(*p));
        }
        b
    }

    /// Signed volume (positive when triangles are oriented outward).
    pub fn signed_volume(&self) -> f64 {
        let mut v = 0.0f64;
        for t in &self.triangles {
            let a = self.positions[t[0] as usize].as_dvec3();
            let b = self.positions[t[1] as usize].as_dvec3();
            let c = self.positions[t[2] as usize].as_dvec3();
            v += a.dot(b.cross(c));
        }
        v / 6.0
    }

    pub fn surface_area(&self) -> f64 {
        (0..self.triangles.len())
            .map(|i| {
                let [a, b, c] = self.tri(i);
                ((b - a).cross(c - a).length() * 0.5) as f64
            })
            .sum()
    }

    /// Flips the winding of all triangles.
    pub fn flip(&mut self) {
        for t in &mut self.triangles {
            t.swap(1, 2);
        }
    }

    /// Makes sure the mesh is oriented outward (positive volume).
    pub fn orient_outward(&mut self) {
        if self.signed_volume() < 0.0 {
            self.flip();
        }
    }

    /// Returns a copy transformed by `m`. Winding is flipped for mirroring transforms
    /// so that the result stays oriented outward.
    pub fn transformed(&self, m: &Mat4) -> Mesh {
        let positions = self.positions.iter().map(|p| m.transform_point3(*p)).collect();
        let mut out = Mesh {
            positions,
            triangles: self.triangles.clone(),
        };
        if m.determinant() < 0.0 {
            out.flip();
        }
        out
    }

    pub fn transform_in_place(&mut self, m: &Mat4) {
        for p in &mut self.positions {
            *p = m.transform_point3(*p);
        }
        if m.determinant() < 0.0 {
            self.flip();
        }
    }

    pub fn append(&mut self, other: &Mesh) {
        let base = self.positions.len() as u32;
        self.positions.extend_from_slice(&other.positions);
        self.triangles
            .extend(other.triangles.iter().map(|t| [t[0] + base, t[1] + base, t[2] + base]));
    }

    /// Removes zero-area triangles and unreferenced vertices.
    pub fn cleanup(&mut self) {
        let pos = &self.positions;
        self.triangles.retain(|t| {
            let a = pos[t[0] as usize];
            let b = pos[t[1] as usize];
            let c = pos[t[2] as usize];
            (b - a).cross(c - a).length_squared() > 1e-18
        });
        let mut remap = vec![u32::MAX; self.positions.len()];
        let mut positions = Vec::with_capacity(self.positions.len());
        for t in &mut self.triangles {
            for i in t.iter_mut() {
                let r = &mut remap[*i as usize];
                if *r == u32::MAX {
                    *r = positions.len() as u32;
                    positions.push(self.positions[*i as usize]);
                }
                *i = *r;
            }
        }
        self.positions = positions;
    }

    /// Number of edges that are not shared by exactly two triangles.
    pub fn open_edge_count(&self) -> usize {
        let mut edges: HashMap<(u32, u32), i32> = HashMap::with_capacity(self.triangles.len() * 3 / 2);
        for t in &self.triangles {
            for k in 0..3 {
                let a = t[k];
                let b = t[(k + 1) % 3];
                let key = if a < b { (a, b) } else { (b, a) };
                *edges.entry(key).or_insert(0) += 1;
            }
        }
        edges.values().filter(|&&c| c != 2).count()
    }

    pub fn face_normal(&self, i: usize) -> Vec3 {
        let [a, b, c] = self.tri(i);
        (b - a).cross(c - a).normalize_or_zero()
    }

    /// Flat, non-indexed position buffer (9 floats per triangle) for rendering.
    pub fn to_flat_positions(&self) -> Vec<f32> {
        let mut v = Vec::with_capacity(self.triangles.len() * 9);
        for t in &self.triangles {
            for &i in t {
                let p = self.positions[i as usize];
                v.extend_from_slice(&[p.x, p.y, p.z]);
            }
        }
        v
    }

    /// Rotation that lays the face `face` flat on the build plate (face normal -> -Z).
    pub fn lay_flat_rotation(&self, face: usize) -> glam::Quat {
        let n = self.face_normal(face);
        glam::Quat::from_rotation_arc(n, -Vec3::Z)
    }

    /// Heuristic "auto orient": finds the largest group of coplanar-ish faces among
    /// the convex hull candidates and returns the rotation that puts it on the plate.
    pub fn best_flat_rotation(&self) -> glam::Quat {
        // per normal bucket: area-weighted normal sum and total area
        let mut buckets: HashMap<(i32, i32, i32), (glam::DVec3, f64)> = HashMap::new();
        for i in 0..self.triangles.len() {
            let [a, b, c] = self.tri(i);
            let cr = (b - a).cross(c - a);
            let area = cr.length() as f64 * 0.5;
            if area <= 0.0 {
                continue;
            }
            let n = cr / (area as f32 * 2.0);
            let key = (
                (n.x * 50.0).round() as i32,
                (n.y * 50.0).round() as i32,
                (n.z * 50.0).round() as i32,
            );
            let e = buckets.entry(key).or_insert((glam::DVec3::ZERO, 0.0));
            e.0 += cr.as_dvec3();
            e.1 += area;
        }
        // the bucket key is a quantised direction; use the mean normal so that
        // the chosen face ends up exactly flat
        let best = buckets
            .into_values()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(n, _)| n.normalize_or_zero().as_vec3());
        match best {
            Some(n) if n != Vec3::ZERO => glam::Quat::from_rotation_arc(n, -Vec3::Z),
            _ => glam::Quat::IDENTITY,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives;

    #[test]
    fn cube_volume_and_orientation() {
        let mut m = primitives::cuboid(Vec3::ZERO, Vec3::new(2.0, 3.0, 4.0));
        assert!((m.signed_volume() - 24.0).abs() < 1e-4);
        assert_eq!(m.open_edge_count(), 0);
        m.flip();
        assert!(m.signed_volume() < 0.0);
        m.orient_outward();
        assert!(m.signed_volume() > 0.0);
        let mirrored = m.transformed(&Mat4::from_scale(Vec3::new(-1.0, 1.0, 1.0)));
        assert!(mirrored.signed_volume() > 0.0);
    }

    #[test]
    fn soup_welding() {
        let m = primitives::cuboid(Vec3::ZERO, Vec3::ONE);
        let soup: Vec<[Vec3; 3]> = (0..m.triangle_count()).map(|i| m.tri(i)).collect();
        let w = Mesh::from_soup(&soup);
        assert_eq!(w.positions.len(), 8);
        assert_eq!(w.open_edge_count(), 0);
    }
}
