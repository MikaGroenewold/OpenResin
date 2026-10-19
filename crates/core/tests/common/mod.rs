//! Helpers shared by the integration tests.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use openresin_core::mesh::Mesh;
use openresin_core::slicer::{LayerRaster, RasterOptions};
use openresin_core::Vec3;

/// Raster options without mirroring.
pub fn raster(width: u32, height: u32, size_x: f32, size_y: f32, anti_aliasing: u8) -> RasterOptions {
    RasterOptions {
        width,
        height,
        size_x,
        size_y,
        mirror_x: false,
        mirror_y: false,
        anti_aliasing,
    }
}

/// Pixel area in mm² for `o`.
pub fn pixel_area(o: &RasterOptions) -> f64 {
    let (px, py) = o.pixel_size();
    px as f64 * py as f64
}

/// Lit area of a layer in mm².
pub fn lit_mm2(r: &LayerRaster, o: &RasterOptions) -> f64 {
    r.lit_area_px() * pixel_area(o)
}

/// Pixel value at `(x, y)` of a decoded image.
pub fn px(img: &[u8], width: u32, x: u32, y: u32) -> u8 {
    img[(y * width + x) as usize]
}

/// Temporary file that is deleted when dropped. Names are unique per process
/// and call, so tests can run in parallel.
pub struct TempFile(pub PathBuf);

impl TempFile {
    pub fn new(name: &str, ext: &str) -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let file = format!("openresin-core-test-{}-{n}-{nanos}-{name}.{ext}", std::process::id());
        TempFile(std::env::temp_dir().join(file))
    }

    pub fn with_bytes(name: &str, ext: &str, data: &[u8]) -> Self {
        let f = Self::new(name, ext);
        std::fs::write(&f.0, data).unwrap();
        f
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// `|a - b| <= tol` with a readable failure message.
#[track_caller]
pub fn assert_close(a: f64, b: f64, tol: f64, what: &str) {
    assert!((a - b).abs() <= tol, "{what}: {a} vs expected {b} (tol {tol})");
}

/// `|a - b| <= rel * |b|`.
#[track_caller]
pub fn assert_rel(a: f64, b: f64, rel: f64, what: &str) {
    assert!(
        (a - b).abs() <= rel * b.abs(),
        "{what}: {a} vs expected {b} (rel tol {rel})"
    );
}

/// Area of a regular `n`-gon with circumradius `r` (the cross-section of the
/// crate's cylinders).
pub fn ngon_area(n: u32, r: f64) -> f64 {
    n as f64 / 2.0 * r * r * (std::f64::consts::TAU / n as f64).sin()
}

/// Every triangle gets its own three vertices (like an unwelded STL file).
pub fn soup(m: &Mesh) -> Mesh {
    let mut out = Mesh::default();
    for i in 0..m.triangle_count() {
        let base = out.positions.len() as u32;
        out.positions.extend_from_slice(&m.tri(i));
        out.triangles.push([base, base + 1, base + 2]);
    }
    out
}

/// Order independent description of the triangles of `m` (orientation is
/// kept: each triangle is rotated so that its smallest vertex comes first).
pub fn canonical_triangles(m: &Mesh) -> Vec<[[u32; 3]; 3]> {
    let key = |v: Vec3| [v.x.to_bits(), v.y.to_bits(), v.z.to_bits()];
    let mut out: Vec<[[u32; 3]; 3]> = (0..m.triangle_count())
        .map(|i| {
            let t = m.tri(i).map(key);
            let k = (0..3).min_by_key(|&k| t[k]).unwrap();
            [t[k], t[(k + 1) % 3], t[(k + 2) % 3]]
        })
        .collect();
    out.sort_unstable();
    out
}

/// ASCII STL text for `m` (scientific notation, CRLF line endings, tabs).
pub fn ascii_stl(m: &Mesh, name: &str) -> String {
    let mut s = format!("solid {name}\r\n");
    for i in 0..m.triangle_count() {
        let n = m.face_normal(i);
        s += &format!("  facet normal {:e} {:e} {:e}\r\n\t outer loop\r\n", n.x, n.y, n.z);
        for v in m.tri(i) {
            s += &format!("\t\tvertex {:e} {:e} {:e}\r\n", v.x, v.y, v.z);
        }
        s += "\t endloop\r\n  endfacet\r\n";
    }
    s += &format!("endsolid {name}\r\n");
    s
}

/// Area of the triangles of `m` whose vertices all lie within `tol` of the
/// plane `z`.
pub fn area_at_z(m: &Mesh, z: f32, tol: f32) -> f64 {
    (0..m.triangle_count())
        .filter(|&i| m.tri(i).iter().all(|v| (v.z - z).abs() <= tol))
        .map(|i| {
            let [a, b, c] = m.tri(i);
            ((b - a).cross(c - a).length() * 0.5) as f64
        })
        .sum()
}

/// Distance from `p` to the axis aligned box `[min, max]` (0 inside).
pub fn box_distance(p: Vec3, min: Vec3, max: Vec3) -> f32 {
    (min - p).max(p - max).max(Vec3::ZERO).length()
}
