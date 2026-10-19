//! Automatic support point detection.

use glam::{Mat4, Vec3};
use rayon::prelude::*;

use super::{SupportPoint, SupportSettings, VerticalRayIndex};
use crate::mesh::Mesh;
use crate::slicer::{RasterOptions, Slicer};

/// Finds support contact points for `mesh` (plate coordinates, already lifted).
///
/// Two strategies are combined:
/// 1. a hexagonal grid of vertical rays; every downward facing surface hit that
///    is flatter than the overhang angle gets a contact, and
/// 2. coarse slicing to find islands – regions that appear in a layer without
///    any overlap with the previous layer (local minima, hanging tips).
pub fn auto_points(mesh: &Mesh, s: &SupportSettings) -> Vec<SupportPoint> {
    if mesh.is_empty() {
        return Vec::new();
    }
    let idx = VerticalRayIndex::new(mesh);
    let b = mesh.bounds();
    let cos_t = s.overhang_angle.to_radians().cos();
    let sp = s.spacing.max(0.3);
    let mut points: Vec<SupportPoint> = Vec::new();

    // 1) overhang sampling on a hex grid
    let mut rows = Vec::new();
    let mut y = b.min.y + sp * 0.5;
    let mut row = 0;
    while y < b.max.y {
        rows.push((y, row));
        y += sp * 0.866;
        row += 1;
    }
    if rows.is_empty() {
        rows.push((b.center().y, 0));
    }
    let grid: Vec<SupportPoint> = rows
        .par_iter()
        .flat_map_iter(|&(y, row)| {
            let mut out = Vec::new();
            let mut x = b.min.x + sp * if row % 2 == 1 { 0.75 } else { 0.25 };
            while x < b.max.x {
                let hits = idx.hits(x, y);
                // Downward faces with solid right below them are internal faces of
                // overlapping shells (nonzero winding, as the slicer sees it); they
                // need no support. Columns with unbalanced winding (open or
                // inconsistently oriented meshes) are not filtered.
                let balanced = hits.iter().map(|h| if h.normal.z < 0.0 { 1 } else { -1 }).sum::<i32>() == 0;
                let mut winding = 0i32;
                for h in &hits {
                    if -h.normal.z > cos_t && (winding == 0 || !balanced) {
                        out.push(SupportPoint {
                            position: Vec3::new(x, y, h.z),
                            normal: h.normal,
                            size: s.default_size,
                        });
                    }
                    winding += if h.normal.z < 0.0 { 1 } else { -1 };
                }
                x += sp;
            }
            out
        })
        .collect();
    points.extend(grid);

    // 2) island detection
    if s.detect_islands {
        let islands = find_islands(mesh, &idx, s);
        for p in islands {
            let near = points.iter().any(|q| {
                (q.position.truncate() - p.position.truncate()).length() < sp * 0.5
                    && (q.position.z - p.position.z).abs() < s.island_layer_step * 4.0
            });
            if !near {
                points.push(p);
            }
        }
    }
    points
}

fn find_islands(mesh: &Mesh, idx: &VerticalRayIndex, s: &SupportSettings) -> Vec<SupportPoint> {
    let b = mesh.bounds();
    let size = b.size();
    // ~0.2 mm pixels, but never more than ~4 MPx per layer
    let res = (0.2f32).max(((size.x * size.y) / 4.0e6).sqrt());
    let w = ((size.x / res).ceil() as u32 + 4).max(4);
    let h = ((size.y / res).ceil() as u32 + 4).max(4);
    let c = b.center();
    let centred = mesh.transformed(&Mat4::from_translation(Vec3::new(-c.x, -c.y, 0.0)));
    let opts = RasterOptions {
        width: w,
        height: h,
        size_x: w as f32 * res,
        size_y: h as f32 * res,
        mirror_x: false,
        mirror_y: false,
        anti_aliasing: 1,
    };
    let dz = s.island_layer_step.max(0.02);
    let slicer = Slicer::new(&[&centred], dz, opts);
    let to_mm = |px: u32, py: u32| {
        (
            (px as f32 + 0.5) * res - opts.size_x * 0.5 + c.x,
            opts.size_y * 0.5 - (py as f32 + 0.5) * res + c.y,
        )
    };

    let n = slicer.layer_count();
    let mut verts: Vec<Vec3> = mesh.positions.clone();
    verts.sort_by(|a, b| a.z.total_cmp(&b.z));
    let mut prev = vec![false; (w * h) as usize];
    let mut out = Vec::new();
    let chunk = 32usize;
    let mut start = 0usize;
    let mut labels = vec![0u32; (w * h) as usize];
    let mut stack = Vec::new();
    while start < n {
        let end = (start + chunk).min(n);
        let masks: Vec<Vec<bool>> = (start..end)
            .into_par_iter()
            .map(|i| slicer.rasterize(i).to_gray().into_iter().map(|v| v > 0).collect())
            .collect();
        for (k, mask) in masks.into_iter().enumerate() {
            let layer = start + k;
            let z = slicer.layer_sample_z(layer);
            labels.fill(0);
            let mut label = 0u32;
            for start_px in 0..mask.len() {
                if !mask[start_px] || labels[start_px] != 0 {
                    continue;
                }
                label += 1;
                // flood fill, collecting stats
                let mut supported = false;
                let (mut sx, mut sy, mut cnt) = (0f64, 0f64, 0u64);
                let mut pixels = Vec::new();
                stack.clear();
                stack.push(start_px);
                labels[start_px] = label;
                while let Some(p) = stack.pop() {
                    let px = (p as u32) % w;
                    let py = (p as u32) / w;
                    pixels.push(p);
                    sx += px as f64;
                    sy += py as f64;
                    cnt += 1;
                    if !supported {
                        // overlap with previous layer, tolerant by one pixel
                        'n: for dy in -1i32..=1 {
                            for dx in -1i32..=1 {
                                let qx = px as i32 + dx;
                                let qy = py as i32 + dy;
                                if qx >= 0
                                    && qy >= 0
                                    && (qx as u32) < w
                                    && (qy as u32) < h
                                    && prev[(qy as u32 * w + qx as u32) as usize]
                                {
                                    supported = true;
                                    break 'n;
                                }
                            }
                        }
                    }
                    let neighbours = [
                        (px > 0).then(|| p - 1),
                        (px + 1 < w).then(|| p + 1),
                        (py > 0).then(|| p - w as usize),
                        (py + 1 < h).then(|| p + w as usize),
                    ];
                    for q in neighbours.into_iter().flatten() {
                        if mask[q] && labels[q] == 0 {
                            labels[q] = label;
                            stack.push(q);
                        }
                    }
                }
                if supported || cnt == 0 {
                    continue;
                }
                // island: support at the pixel closest to the centroid
                let (cx, cy) = (sx / cnt as f64, sy / cnt as f64);
                let best = pixels
                    .iter()
                    .min_by(|&&a, &&b| {
                        let da = ((a as u32 % w) as f64 - cx).powi(2) + ((a as u32 / w) as f64 - cy).powi(2);
                        let db = ((b as u32 % w) as f64 - cx).powi(2) + ((b as u32 / w) as f64 - cy).powi(2);
                        da.total_cmp(&db)
                    })
                    .copied()
                    .unwrap();
                // Prefer the true local minimum: the lowest mesh vertex inside the
                // island's footprint close to this layer (exact for tips/cones).
                let (bx0, by0, bx1, by1) = pixels.iter().fold((u32::MAX, u32::MAX, 0, 0), |a, &p| {
                    let (px, py) = (p as u32 % w, p as u32 / w);
                    (a.0.min(px), a.1.min(py), a.2.max(px), a.3.max(py))
                });
                let (mx0, my1) = to_mm(bx0, by0);
                let (mx1, my0) = to_mm(bx1, by1);
                let lo = verts.partition_point(|v| v.z < z - (dz * 2.0).max(1.0));
                let lowest = verts[lo..]
                    .iter()
                    .take_while(|v| v.z <= z + dz)
                    .filter(|v| v.x >= mx0 - res && v.x <= mx1 + res && v.y >= my0 - res && v.y <= my1 + res)
                    .min_by(|a, b| a.z.total_cmp(&b.z));
                if let Some(v) = lowest {
                    out.push(SupportPoint {
                        position: *v,
                        normal: -Vec3::Z,
                        size: s.default_size,
                    });
                    continue;
                }
                let (x, y) = to_mm(best as u32 % w, best as u32 / w);
                let hit = idx
                    .hits(x, y)
                    .into_iter()
                    .filter(|h| h.normal.z < 0.0 && h.z <= z + dz)
                    .min_by(|a, b| (z - a.z).abs().total_cmp(&(z - b.z).abs()));
                let (pz, normal) = match hit {
                    Some(h) => (h.z, h.normal),
                    None => (z - dz * 0.5, -Vec3::Z),
                };
                out.push(SupportPoint {
                    position: Vec3::new(x, y, pz),
                    normal,
                    size: s.default_size,
                });
            }
            prev = mask;
        }
        start = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::{cuboid, frustum};

    #[test]
    fn hanging_cone_tip_is_detected_as_island() {
        let s = SupportSettings {
            spacing: 50.0, // grid too coarse to hit the tip
            ..Default::default()
        };
        // inverted cone: tip at z=10, widening upwards (steep, not an overhang by angle)
        let m = frustum(Vec3::new(3.0, 2.0, 10.0), Vec3::new(3.0, 2.0, 20.0), 0.0, 2.0, 24);
        let pts = auto_points(&m, &s);
        assert!(!pts.is_empty());
        let p = pts.iter().min_by(|a, b| a.position.z.total_cmp(&b.position.z)).unwrap();
        assert!((p.position.z - 10.0).abs() < 0.5, "{:?}", p.position);
        assert!((p.position.x - 3.0).abs() < 0.5 && (p.position.y - 2.0).abs() < 0.5);
        let _ = cuboid(Vec3::ZERO, Vec3::ONE);
    }
}
