//! Shared helpers for the `openresin-formats` integration tests.
//!
//! Included by the other test files with `#[path = "formats_helpers.rs"] mod helpers;`.
//! Cargo also builds it as its own (empty) test target.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

use openresin_core::estimate::PrintEstimate;
use openresin_core::primitives::{cuboid, frustum};
use openresin_core::profile::{builtin_printers, LayerParams, MirrorMode, Motion, PrintSettings, PrinterProfile};
use openresin_core::slicer::{LayerRaster, Run, Slicer};
use openresin_core::{Mesh, Vec3};
use openresin_formats::anycubic::{self, LayerCodec};
use openresin_formats::cxdlpv4::{self, V4Codec};
use openresin_formats::preview::render_meshes;
use openresin_formats::{slice_to_file, ExportJob, ExportProgress, ExportResult, Format, Thumbnail};

// ---------------------------------------------------------------- temp dirs

/// A unique directory under the system temp dir, removed on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "openresin-fmt-{tag}-{}-{}-{nanos}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    /// Names of all entries in the directory.
    pub fn entries(&self) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(&self.0)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------- PRNG

/// Small deterministic PRNG (xorshift64*), so tests need no extra dependency.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }
    pub fn u8(&mut self) -> u8 {
        self.next_u64() as u8
    }
}

// ---------------------------------------------------------------- settings and scenes

/// Print settings where every field has a distinct, non-default value so that
/// swapped or dropped fields show up in the metadata checks.
pub fn test_settings() -> PrintSettings {
    PrintSettings {
        name: "Test resin".into(),
        layer_height: 0.05,
        bottom_layers: 3,
        transition_layers: 2,
        bottom: LayerParams {
            exposure: 24.0,
            light_off_delay: 1.5,
            wait_before_lift: 0.5,
            wait_after_lift: 1.0,
            wait_after_retract: 2.0,
            lift: Motion {
                distance: 5.0,
                speed: 48.0,
                distance2: 2.0,
                speed2: 150.0,
            },
            retract: Motion {
                distance: 5.0,
                speed: 120.0,
                distance2: 2.0,
                speed2: 60.0,
            },
            light_pwm: 230,
        },
        normal: LayerParams {
            exposure: 2.2,
            light_off_delay: 1.0,
            wait_before_lift: 0.25,
            wait_after_lift: 0.75,
            wait_after_retract: 1.25,
            lift: Motion {
                distance: 4.0,
                speed: 90.0,
                distance2: 1.5,
                speed2: 180.0,
            },
            retract: Motion {
                distance: 4.0,
                speed: 210.0,
                distance2: 1.5,
                speed2: 72.0,
            },
            light_pwm: 200,
        },
        wait_mode: true,
        anti_aliasing: 4,
        resin_density: 1.12,
        resin_price_per_liter: 42.0,
        currency: "€".into(),
    }
}

/// Test scene in plate coordinates (fits on every built-in printer, the
/// smallest plate is about 55 x 57 mm).
pub struct Scene {
    pub meshes: Vec<Mesh>,
    pub negatives: Vec<Mesh>,
}

/// Number of layers of [`Scene::standard`] at 0.05 mm.
pub const STANDARD_LAYERS: u32 = 60;

impl Scene {
    /// * a 16 x 12 x 2.5 mm cuboid at x -16..0 with a 2.5 mm radius through-hole
    ///   (a negative volume) at (-8, 0);
    /// * a cone frustum (r 4 -> 3 mm, 3 mm high) at (9, 4), so cross sections
    ///   shrink from layer to layer and edges are anti-aliased;
    /// * an overhanging bar x 6..12, y -9..5, z 1.8..3.0 that partly rests on
    ///   the frustum (overlapping meshes are united) and partly floats.
    pub fn standard() -> Self {
        let base = cuboid(Vec3::new(-8.0, 0.0, 1.25), Vec3::new(16.0, 12.0, 2.5));
        let cone = frustum(Vec3::new(9.0, 4.0, 0.0), Vec3::new(9.0, 4.0, 3.0), 4.0, 3.0, 48);
        let bar = cuboid(Vec3::new(9.0, -2.0, 2.4), Vec3::new(6.0, 14.0, 1.2));
        let hole = frustum(Vec3::new(-8.0, 0.0, -1.0), Vec3::new(-8.0, 0.0, 4.0), 2.5, 2.5, 32);
        Scene {
            meshes: vec![base, cone, bar],
            negatives: vec![hole],
        }
    }

    pub fn mesh_refs(&self) -> Vec<&Mesh> {
        self.meshes.iter().collect()
    }

    pub fn negative_refs(&self) -> Vec<&Mesh> {
        self.negatives.iter().collect()
    }

    /// Reference slicer for the scene, built directly with the core slicer.
    pub fn slicer(&self, printer: &PrinterProfile, settings: &PrintSettings) -> Slicer {
        let opts = printer.raster_options(settings.anti_aliasing);
        if self.negatives.is_empty() {
            Slicer::new(&self.mesh_refs(), settings.layer_height, opts)
        } else {
            Slicer::with_negatives(&self.mesh_refs(), &self.negative_refs(), settings.layer_height, opts)
        }
    }

    pub fn thumbnail(&self) -> Thumbnail {
        render_meshes(&self.mesh_refs(), 400, 300)
    }
}

/// Known points of the standard scene: (x, y, first layer, last layer) where
/// the point is solid; outside that range it is empty.
pub const STANDARD_PROBES: [(f32, f32, u32, u32); 4] = [
    // solid part of the cuboid (top at 2.5 mm: layers 0..=49)
    (-13.0, 3.0, 0, 49),
    // centre of the cone (3 mm: every layer)
    (9.0, 4.0, 0, 59),
    // floating part of the bar (z 1.8..3.0: layers 36..=59)
    (9.0, -7.0, 36, 59),
    // centre of the hole: never solid
    (-8.0, 0.0, 1, 0),
];

/// A small printer used where the profile itself does not matter (non-square
/// plate and pixels so transposed images are caught).
pub fn small_printer(mirror: MirrorMode) -> PrinterProfile {
    PrinterProfile {
        id: "test-small".into(),
        vendor: "Test".into(),
        name: "Test Small".into(),
        resolution_x: 400,
        resolution_y: 250,
        size_x: 80.0,
        size_y: 60.0,
        size_z: 100.0,
        mirror,
        formats: vec!["goo".into()],
        native_formats: Vec::new(),
        network: None,
        machine_type: Some("Test Printer".into()),
    }
}

/// The built-in printer with the fewest pixels that lists `format` as an output
/// format (the generic printer for SL1, which no vendor profile lists).
pub fn profile_for(format: Format) -> PrinterProfile {
    let ext = format.extension();
    builtin_printers()
        .into_iter()
        .filter(|p| p.formats.iter().any(|f| f == ext))
        .min_by_key(|p| p.resolution_x as u64 * p.resolution_y as u64)
        .unwrap_or_else(|| panic!("no built-in printer lists .{ext}"))
}

// ---------------------------------------------------------------- export

pub struct Exported {
    pub path: PathBuf,
    pub result: ExportResult,
    pub progress: Vec<ExportProgress>,
}

/// Exports `scene` with `slice_to_file` (negatives included).
pub fn export(
    dir: &TempDir,
    name: &str,
    format: Format,
    printer: &PrinterProfile,
    settings: &PrintSettings,
    scene: &Scene,
    thumbnail: Option<&Thumbnail>,
) -> Exported {
    let path = dir.file(&format!("{name}.{}", format.extension()));
    let meshes = scene.mesh_refs();
    let negatives = scene.negative_refs();
    let job = ExportJob {
        meshes: &meshes,
        negatives: &negatives,
        printer,
        settings,
        format,
        thumbnail,
        job_name: "test job",
    };
    let progress = Mutex::new(Vec::new());
    let result = slice_to_file(
        &job,
        &path,
        &|p| progress.lock().unwrap().push(p),
        &AtomicBool::new(false),
    )
    .unwrap_or_else(|e| panic!("{format:?} export failed: {e}"));
    Exported {
        path,
        result,
        progress: progress.into_inner().unwrap(),
    }
}

// ---------------------------------------------------------------- quantisation

/// CTB's 7-bit grey: the top 7 bits are stored, the decoder repeats the lowest
/// stored bit (`v7 << 1 | v7 & 1`), and level 0 is black.
pub fn quantize_ctb7(v: u8) -> u8 {
    let c = v >> 1;
    if c == 0 {
        0
    } else {
        (c << 1) | (c & 1)
    }
}

/// How a format changes pixel values (identity for lossless formats).
pub fn quantizer(format: Format, printer: &PrinterProfile, aa: u8) -> Box<dyn Fn(u8) -> u8> {
    match format {
        Format::Goo | Format::Sl1 | Format::Zip | Format::Cxdlp => Box::new(|v| v),
        Format::Ctb => Box::new(quantize_ctb7),
        Format::Cxdlpv4 => match cxdlpv4::codec_for(printer) {
            V4Codec::Rle7 => Box::new(quantize_ctb7),
            V4Codec::PixelPair => Box::new(cxdlpv4::quantize_pixel_pair),
        },
        Format::Anycubic(m) => match m.codec() {
            LayerCodec::Pw0 => Box::new(anycubic::quantize_pw0),
            LayerCodec::Pws => Box::new(move |v| anycubic::quantize_planes(v, aa)),
        },
    }
}

/// Runs with `q` applied and neighbours of equal value merged (row splits
/// and run limits do not matter).
pub fn canonical(runs: &[Run], q: &dyn Fn(u8) -> u8) -> Vec<(u8, u64)> {
    let mut out: Vec<(u8, u64)> = Vec::with_capacity(runs.len());
    for r in runs {
        if r.len == 0 {
            continue;
        }
        let v = q(r.value);
        match out.last_mut() {
            Some((lv, n)) if *lv == v => *n += r.len as u64,
            _ => out.push((v, r.len as u64)),
        }
    }
    out
}

/// Value of pixel (x, y), found by walking the runs.
pub fn pixel_at(r: &LayerRaster, x: u32, y: u32) -> u8 {
    let target = y as u64 * r.width as u64 + x as u64;
    let mut pos = 0u64;
    for run in &r.runs {
        if target < pos + run.len as u64 {
            return run.value;
        }
        pos += run.len as u64;
    }
    panic!("pixel ({x}, {y}) outside of a {}x{} raster", r.width, r.height);
}

/// Asserts that `got` equals `expected` after quantising `expected` with `q`.
pub fn assert_layer_matches(expected: &LayerRaster, got: &LayerRaster, q: &dyn Fn(u8) -> u8, ctx: &str) {
    assert_eq!(
        (got.width, got.height),
        (expected.width, expected.height),
        "{ctx}: size"
    );
    assert_eq!(
        got.pixel_count(),
        expected.width as u64 * expected.height as u64,
        "{ctx}: pixel count"
    );
    let want = canonical(&expected.runs, q);
    let have = canonical(&got.runs, &|v| v);
    if want != have {
        // locate the first differing pixel for a useful message
        let a: Vec<u8> = expected.to_gray().into_iter().map(q).collect();
        let b = got.to_gray();
        let first = a
            .iter()
            .zip(&b)
            .position(|(x, y)| x != y)
            .unwrap_or(a.len().min(b.len()));
        let diffs = a.iter().zip(&b).filter(|(x, y)| x != y).count();
        let w = expected.width as usize;
        panic!(
            "{ctx}: {diffs} pixels differ, first at ({}, {}): expected {} (raw {}), got {}",
            first % w,
            first / w,
            a.get(first).copied().unwrap_or(0),
            expected.to_gray().get(first).copied().unwrap_or(0),
            b.get(first).copied().unwrap_or(0)
        );
    }
}

// ---------------------------------------------------------------- thumbnails

/// Largest per-channel difference between two RGB images (alpha ignored).
pub fn max_rgb_diff(a: &Thumbnail, b: &Thumbnail) -> u8 {
    assert_eq!((a.width, a.height), (b.width, b.height));
    a.rgba
        .chunks_exact(4)
        .zip(b.rgba.chunks_exact(4))
        .flat_map(|(p, q)| (0..3).map(move |k| p[k].abs_diff(q[k])))
        .max()
        .unwrap_or(0)
}

/// Fraction of pixels that show the (blue-ish) model rather than background.
pub fn model_fraction(t: &Thumbnail) -> f64 {
    let lit = t.rgba.chunks_exact(4).filter(|p| p[2] > 150).count();
    lit as f64 / (t.width as u64 * t.height as u64) as f64
}

/// Asserts that a decoded preview is the scene thumbnail scaled to its size,
/// within `tol` per channel (RGB565/RGB555 storage), and not blank.
pub fn assert_preview(decoded: &Thumbnail, source: &Thumbnail, size: (u32, u32), tol: u8, ctx: &str) {
    assert_eq!((decoded.width, decoded.height), size, "{ctx}: preview size");
    let want = source.fit(size.0, size.1);
    let diff = max_rgb_diff(decoded, &want);
    assert!(
        diff <= tol,
        "{ctx}: preview differs from the thumbnail by {diff} (> {tol})"
    );
    let f = model_fraction(decoded);
    assert!(f > 0.02, "{ctx}: preview looks blank ({:.1}% model pixels)", f * 100.0);
}

// ---------------------------------------------------------------- estimates

/// Independent print time formula: exposure + rests + two-stage lift/retract.
pub fn expected_print_time(s: &PrintSettings, layers: u32) -> f64 {
    let moves = |m: &Motion| {
        let t = |d: f32, v: f32| {
            if d > 0.0 && v > 0.0 {
                d as f64 / v as f64 * 60.0
            } else {
                0.0
            }
        };
        t(m.distance, m.speed) + t(m.distance2, m.speed2)
    };
    (0..layers)
        .map(|i| {
            let p = if i < s.bottom_layers { &s.bottom } else { &s.normal };
            let rests = if s.wait_mode {
                p.wait_before_lift + p.wait_after_lift + p.wait_after_retract
            } else {
                p.light_off_delay
            };
            s.exposure_for_layer(i) as f64 + rests as f64 + moves(&p.lift) + moves(&p.retract)
        })
        .sum()
}

pub fn approx(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

/// Checks the estimate of an export against the layer areas.
pub fn assert_estimate_consistent(est: &PrintEstimate, areas: &[f64], s: &PrintSettings) {
    let vol: f64 = areas.iter().sum::<f64>() * s.layer_height as f64;
    assert!(
        approx(est.volume_mm3, vol, 1e-6 * vol.max(1.0)),
        "{} vs {vol}",
        est.volume_mm3
    );
    assert!(approx(
        est.weight_g,
        vol / 1000.0 * s.resin_density as f64,
        1e-9 + 1e-6 * vol
    ));
    assert!(approx(
        est.cost,
        vol / 1e6 * s.resin_price_per_liter as f64,
        1e-9 + 1e-6 * vol
    ));
    let t = expected_print_time(s, areas.len() as u32);
    assert!(
        approx(est.print_time_s, t, 1e-3 * t),
        "print time {} vs {t}",
        est.print_time_s
    );
}

// ---------------------------------------------------------------- binary helpers

pub fn le_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

pub fn le_u16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}

pub fn le_f32(b: &[u8], at: usize) -> f32 {
    f32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

pub fn be_u32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}

pub fn be_u16(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes(b[at..at + 2].try_into().unwrap())
}

pub fn be_f32(b: &[u8], at: usize) -> f32 {
    f32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}

/// NUL terminated string inside a fixed field.
pub fn fixed_str(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

/// Asserts `a == b` within `tol`, with context.
#[track_caller]
pub fn assert_close(a: f32, b: f32, tol: f32, what: &str) {
    assert!((a - b).abs() <= tol, "{what}: got {a}, expected {b} (tolerance {tol})");
}
