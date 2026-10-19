//! Support structures: contact point detection and geometry generation.

mod auto;
mod index;

pub use auto::auto_points;
pub use index::{VerticalHit, VerticalRayIndex};

use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};

use crate::geometry::{convex_hull, offset_convex};
use crate::mesh::Mesh;
use crate::primitives::{extrude_convex, frustum, sphere};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SupportSize {
    Light,
    #[default]
    Medium,
    Heavy,
}

/// A support contact point on the model surface.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SupportPoint {
    pub position: Vec3,
    /// Outward surface normal at the contact.
    pub normal: Vec3,
    #[serde(default)]
    pub size: SupportSize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SupportSettings {
    /// Distance between the plate and the lowest model point when supported (mm).
    pub lift_height: f32,
    /// Approximate distance between support contacts on overhangs (mm).
    pub spacing: f32,
    /// Surfaces flatter than this angle (degrees from horizontal) get supported.
    pub overhang_angle: f32,
    /// Length of the conical tip (mm).
    pub tip_length: f32,
    /// Contact diameters for light/medium/heavy supports (mm).
    pub tip_diameter: [f32; 3],
    /// Shaft diameters for light/medium/heavy supports (mm).
    pub shaft_diameter: [f32; 3],
    /// Diameter of the conical foot on the raft/plate (mm).
    pub base_diameter: f32,
    /// Height of the conical foot (mm).
    pub base_height: f32,
    pub raft: bool,
    pub raft_thickness: f32,
    /// Raft extends this far beyond the outermost support feet (mm).
    pub raft_margin: f32,
    /// Allow supports that start on the model itself.
    pub model_contacts: bool,
    /// Detect and support islands (unsupported areas appearing mid-print).
    pub detect_islands: bool,
    /// Layer step used for island detection (mm).
    pub island_layer_step: f32,
    pub default_size: SupportSize,
    /// Polygon segments for round support parts.
    pub segments: u32,
    /// Connect neighbouring pillars that stand on the plate/raft with diagonal
    /// braces (zig-zag, about 45 degrees).
    pub cross_bracing: bool,
    /// Maximum horizontal distance between braced pillars (mm).
    pub bracing_max_distance: f32,
    /// Pillars shorter than this are not braced (mm).
    pub bracing_min_height: f32,
    /// Brace diameter (mm).
    pub bracing_diameter: f32,
    /// Maximum number of braced neighbours per pillar.
    pub bracing_neighbors: u32,
    /// Minimum distance between a brace and the model surface (mm).
    pub bracing_clearance: f32,
}

impl Default for SupportSettings {
    fn default() -> Self {
        Self {
            lift_height: 5.0,
            spacing: 3.0,
            overhang_angle: 45.0,
            tip_length: 2.5,
            tip_diameter: [0.3, 0.45, 0.65],
            shaft_diameter: [0.8, 1.1, 1.6],
            base_diameter: 3.5,
            base_height: 0.8,
            raft: true,
            raft_thickness: 1.0,
            raft_margin: 1.5,
            model_contacts: true,
            detect_islands: true,
            island_layer_step: 0.1,
            default_size: SupportSize::Medium,
            segments: 8,
            cross_bracing: false,
            bracing_max_distance: 10.0,
            bracing_min_height: 8.0,
            bracing_diameter: 0.8,
            bracing_neighbors: 3,
            bracing_clearance: 0.5,
        }
    }
}

impl SupportSettings {
    fn radii(&self, size: SupportSize) -> (f32, f32) {
        let i = match size {
            SupportSize::Light => 0,
            SupportSize::Medium => 1,
            SupportSize::Heavy => 2,
        };
        (self.tip_diameter[i] * 0.5, self.shaft_diameter[i] * 0.5)
    }

    pub fn raft_top(&self) -> f32 {
        if self.raft {
            self.raft_thickness
        } else {
            0.0
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SupportGeometry {
    /// All support parts including the raft, in plate coordinates.
    pub mesh: Mesh,
    /// Indices of support points that could not be built (blocked by the model).
    pub failed: Vec<usize>,
    /// Number of cross braces that were added.
    pub braces: usize,
}

/// A support shaft standing on the plate/raft.
#[derive(Debug, Clone, Copy)]
struct Pillar {
    xy: Vec2,
    /// lowest height usable for braces (top of the foot)
    z0: f32,
    /// top of the shaft (start of the contact tip)
    z1: f32,
}

/// Builds support geometry for `points` (plate coordinates) under `model`.
pub fn build(model: &Mesh, points: &[SupportPoint], s: &SupportSettings) -> SupportGeometry {
    let idx = VerticalRayIndex::new(model);
    let raft_top = s.raft_top();
    let seg = s.segments.max(4);
    let mut mesh = Mesh::default();
    let mut feet: Vec<Vec2> = Vec::new();
    let mut failed = Vec::new();
    let mut pillars: Vec<Pillar> = Vec::new();

    for (pi, p) in points.iter().enumerate() {
        let (tip_r, shaft_r) = s.radii(p.size);
        let n = p.normal.normalize_or(-Vec3::Z);
        // tip direction: between the surface normal and straight down
        let mut dir = (n + -Vec3::Z).normalize_or(-Vec3::Z);
        if dir.z > -0.5 {
            dir = (n * 0.3 + -Vec3::Z).normalize_or(-Vec3::Z);
        }
        let contact = p.position - n * (tip_r * 0.5);
        let mut tip_end = p.position + dir * s.tip_length;
        if idx.contains(tip_end) {
            tip_end = p.position - Vec3::Z * s.tip_length;
        }
        // something below the tip?
        let below = idx
            .hits(tip_end.x, tip_end.y)
            .into_iter()
            .rfind(|h| h.z < tip_end.z - 1e-3);
        let mut parts: Vec<Mesh> = Vec::with_capacity(4);
        match below {
            Some(h) => {
                let gap = tip_end.z - h.z;
                if !s.model_contacts || h.normal.z <= 0.0 || gap < s.tip_length * 0.5 {
                    failed.push(pi);
                    continue;
                }
                let foot_tip = Vec3::new(tip_end.x, tip_end.y, h.z - tip_r * 0.5);
                let foot_top_z = (h.z + s.tip_length).min(tip_end.z);
                let foot_top = Vec3::new(tip_end.x, tip_end.y, foot_top_z);
                parts.push(frustum(foot_tip, foot_top, tip_r, shaft_r, seg));
                if tip_end.z > foot_top_z + 1e-3 {
                    parts.push(frustum(foot_top, tip_end, shaft_r, shaft_r, seg));
                }
            }
            None => {
                let foot_top = raft_top + s.base_height;
                if tip_end.z < foot_top + shaft_r {
                    // model too close to the plate for a full support; go straight down
                    tip_end = Vec3::new(p.position.x, p.position.y, (foot_top + shaft_r).min(p.position.z));
                }
                let base_bottom = Vec3::new(tip_end.x, tip_end.y, raft_top - 0.05);
                let base_top = Vec3::new(tip_end.x, tip_end.y, foot_top);
                parts.push(frustum(base_bottom, base_top, s.base_diameter * 0.5, shaft_r, seg * 2));
                if tip_end.z > base_bottom.z + 1e-3 {
                    parts.push(frustum(base_bottom, tip_end, shaft_r, shaft_r, seg));
                }
                feet.push(tip_end.truncate());
                pillars.push(Pillar {
                    xy: tip_end.truncate(),
                    z0: foot_top,
                    z1: tip_end.z,
                });
            }
        }
        parts.push(sphere(tip_end, shaft_r, seg, (seg / 2).max(3)));
        if (tip_end - contact).length() > 1e-3 {
            parts.push(frustum(contact, tip_end, tip_r, shaft_r, seg));
        }
        for part in &parts {
            mesh.append(part);
        }
    }

    let braces = if s.cross_bracing {
        add_braces(model, &pillars, s, &mut mesh)
    } else {
        0
    };

    if s.raft && !feet.is_empty() {
        let hull = convex_hull(&feet);
        let r = s.base_diameter * 0.5 + s.raft_margin;
        let outline = if hull.len() >= 3 {
            offset_convex(&hull, r)
        } else {
            // one or two feet: use a box around them
            let min = feet.iter().fold(Vec2::splat(f32::MAX), |a, b| a.min(*b)) - Vec2::splat(r);
            let max = feet.iter().fold(Vec2::splat(f32::MIN), |a, b| a.max(*b)) + Vec2::splat(r);
            vec![min, Vec2::new(max.x, min.y), max, Vec2::new(min.x, max.y)]
        };
        let chamfer = (s.raft_thickness * 0.8).min(s.raft_margin);
        mesh.append(&extrude_convex(&outline, 0.0, s.raft_thickness, chamfer));
    }

    SupportGeometry { mesh, failed, braces }
}

/// Adds zig-zag braces between neighbouring pillars. Braces that would come
/// closer than `bracing_clearance` to the model are skipped. Returns the number
/// of braces added.
fn add_braces(model: &Mesh, pillars: &[Pillar], s: &SupportSettings, mesh: &mut Mesh) -> usize {
    let min_h = s.bracing_min_height.max(0.0);
    let max_d = s.bracing_max_distance.max(0.0);
    let r = (s.bracing_diameter * 0.5).max(0.05);
    let tall: Vec<usize> = (0..pillars.len())
        .filter(|&i| pillars[i].z1 - pillars[i].z0 >= min_h)
        .collect();
    if tall.len() < 2 || max_d <= 0.0 {
        return 0;
    }
    // neighbour pairs: the k nearest pillars within reach
    let k = s.bracing_neighbors.max(1) as usize;
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    for &i in &tall {
        let mut near: Vec<(f32, usize)> = tall
            .iter()
            .filter(|&&j| j != i)
            .map(|&j| ((pillars[j].xy - pillars[i].xy).length(), j))
            .filter(|(d, _)| *d <= max_d && *d > 1e-3)
            .collect();
        near.sort_by(|a, b| a.0.total_cmp(&b.0));
        for &(_, j) in near.iter().take(k) {
            pairs.push((i.min(j), i.max(j)));
        }
    }
    pairs.sort_unstable();
    pairs.dedup();
    let bvh = crate::bvh::Bvh::new(model);
    let clearance = r + s.bracing_clearance.max(0.0);
    let seg = s.segments.clamp(4, 8);
    let mut count = 0;
    for (i, j) in pairs {
        let (a, b) = (pillars[i], pillars[j]);
        let d = (b.xy - a.xy).length();
        let rise = d.max(2.0);
        let lo = a.z0.max(b.z0) + r;
        let hi = a.z1.min(b.z1) - (1.0f32).max(2.0 * r);
        let mut z = lo;
        let mut forward = true;
        while z + rise <= hi {
            let (p, q) = if forward {
                (a.xy.extend(z), b.xy.extend(z + rise))
            } else {
                (b.xy.extend(z), a.xy.extend(z + rise))
            };
            if !bvh.segment_near(p, q, clearance) {
                mesh.append(&frustum(p, q, r, r, seg));
                count += 1;
            }
            z += rise;
            forward = !forward;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::cuboid;

    #[test]
    fn supports_reach_plate_and_raft_exists() {
        let s = SupportSettings::default();
        let model = cuboid(Vec3::new(0.0, 0.0, s.lift_height + 5.0), Vec3::new(10.0, 10.0, 10.0));
        let pts = auto_points(&model, &s);
        assert!(pts.len() >= 4, "only {} points", pts.len());
        let g = build(&model, &pts, &s);
        assert!(g.failed.is_empty());
        let b = g.mesh.bounds();
        assert!(b.min.z <= 0.0 + 1e-4);
        assert!(b.max.z >= s.lift_height - 0.5);
        assert!(g.mesh.signed_volume() > 0.0);
    }

    #[test]
    fn cross_bracing_between_tall_pillars() {
        let s = SupportSettings {
            lift_height: 20.0,
            cross_bracing: true,
            ..Default::default()
        };
        let model = cuboid(Vec3::new(0.0, 0.0, s.lift_height + 5.0), Vec3::new(12.0, 12.0, 10.0));
        let pts = auto_points(&model, &s);
        let plain = build(
            &model,
            &pts,
            &SupportSettings {
                cross_bracing: false,
                ..s.clone()
            },
        );
        let g = build(&model, &pts, &s);
        assert_eq!(plain.braces, 0);
        assert!(g.braces > 4, "braces {}", g.braces);
        assert!(g.mesh.triangle_count() > plain.mesh.triangle_count());
        assert_eq!(g.mesh.open_edge_count(), 0);
        assert!(g.mesh.signed_volume() > plain.mesh.signed_volume());
        // short pillars are not braced
        let low = SupportSettings {
            lift_height: 4.0,
            ..s.clone()
        };
        let m2 = cuboid(Vec3::new(0.0, 0.0, low.lift_height + 5.0), Vec3::new(12.0, 12.0, 10.0));
        let g2 = build(&m2, &auto_points(&m2, &low), &low);
        assert_eq!(g2.braces, 0);
        // serde: old settings without bracing fields still load
        let json = serde_json::to_string(&SupportSettings::default()).unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
        v.as_object_mut().unwrap().remove("cross_bracing");
        v.as_object_mut().unwrap().remove("bracing_diameter");
        let back: SupportSettings = serde_json::from_value(v).unwrap();
        assert_eq!(back, SupportSettings::default());
    }

    #[test]
    fn braces_avoid_the_model() {
        let s = SupportSettings {
            lift_height: 20.0,
            cross_bracing: true,
            bracing_max_distance: 20.0,
            ..Default::default()
        };
        // a slab on top and a column hanging down between the pillars
        let mut model = cuboid(Vec3::new(0.0, 0.0, 25.0), Vec3::new(24.0, 6.0, 4.0));
        model.append(&cuboid(Vec3::new(0.0, 0.0, 15.0), Vec3::new(2.0, 2.0, 18.0)));
        let pts: Vec<SupportPoint> = [-7.0f32, 7.0]
            .iter()
            .map(|&x| SupportPoint {
                position: Vec3::new(x, 0.0, 23.0),
                normal: -Vec3::Z,
                size: SupportSize::Medium,
            })
            .collect();
        let g = build(&model, &pts, &s);
        assert!(g.failed.is_empty());
        // a brace from x=-7 to x=7 would cross the hanging column (x in -1..1, z 6..24)
        let bvh = crate::bvh::Bvh::new(&model);
        let mut close = 0;
        for p in &g.mesh.positions {
            if p.z > 6.0 && p.z < 20.0 && p.x.abs() < 3.0 && bvh.closest_point(*p, 0.3).is_some() {
                close += 1;
            }
        }
        assert_eq!(close, 0);
        assert_eq!(g.braces, 0, "the only possible brace crosses the column");
        // without the hanging column the brace is built
        let slab = cuboid(Vec3::new(0.0, 0.0, 25.0), Vec3::new(24.0, 6.0, 4.0));
        assert_eq!(build(&slab, &pts, &s).braces, 1);
    }

    #[test]
    fn overhang_supported_from_model_or_plate() {
        let s = SupportSettings::default();
        // a "T": column with a wide flat top overhanging
        let mut model = cuboid(Vec3::new(0.0, 0.0, s.lift_height + 10.0), Vec3::new(4.0, 4.0, 20.0));
        model.append(&cuboid(
            Vec3::new(0.0, 0.0, s.lift_height + 21.0),
            Vec3::new(24.0, 24.0, 2.0),
        ));
        let pts = auto_points(&model, &s);
        let high = pts.iter().filter(|p| p.position.z > s.lift_height + 19.0).count();
        assert!(high > 10, "overhang points: {high}");
        let g = build(&model, &pts, &s);
        assert!(!g.mesh.is_empty());
    }
}
