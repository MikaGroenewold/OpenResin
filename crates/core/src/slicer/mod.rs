//! Mesh slicing: plane/triangle intersection and scanline rasterization into
//! run-length encoded greyscale layer images.

mod raster;

pub use raster::{LayerRaster, Run};

use glam::Vec3;
use serde::{Deserialize, Serialize};

use crate::mesh::Mesh;

/// Describes the target LCD and how geometry maps onto it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RasterOptions {
    /// Image width in pixels (LCD X resolution).
    pub width: u32,
    /// Image height in pixels (LCD Y resolution).
    pub height: u32,
    /// Physical display width in mm.
    pub size_x: f32,
    /// Physical display depth in mm.
    pub size_y: f32,
    /// Mirror the image horizontally (most LCD printers expose a mirrored image).
    pub mirror_x: bool,
    /// Mirror the image vertically.
    pub mirror_y: bool,
    /// Anti-aliasing level (1 = off, otherwise number of vertical sub-samples
    /// combined with exact horizontal coverage).
    pub anti_aliasing: u8,
}

impl RasterOptions {
    pub fn pixel_size(&self) -> (f32, f32) {
        (self.size_x / self.width as f32, self.size_y / self.height as f32)
    }

    /// Converts plate coordinates (mm) to image pixel coordinates (float).
    #[inline]
    pub fn to_pixel(&self, x: f32, y: f32) -> (f32, f32) {
        let mut px = (x + self.size_x * 0.5) * (self.width as f32 / self.size_x);
        let mut py = (self.size_y * 0.5 - y) * (self.height as f32 / self.size_y);
        if self.mirror_x {
            px = self.width as f32 - px;
        }
        if self.mirror_y {
            py = self.height as f32 - py;
        }
        (px, py)
    }
}

/// A line segment of a cross-section in pixel space.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Segment {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

const BUCKET_LAYERS: usize = 16;

/// Prepared slicer for a set of meshes. Cheap to share between threads; every
/// layer can be rasterized independently.
pub struct Slicer {
    tris: Vec<[Vec3; 3]>,
    /// Triangle indices per bucket of `BUCKET_LAYERS` layers.
    buckets: Vec<Vec<u32>>,
    /// Negative volumes (hollowing cavities, drain holes, engravings).
    neg_tris: Vec<[Vec3; 3]>,
    neg_buckets: Vec<Vec<u32>>,
    layer_height: f32,
    layer_count: usize,
    opts: RasterOptions,
}

/// Assigns triangles to buckets of `BUCKET_LAYERS` layers.
fn bucketize(tris: &[[Vec3; 3]], layer_count: usize, layer_height: f32) -> Vec<Vec<u32>> {
    let bucket_count = layer_count.div_ceil(BUCKET_LAYERS);
    let mut buckets = vec![Vec::new(); bucket_count];
    let bucket_h = layer_height * BUCKET_LAYERS as f32;
    for (i, t) in tris.iter().enumerate() {
        let zmin = t[0].z.min(t[1].z).min(t[2].z);
        let zmax = t[0].z.max(t[1].z).max(t[2].z);
        if zmax < 0.0 || bucket_count == 0 {
            continue;
        }
        let b0 = ((zmin / bucket_h).floor().max(0.0) as usize).min(bucket_count - 1);
        let b1 = ((zmax / bucket_h).floor().max(0.0) as usize).min(bucket_count - 1);
        for b in &mut buckets[b0..=b1] {
            b.push(i as u32);
        }
    }
    buckets
}

impl Slicer {
    /// `meshes` must already be positioned in plate coordinates (z = 0 is the plate).
    pub fn new(meshes: &[&Mesh], layer_height: f32, opts: RasterOptions) -> Self {
        assert!(layer_height > 0.0, "layer height must be positive");
        let mut tris = Vec::new();
        let mut max_z = 0f32;
        for m in meshes {
            for i in 0..m.triangle_count() {
                let t = m.tri(i);
                max_z = max_z.max(t[0].z.max(t[1].z).max(t[2].z));
                tris.push(t);
            }
        }
        let layer_count = if tris.is_empty() {
            0
        } else {
            ((max_z / layer_height) - 1e-4).ceil().max(0.0) as usize
        };
        let buckets = bucketize(&tris, layer_count, layer_height);
        Self {
            tris,
            buckets,
            neg_tris: Vec::new(),
            neg_buckets: Vec::new(),
            layer_height,
            layer_count,
            opts,
        }
    }

    /// Like [`Slicer::new`], but additionally subtracts `negative` meshes
    /// (hollowing cavities, drain holes, engraved text, ...): a pixel is solid
    /// where the winding of the positive meshes is non-zero and the winding of
    /// the negative meshes is zero. Negative meshes should be closed; their
    /// orientation does not matter. The layer count is determined by the
    /// positive meshes only.
    pub fn with_negatives(positive: &[&Mesh], negative: &[&Mesh], layer_height: f32, opts: RasterOptions) -> Self {
        let mut s = Self::new(positive, layer_height, opts);
        let mut neg = Vec::new();
        for m in negative {
            for i in 0..m.triangle_count() {
                neg.push(m.tri(i));
            }
        }
        s.neg_buckets = bucketize(&neg, s.layer_count, layer_height);
        s.neg_tris = neg;
        s
    }

    /// Whether negative volumes are subtracted.
    pub fn has_negatives(&self) -> bool {
        !self.neg_tris.is_empty()
    }

    pub fn layer_count(&self) -> usize {
        self.layer_count
    }

    pub fn layer_height(&self) -> f32 {
        self.layer_height
    }

    pub fn options(&self) -> &RasterOptions {
        &self.opts
    }

    /// Height of the top of layer `i` above the plate (the Z position reported to the printer).
    pub fn layer_position_z(&self, i: usize) -> f32 {
        (i as f32 + 1.0) * self.layer_height
    }

    /// Height at which layer `i` is sampled (middle of the layer).
    pub fn layer_sample_z(&self, i: usize) -> f32 {
        (i as f32 + 0.5) * self.layer_height
    }

    /// Cross-section segments of layer `i` in pixel coordinates.
    pub(crate) fn segments(&self, i: usize) -> Vec<Segment> {
        self.segments_of(&self.tris, &self.buckets, i)
    }

    fn segments_of(&self, tris: &[[Vec3; 3]], buckets: &[Vec<u32>], i: usize) -> Vec<Segment> {
        let z = self.layer_sample_z(i);
        let Some(bucket) = buckets.get(i / BUCKET_LAYERS) else {
            return Vec::new();
        };
        let mut segs = Vec::with_capacity(bucket.len() / 4 + 16);
        for &ti in bucket {
            let t = &tris[ti as usize];
            if let Some((p, q)) = intersect_triangle(t, z) {
                let (x0, y0) = self.opts.to_pixel(p.x, p.y);
                let (x1, y1) = self.opts.to_pixel(q.x, q.y);
                if y0 != y1 {
                    segs.push(Segment { x0, y0, x1, y1 });
                }
            }
        }
        segs
    }

    /// Rasterizes layer `i`.
    pub fn rasterize(&self, i: usize) -> LayerRaster {
        if self.neg_tris.is_empty() {
            return raster::rasterize(&self.segments(i), &self.opts);
        }
        let neg = self.segments_of(&self.neg_tris, &self.neg_buckets, i);
        raster::rasterize_masked(&self.segments(i), &neg, &self.opts)
    }

    /// Cross-section outline of layer `i` in plate coordinates (unordered segments),
    /// useful for previews.
    pub fn section_mm(&self, i: usize) -> Vec<[glam::Vec2; 2]> {
        let z = self.layer_sample_z(i);
        let Some(bucket) = self.buckets.get(i / BUCKET_LAYERS) else {
            return Vec::new();
        };
        bucket
            .iter()
            .filter_map(|&ti| intersect_triangle(&self.tris[ti as usize], z))
            .map(|(p, q)| [p.truncate(), q.truncate()])
            .collect()
    }

    /// Cross-section outline of the negative volumes at layer `i` in plate
    /// coordinates (unordered segments).
    pub fn negative_section_mm(&self, i: usize) -> Vec<[glam::Vec2; 2]> {
        let z = self.layer_sample_z(i);
        let Some(bucket) = self.neg_buckets.get(i / BUCKET_LAYERS) else {
            return Vec::new();
        };
        bucket
            .iter()
            .filter_map(|&ti| intersect_triangle(&self.neg_tris[ti as usize], z))
            .map(|(p, q)| [p.truncate(), q.truncate()])
            .collect()
    }
}

#[inline]
fn canonical_less(a: Vec3, b: Vec3) -> bool {
    (a.z, a.x, a.y) < (b.z, b.x, b.y)
}

/// Intersects a triangle with the plane `z`. The returned segment is oriented so
/// that the solid (for an outward oriented mesh) lies on its left.
#[inline]
fn intersect_triangle(t: &[Vec3; 3], z: f32) -> Option<(Vec3, Vec3)> {
    let d = [t[0].z - z, t[1].z - z, t[2].z - z];
    let above = [d[0] >= 0.0, d[1] >= 0.0, d[2] >= 0.0];
    let n_above = above.iter().filter(|&&a| a).count();
    if n_above == 0 || n_above == 3 {
        return None;
    }
    // the vertex that is alone on its side of the plane
    let lone = if n_above == 1 {
        above.iter().position(|&a| a).unwrap()
    } else {
        above.iter().position(|&a| !a).unwrap()
    };
    let a = lone;
    let b = (lone + 1) % 3;
    let c = (lone + 2) % 3;
    // Interpolate along each edge in a canonical vertex order so that the two
    // triangles sharing an edge produce bit-identical points. Otherwise tiny
    // rounding differences let a scanline slip between two segments, which
    // shows up as pinholes or streaks in the layer image.
    let lerp = |i: usize, j: usize| {
        let (i, j) = if canonical_less(t[i], t[j]) { (i, j) } else { (j, i) };
        let s = d[i] / (d[i] - d[j]);
        t[i] + (t[j] - t[i]) * s
    };
    let p_ab = lerp(a, b);
    let p_ca = lerp(c, a);
    // For a counter-clockwise (outward) triangle whose lone vertex is above the
    // plane, walking from edge (a,b) to edge (c,a) keeps the solid on the left
    // (direction = Z x normal).
    if above[a] {
        Some((p_ab, p_ca))
    } else {
        Some((p_ca, p_ab))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives;

    fn opts(w: u32, h: u32, sx: f32, sy: f32, aa: u8) -> RasterOptions {
        RasterOptions {
            width: w,
            height: h,
            size_x: sx,
            size_y: sy,
            mirror_x: false,
            mirror_y: false,
            anti_aliasing: aa,
        }
    }

    #[test]
    fn segment_orientation_is_ccw() {
        // A cube sliced in the middle must produce a CCW (positive area) loop.
        let cube = primitives::cuboid(Vec3::new(0.0, 0.0, 1.0), Vec3::splat(2.0));
        let mut area = 0.0f32;
        for i in 0..cube.triangle_count() {
            if let Some((p, q)) = intersect_triangle(&cube.tri(i), 1.0) {
                area += p.x * q.y - q.x * p.y;
            }
        }
        assert!((area * 0.5 - 4.0).abs() < 1e-4, "area {area}");
    }

    #[test]
    fn cube_raster_area_binary_and_aa() {
        // 10mm cube on a 100x100 px, 100x100mm display (1 px = 1 mm).
        let cube = primitives::cuboid(Vec3::new(0.25, 0.0, 5.0), Vec3::new(10.0, 10.0, 10.0));
        for aa in [1u8, 4] {
            let s = Slicer::new(&[&cube], 0.5, opts(100, 100, 100.0, 100.0, aa));
            assert_eq!(s.layer_count(), 20);
            let r = s.rasterize(3);
            assert_eq!(r.pixel_count(), 100 * 100);
            let lit = r.lit_area_px();
            assert!((lit - 100.0).abs() < 1.01, "aa {aa}: lit {lit}");
        }
    }

    #[test]
    fn mirroring_moves_pixels() {
        let cube = primitives::cuboid(Vec3::new(-20.0, 30.0, 5.0), Vec3::new(10.0, 10.0, 10.0));
        let mut o = opts(100, 100, 100.0, 100.0, 1);
        let r = Slicer::new(&[&cube], 0.5, o).rasterize(0);
        let (x0, y0, x1, y1) = r.bbox.unwrap();
        // left/back quadrant of the image
        assert_eq!((x0, y0, x1, y1), (25, 15, 35, 25));
        o.mirror_x = true;
        let r = Slicer::new(&[&cube], 0.5, o).rasterize(0);
        assert_eq!(r.bbox.unwrap(), (65, 15, 75, 25));
    }

    #[test]
    fn overlapping_meshes_union() {
        let a = primitives::cuboid(Vec3::new(0.0, 0.0, 5.0), Vec3::new(10.0, 10.0, 10.0));
        let b = primitives::cuboid(Vec3::new(5.0, 0.0, 5.0), Vec3::new(10.0, 10.0, 10.0));
        let s = Slicer::new(&[&a, &b], 1.0, opts(100, 100, 100.0, 100.0, 1));
        let lit = s.rasterize(2).lit_area_px();
        assert!((lit - 150.0).abs() < 1.0, "{lit}");
    }

    #[test]
    fn sphere_slices_have_no_pinholes() {
        let sphere = primitives::sphere(Vec3::new(1.234, -2.345, 20.0), 19.7, 97, 61);
        for aa in [1u8, 4] {
            let s = Slicer::new(&[&sphere], 0.05, opts(1600, 1000, 192.0, 120.0, aa));
            for layer in (0..s.layer_count()).step_by(7) {
                let r = s.rasterize(layer);
                r.for_each_row(|y, row| {
                    // a convex cross-section must be a single run of non-black pixels per row
                    let mut runs = 0;
                    let mut inside = false;
                    for &v in row {
                        let lit = v > 0;
                        if lit && !inside {
                            runs += 1;
                        }
                        inside = lit;
                    }
                    assert!(runs <= 1, "aa {aa} layer {layer} row {y}: {runs} runs");
                    // and fully lit pixels must be contiguous as well
                    if aa == 1 {
                        let first = row.iter().position(|&v| v == 255);
                        let last = row.iter().rposition(|&v| v == 255);
                        if let (Some(a), Some(b)) = (first, last) {
                            assert!(row[a..=b].iter().all(|&v| v == 255), "hole in layer {layer} row {y}");
                        }
                    }
                });
            }
        }
    }

    #[test]
    fn negative_volumes_subtract_with_exact_coverage() {
        let outer = primitives::cuboid(Vec3::new(0.0, 0.0, 5.0), Vec3::new(20.0, 20.0, 10.0));
        // cavity at fractional pixel positions, oriented outward (the slicer does not care)
        let cavity = primitives::cuboid(Vec3::new(0.3, -0.45, 5.0), Vec3::new(10.0, 9.5, 6.0));
        let mut inverted = cavity.clone();
        inverted.flip();
        for aa in [1u8, 4] {
            for neg in [&cavity, &inverted] {
                let s = Slicer::with_negatives(&[&outer], &[neg], 1.0, opts(100, 100, 100.0, 100.0, aa));
                assert!(s.has_negatives());
                assert_eq!(s.layer_count(), 10);
                let solid = s.rasterize(0).lit_area_px();
                assert!((solid - 400.0).abs() < 1e-3, "aa {aa}: {solid}");
                let hollow = s.rasterize(4).lit_area_px();
                let expect = 400.0 - 10.0 * 9.5;
                let tol = if aa == 1 { 10.5 } else { 0.6 };
                assert!((hollow - expect).abs() < tol, "aa {aa}: {hollow} vs {expect}");
                if aa == 4 {
                    // exact horizontal coverage: the 0.3 px offset shows up as partial pixels
                    let px = s.rasterize(4).to_gray();
                    assert!(px.iter().any(|&v| v > 0 && v < 255));
                }
            }
        }
    }

    #[test]
    fn drain_hole_through_wall() {
        let outer = primitives::cuboid(Vec3::new(0.0, 0.0, 5.0), Vec3::new(20.0, 20.0, 10.0));
        let cavity = primitives::cuboid(Vec3::new(0.0, 0.0, 5.0), Vec3::new(10.0, 10.0, 6.0));
        // horizontal hole along +x at z = 4.75 (layer 9 at 0.5 mm), from outside into the cavity
        let hole = primitives::frustum(Vec3::new(13.0, 0.0, 4.75), Vec3::new(3.0, 0.0, 4.75), 1.0, 1.0, 32);
        for aa in [1u8, 4] {
            let s = Slicer::with_negatives(&[&outer], &[&cavity, &hole], 0.5, opts(100, 100, 100.0, 100.0, aa));
            let lit = s.rasterize(9).lit_area_px();
            // the part of the hole outside the model must not add material
            let expect = 400.0 - 100.0 - 5.0 * 2.0;
            assert!((lit - expect).abs() < 1.01, "aa {aa}: {lit} vs {expect}");
            // a layer below the hole is unaffected
            assert!((s.rasterize(5).lit_area_px() - 300.0).abs() < 1.01);
            let outline = s.negative_section_mm(9);
            assert!(!outline.is_empty());
        }
    }

    #[test]
    fn without_negatives_identical_to_new() {
        let sphere = primitives::sphere(Vec3::new(1.234, -2.345, 10.0), 9.7, 41, 23);
        for aa in [1u8, 4] {
            let o = opts(400, 300, 96.0, 72.0, aa);
            let a = Slicer::new(&[&sphere], 0.1, o);
            let b = Slicer::with_negatives(&[&sphere], &[], 0.1, o);
            // a negative mesh far away from the model changes nothing either
            let far = primitives::cuboid(Vec3::new(40.0, 30.0, 5.0), Vec3::splat(1.0));
            let c = Slicer::with_negatives(&[&sphere], &[&far], 0.1, o);
            assert!(!b.has_negatives());
            for layer in (0..a.layer_count()).step_by(13) {
                let ra = a.rasterize(layer);
                assert_eq!(ra, b.rasterize(layer));
                assert_eq!(ra, c.rasterize(layer));
            }
        }
        // only negatives: nothing to print
        let neg = primitives::cuboid(Vec3::new(0.0, 0.0, 5.0), Vec3::splat(4.0));
        let s = Slicer::with_negatives(&[], &[&neg], 0.1, opts(100, 100, 100.0, 100.0, 1));
        assert_eq!(s.layer_count(), 0);
    }

    #[test]
    fn hollow_box_has_hole() {
        // outer cube minus inverted inner cube = hollow shell
        let outer = primitives::cuboid(Vec3::new(0.0, 0.0, 5.0), Vec3::new(20.0, 20.0, 10.0));
        let mut inner = primitives::cuboid(Vec3::new(0.0, 0.0, 5.0), Vec3::new(10.0, 10.0, 20.0));
        inner.flip();
        let s = Slicer::new(&[&outer, &inner], 1.0, opts(100, 100, 100.0, 100.0, 1));
        let lit = s.rasterize(2).lit_area_px();
        assert!((lit - 300.0).abs() < 1.0, "{lit}");
    }
}
