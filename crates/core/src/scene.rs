//! Build plate scene: placed models and their supports.

use std::sync::Arc;

use glam::{Mat3, Mat4, Quat, Vec2, Vec3};
use serde::{Deserialize, Serialize};

use crate::geometry::Aabb;
use crate::mesh::Mesh;
use crate::profile::PrinterProfile;
use crate::support::{self, SupportPoint, SupportSettings};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            translation: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
        }
    }
}

impl Transform {
    pub fn matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }
}

pub type ObjectId = u64;

/// What an object contributes to the print.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectRole {
    /// Printed material.
    #[default]
    Solid,
    /// Removed from every solid it overlaps (boolean difference).
    Subtract,
}

#[derive(Debug, Clone)]
pub struct SceneObject {
    pub id: ObjectId,
    pub name: String,
    /// Mesh in local coordinates, centred on its bounding box centre.
    pub mesh: Arc<Mesh>,
    pub transform: Transform,
    /// Support contact points in local (mesh) coordinates.
    pub supports: Vec<SupportPoint>,
    pub support_settings: SupportSettings,
    pub role: ObjectRole,
    /// Volumes removed from this object (hollowing cavity, drain holes,
    /// engraved text), in local coordinates like `mesh`.
    pub cutouts: Vec<Arc<Mesh>>,
}

impl SceneObject {
    pub fn world_mesh(&self) -> Mesh {
        self.mesh.transformed(&self.transform.matrix())
    }

    pub fn world_bounds(&self) -> Aabb {
        self.mesh.transformed_bounds(&self.transform.matrix())
    }

    /// Support points converted to world (plate) coordinates.
    pub fn world_support_points(&self) -> Vec<SupportPoint> {
        let m = self.transform.matrix();
        let nm = Mat3::from_mat4(m).inverse().transpose();
        self.supports
            .iter()
            .map(|p| SupportPoint {
                position: m.transform_point3(p.position),
                normal: (nm * p.normal).normalize_or_zero(),
                ..*p
            })
            .collect()
    }

    pub fn set_world_support_points(&mut self, pts: &[SupportPoint]) {
        let m = self.transform.matrix();
        let inv = m.inverse();
        let nm = Mat3::from_mat4(m).transpose();
        self.supports = pts
            .iter()
            .map(|p| SupportPoint {
                position: inv.transform_point3(p.position),
                normal: (nm * p.normal).normalize_or_zero(),
                ..*p
            })
            .collect();
    }

    /// Generated support structure (including raft) in world coordinates.
    pub fn support_mesh(&self) -> support::SupportGeometry {
        if self.supports.is_empty() {
            return support::SupportGeometry::default();
        }
        support::build(&self.world_mesh(), &self.world_support_points(), &self.support_settings)
    }

    /// Height of the lowest model point above the plate.
    pub fn elevation(&self) -> f32 {
        self.world_bounds().min.z
    }
}

#[derive(Debug, Clone, Default)]
pub struct Scene {
    pub objects: Vec<SceneObject>,
    next_id: ObjectId,
}

impl Scene {
    /// Adds a mesh; it is re-centred and dropped onto the plate at the origin.
    pub fn add(&mut self, name: impl Into<String>, mut mesh: Mesh) -> ObjectId {
        let b = mesh.bounds();
        if !b.is_empty() {
            mesh.transform_in_place(&Mat4::from_translation(-b.center()));
        }
        self.next_id += 1;
        let id = self.next_id;
        let mut obj = SceneObject {
            id,
            name: name.into(),
            mesh: Arc::new(mesh),
            transform: Transform::default(),
            supports: Vec::new(),
            support_settings: SupportSettings::default(),
            role: ObjectRole::Solid,
            cutouts: Vec::new(),
        };
        let b = obj.world_bounds();
        if !b.is_empty() {
            obj.transform.translation.z -= b.min.z;
        }
        self.objects.push(obj);
        id
    }

    /// Inserts an object with an explicit id (used when loading projects).
    pub fn insert(&mut self, obj: SceneObject) {
        self.next_id = self.next_id.max(obj.id);
        self.objects.push(obj);
    }

    pub fn get(&self, id: ObjectId) -> Option<&SceneObject> {
        self.objects.iter().find(|o| o.id == id)
    }

    pub fn get_mut(&mut self, id: ObjectId) -> Option<&mut SceneObject> {
        self.objects.iter_mut().find(|o| o.id == id)
    }

    pub fn remove(&mut self, id: ObjectId) -> Option<SceneObject> {
        let idx = self.objects.iter().position(|o| o.id == id)?;
        Some(self.objects.remove(idx))
    }

    pub fn duplicate(&mut self, id: ObjectId) -> Option<ObjectId> {
        let mut obj = self.get(id)?.clone();
        self.next_id += 1;
        obj.id = self.next_id;
        obj.name = format!("{} copy", obj.name);
        let size = obj.world_bounds().size();
        obj.transform.translation.x += size.x + 2.0;
        let new_id = obj.id;
        self.objects.push(obj);
        Some(new_id)
    }

    /// Moves the object vertically so its lowest point sits at `elevation`.
    pub fn drop_to(&mut self, id: ObjectId, elevation: f32) {
        if let Some(o) = self.get_mut(id) {
            let b = o.world_bounds();
            if !b.is_empty() {
                o.transform.translation.z += elevation - b.min.z;
            }
        }
    }

    pub fn bounds(&self) -> Aabb {
        self.objects
            .iter()
            .fold(Aabb::EMPTY, |acc, o| acc.union(&o.world_bounds()))
    }

    /// Objects (by id) that leave the printable volume.
    pub fn out_of_bounds(&self, printer: &PrinterProfile) -> Vec<ObjectId> {
        let hx = printer.size_x * 0.5;
        let hy = printer.size_y * 0.5;
        self.objects
            .iter()
            .filter(|o| o.role == ObjectRole::Solid)
            .filter(|o| {
                let b = o.world_bounds();
                b.min.x < -hx
                    || b.max.x > hx
                    || b.min.y < -hy
                    || b.max.y > hy
                    || b.max.z > printer.size_z
                    || b.min.z < -1e-3
            })
            .map(|o| o.id)
            .collect()
    }

    /// Arranges all objects on the plate using a simple shelf packer.
    /// Returns false if not everything fits.
    pub fn arrange(&mut self, printer: &PrinterProfile, spacing: f32) -> bool {
        let n = self.objects.len();
        let bounds: Vec<Aabb> = self.objects.iter().map(|o| o.world_bounds()).collect();
        // cutters stay where they are relative to the parts they cut: each one
        // moves with the solid whose bounding box it overlaps most
        let overlap = |a: &Aabb, b: &Aabb| (a.max.min(b.max) - a.min.max(b.min)).max(Vec3::ZERO).element_product();
        let parent: Vec<Option<usize>> = (0..n)
            .map(|i| {
                if self.objects[i].role != ObjectRole::Subtract {
                    return None;
                }
                (0..n)
                    .filter(|&j| self.objects[j].role == ObjectRole::Solid)
                    .map(|j| (j, overlap(&bounds[i], &bounds[j])))
                    .filter(|&(_, v)| v > 0.0)
                    .max_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(j, _)| j)
            })
            .collect();
        let mut order: Vec<usize> = (0..n)
            .filter(|&i| self.objects[i].role == ObjectRole::Solid && !bounds[i].is_empty())
            .collect();
        let sizes: Vec<Vec2> = bounds.iter().map(|b| b.size().truncate()).collect();
        order.sort_by(|&a, &b| sizes[b].y.total_cmp(&sizes[a].y));
        let w = printer.size_x;
        let h = printer.size_y;
        let mut x = 0f32;
        let mut y = 0f32;
        let mut shelf_h = 0f32;
        let mut placed = Vec::with_capacity(order.len());
        let mut fits = true;
        for &i in &order {
            let s = sizes[i];
            if x > 0.0 && x + s.x > w {
                x = 0.0;
                y += shelf_h + spacing;
                shelf_h = 0.0;
            }
            if y + s.y > h || s.x > w {
                fits = false;
            }
            placed.push((i, Vec2::new(x + s.x * 0.5, y + s.y * 0.5)));
            x += s.x + spacing;
            shelf_h = shelf_h.max(s.y);
        }
        // centre the packed block on the plate
        let used_w = placed.iter().map(|(i, c)| c.x + sizes[*i].x * 0.5).fold(0f32, f32::max);
        let used_h = placed.iter().map(|(i, c)| c.y + sizes[*i].y * 0.5).fold(0f32, f32::max);
        let offset = Vec2::new(-used_w * 0.5, -used_h * 0.5);
        let mut moved = vec![Vec2::ZERO; n];
        for (i, c) in placed {
            moved[i] = c + offset - bounds[i].center().truncate();
        }
        for (i, o) in self.objects.iter_mut().enumerate() {
            let d = parent[i].map_or(moved[i], |p| moved[p]);
            o.transform.translation += d.extend(0.0);
        }
        fits
    }

    /// All solid meshes to slice (models and supports) in plate coordinates.
    pub fn slice_meshes(&self) -> Vec<Mesh> {
        self.slice_volumes().0
    }

    /// Solid and subtracted volumes in plate coordinates: `(positive, negative)`.
    pub fn slice_volumes(&self) -> (Vec<Mesh>, Vec<Mesh>) {
        let mut pos = Vec::new();
        let mut neg = Vec::new();
        for o in &self.objects {
            match o.role {
                ObjectRole::Solid => {
                    pos.push(o.world_mesh());
                    let s = o.support_mesh();
                    if !s.mesh.is_empty() {
                        pos.push(s.mesh);
                    }
                    let m = o.transform.matrix();
                    for c in &o.cutouts {
                        neg.push(c.transformed(&m));
                    }
                }
                ObjectRole::Subtract => neg.push(o.world_mesh()),
            }
        }
        (pos, neg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives;

    #[test]
    fn add_drops_to_plate_and_arranges() {
        let mut s = Scene::default();
        for _ in 0..6 {
            s.add(
                "cube",
                primitives::cuboid(Vec3::new(50.0, 50.0, 50.0), Vec3::splat(20.0)),
            );
        }
        assert!(s.objects.iter().all(|o| o.elevation().abs() < 1e-4));
        let p = PrinterProfile::generic();
        assert!(s.arrange(&p, 2.0));
        assert!(s.out_of_bounds(&p).is_empty());
        // no overlaps
        for a in &s.objects {
            for b in &s.objects {
                if a.id < b.id {
                    let (ba, bb) = (a.world_bounds(), b.world_bounds());
                    let overlap = ba.min.x < bb.max.x - 1e-3
                        && ba.max.x > bb.min.x + 1e-3
                        && ba.min.y < bb.max.y - 1e-3
                        && ba.max.y > bb.min.y + 1e-3;
                    assert!(!overlap);
                }
            }
        }
    }
}
