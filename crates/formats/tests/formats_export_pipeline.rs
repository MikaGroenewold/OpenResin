//! The export pipeline (`slice_to_file`): errors, cancellation, negative
//! volumes, mirroring, deterministic multi-threaded output and the job
//! information a reader reports afterwards.

#[path = "formats_helpers.rs"]
mod helpers;

use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

use helpers::*;
use openresin_core::primitives::{cuboid, frustum};
use openresin_core::profile::{builtin_printers, find_printer, MirrorMode, PrintSettings, PrinterProfile};
use openresin_core::slicer::LayerRaster;
use openresin_core::{Mesh, Vec3};
use openresin_formats::{ctb, open, slice_to_file, ExportJob, ExportResult, Format, FormatError, Result};

fn job<'a>(
    meshes: &'a [&'a Mesh],
    negatives: &'a [&'a Mesh],
    printer: &'a PrinterProfile,
    settings: &'a PrintSettings,
    format: Format,
) -> ExportJob<'a> {
    ExportJob {
        meshes,
        negatives,
        printer,
        settings,
        format,
        thumbnail: None,
        job_name: "pipeline",
    }
}

fn run(job: &ExportJob, path: &Path) -> Result<ExportResult> {
    slice_to_file(job, path, &|_| {}, &AtomicBool::new(false))
}

fn with_threads<T: Send>(n: usize, f: impl FnOnce() -> T + Send) -> T {
    rayon::ThreadPoolBuilder::new()
        .num_threads(n)
        .build()
        .unwrap()
        .install(f)
}

fn block() -> Mesh {
    cuboid(Vec3::new(0.0, 0.0, 1.0), Vec3::new(10.0, 8.0, 2.0))
}

// ---------------------------------------------------------------- errors

#[test]
fn empty_scene_is_an_error_and_writes_nothing() {
    let p = small_printer(MirrorMode::Lcd);
    let s = test_settings();
    let below = cuboid(Vec3::new(0.0, 0.0, -5.0), Vec3::splat(4.0)); // entirely under the plate
    let empty = Mesh::default();
    for f in Format::ALL {
        let dir = TempDir::new("empty");
        let path = dir.file(&format!("e.{}", f.extension()));
        for meshes in [&[][..], &[&empty][..], &[&below][..]] {
            let err = run(&job(meshes, &[], &p, &s, f), &path).unwrap_err();
            assert!(
                matches!(&err, FormatError::Invalid(m) if m.contains("empty")),
                "{f:?}: unexpected error {err}"
            );
        }
        assert!(dir.entries().is_empty(), "{f:?}: {:?}", dir.entries());
    }
}

#[test]
fn invalid_settings_are_rejected() {
    let p = small_printer(MirrorMode::Lcd);
    let m = block();
    let dir = TempDir::new("invalid");
    let path = dir.file("x.goo");
    let cases: Vec<(&str, PrintSettings)> = vec![
        (
            "anti-aliasing 3",
            PrintSettings {
                anti_aliasing: 3,
                ..test_settings()
            },
        ),
        (
            "anti-aliasing 0",
            PrintSettings {
                anti_aliasing: 0,
                ..test_settings()
            },
        ),
        (
            "layer height 0",
            PrintSettings {
                layer_height: 0.0,
                ..test_settings()
            },
        ),
        (
            "layer height 1 mm",
            PrintSettings {
                layer_height: 1.0,
                ..test_settings()
            },
        ),
        (
            "layer height NaN",
            PrintSettings {
                layer_height: f32::NAN,
                ..test_settings()
            },
        ),
        ("zero exposure", {
            let mut s = test_settings();
            s.normal.exposure = 0.0;
            s
        }),
        ("negative bottom exposure", {
            let mut s = test_settings();
            s.bottom.exposure = -1.0;
            s
        }),
    ];
    for (name, s) in cases {
        let err = run(&job(&[&m], &[], &p, &s, Format::Goo), &path).unwrap_err();
        assert!(matches!(err, FormatError::Invalid(_)), "{name}: {err}");
    }
    assert!(dir.entries().is_empty());
}

#[test]
fn writer_errors_remove_the_partial_file() {
    let m = block();
    let s = test_settings();
    let mut huge = small_printer(MirrorMode::Lcd);
    huge.resolution_x = 20_000; // beyond CXDLP's 14-bit columns
    huge.resolution_y = 9_000;
    let dir = TempDir::new("writer-error");
    let path = dir.file("big.cxdlp");
    std::fs::write(&path, b"previous file").unwrap();
    let err = run(&job(&[&m], &[], &huge, &s, Format::Cxdlp), &path).unwrap_err();
    assert!(matches!(err, FormatError::Unsupported(_)), "{err}");
    assert_eq!(dir.entries(), vec!["big.cxdlp".to_string()], "no .part file left");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"previous file",
        "existing file untouched"
    );
}

// ---------------------------------------------------------------- cancellation

#[test]
fn cancel_before_start_for_every_format() {
    let p = small_printer(MirrorMode::Lcd);
    let s = test_settings();
    let m = block();
    for f in Format::ALL {
        let dir = TempDir::new("cancel0");
        let path = dir.file(&format!("c.{}", f.extension()));
        let calls = AtomicU32::new(0);
        let err = slice_to_file(
            &job(&[&m], &[], &p, &s, f),
            &path,
            &|_| {
                calls.fetch_add(1, Ordering::Relaxed);
            },
            &AtomicBool::new(true),
        )
        .unwrap_err();
        assert!(matches!(err, FormatError::Cancelled), "{f:?}: {err}");
        assert_eq!(calls.load(Ordering::Relaxed), 0, "{f:?}: no progress after cancel");
        assert!(dir.entries().is_empty(), "{f:?}: left {:?}", dir.entries());
    }
}

#[test]
fn cancel_during_export_stops_early_for_every_format() {
    let p = small_printer(MirrorMode::Lcd);
    let s = test_settings();
    let scene = Scene::standard();
    let meshes = scene.mesh_refs();
    for f in Format::ALL {
        let dir = TempDir::new("cancel1");
        let path = dir.file(&format!("c.{}", f.extension()));
        std::fs::write(&path, b"keep me").unwrap();
        let cancel = AtomicBool::new(false);
        let seen = Mutex::new(Vec::new());
        // two threads -> batches of 4 layers, so 60 layers need 15 batches
        let err = with_threads(2, || {
            slice_to_file(
                &job(&meshes, &[], &p, &s, f),
                &path,
                &|pr| {
                    seen.lock().unwrap().push(pr);
                    cancel.store(true, Ordering::Relaxed);
                },
                &cancel,
            )
        })
        .unwrap_err();
        assert!(matches!(err, FormatError::Cancelled), "{f:?}: {err}");
        let seen = seen.into_inner().unwrap();
        assert_eq!(seen.len(), 1, "{f:?}: stopped after the first batch");
        assert_eq!((seen[0].done, seen[0].total), (4, STANDARD_LAYERS), "{f:?}");
        assert_eq!(
            dir.entries(),
            vec![format!("c.{}", f.extension())],
            "{f:?}: no .part left"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"keep me",
            "{f:?}: old file replaced by a cancelled export"
        );
    }
}

#[test]
fn successful_export_replaces_an_existing_file() {
    let p = small_printer(MirrorMode::Lcd);
    let s = test_settings();
    let m = block();
    let dir = TempDir::new("replace");
    let path = dir.file("r.goo");
    std::fs::write(&path, b"old").unwrap();
    let res = run(&job(&[&m], &[], &p, &s, Format::Goo), &path).unwrap();
    assert_eq!(dir.entries(), vec!["r.goo".to_string()]);
    assert_eq!(res.file_size, std::fs::metadata(&path).unwrap().len());
    assert_eq!(open(&path).unwrap().info().layer_count, 40);
}

// ---------------------------------------------------------------- negatives

fn decode_all(path: &Path) -> Vec<LayerRaster> {
    let f = open(path).unwrap();
    (0..f.info().layer_count).map(|i| f.layer(i).unwrap()).collect()
}

#[test]
fn negative_volumes_are_subtracted() {
    let p = small_printer(MirrorMode::Lcd);
    let s = test_settings();
    let opts = p.raster_options(s.anti_aliasing);
    let base = block(); // 10 x 8 x 2 mm, 40 layers
    let r = 2.0;
    let hole = frustum(Vec3::new(1.0, 0.5, -1.0), Vec3::new(1.0, 0.5, 3.0), r, r, 64);
    let mut flipped = hole.clone();
    flipped.flip();
    let far_away = cuboid(Vec3::new(25.0, 20.0, 1.0), Vec3::splat(3.0));
    let wider = cuboid(Vec3::new(5.0, 0.0, 1.5), Vec3::new(6.0, 20.0, 1.0)); // cuts x 2..5 for z 1..2
    let dir = TempDir::new("neg");
    let export = |name: &str, negatives: &[&Mesh]| -> (ExportResult, Vec<LayerRaster>) {
        let path = dir.file(&format!("{name}.goo"));
        let res = run(&job(&[&base], negatives, &p, &s, Format::Goo), &path).unwrap();
        let layers = decode_all(&path);
        (res, layers)
    };
    let (plain, plain_layers) = export("plain", &[]);
    let (holed, holed_layers) = export("hole", &[&hole]);
    let (_, flipped_layers) = export("flipped", &[&flipped]);
    let (_, far_layers) = export("far", &[&far_away]);
    let (cut, cut_layers) = export("wider", &[&wider]);

    assert_eq!(plain_layers.len(), 40);
    assert_eq!(holed_layers.len(), 40, "layer count comes from the positive meshes");
    assert_eq!(flipped_layers, holed_layers, "orientation of negatives does not matter");
    assert_eq!(far_layers, plain_layers, "a negative outside the part changes nothing");

    let (hx, hy) = opts.to_pixel(1.0, 0.5);
    let (sx, sy) = opts.to_pixel(-4.0, 3.0);
    for i in 0..40 {
        assert_eq!(pixel_at(&plain_layers[i], hx as u32, hy as u32), 255, "layer {i}");
        assert_eq!(pixel_at(&holed_layers[i], hx as u32, hy as u32), 0, "layer {i}: hole");
        assert_eq!(
            pixel_at(&holed_layers[i], sx as u32, sy as u32),
            255,
            "layer {i}: solid part"
        );
    }
    // removed volume = polygon area * 2 mm height
    let n = 64.0f64;
    let hole_area = 0.5 * n * (r as f64).powi(2) * (std::f64::consts::TAU / n).sin();
    let removed = plain.estimate.volume_mm3 - holed.estimate.volume_mm3;
    assert!(
        (removed - hole_area * 2.0).abs() < 0.02 * hole_area * 2.0,
        "removed {removed} mm³, expected {}",
        hole_area * 2.0
    );
    assert!(
        (plain.estimate.volume_mm3 - 160.0).abs() < 1.0,
        "block volume {}",
        plain.estimate.volume_mm3
    );
    // the wider negative removes x 2..5 (3 x 8 mm) in the upper 1 mm only
    let removed = plain.estimate.volume_mm3 - cut.estimate.volume_mm3;
    assert!((removed - 24.0).abs() < 0.5, "removed {removed} mm³, expected 24");
    let (cx, cy) = opts.to_pixel(3.5, 0.0);
    assert_eq!(pixel_at(&cut_layers[10], cx as u32, cy as u32), 255, "below the cut");
    assert_eq!(pixel_at(&cut_layers[30], cx as u32, cy as u32), 0, "inside the cut");
}

#[test]
fn overlapping_meshes_are_united() {
    let p = small_printer(MirrorMode::Lcd);
    let s = test_settings();
    let a = cuboid(Vec3::new(0.0, 0.0, 1.0), Vec3::new(10.0, 10.0, 2.0));
    let b = cuboid(Vec3::new(5.0, 0.0, 1.0), Vec3::new(10.0, 10.0, 2.0)); // overlaps half of a
    let dir = TempDir::new("union");
    let path = dir.file("u.goo");
    let res = run(&job(&[&a, &b], &[], &p, &s, Format::Goo), &path).unwrap();
    // union 15 x 10 x 2 = 300 mm³, not 400
    assert!(
        (res.estimate.volume_mm3 - 300.0).abs() < 1.5,
        "{}",
        res.estimate.volume_mm3
    );
    // the row through both boxes is one continuous run of 15 mm (75 pixels), no seam
    let layer = &decode_all(&path)[5];
    let (_, row) = p.raster_options(s.anti_aliasing).to_pixel(0.0, 0.0);
    let row = row as u32;
    let values: Vec<u8> = (0..p.resolution_x).map(|x| pixel_at(layer, x, row)).collect();
    let lit: Vec<usize> = (0..values.len()).filter(|&x| values[x] != 0).collect();
    assert_eq!(lit.len(), 75, "{values:?}");
    assert_eq!(lit[74] - lit[0], 74, "lit pixels are contiguous");
    assert!(lit.iter().all(|&x| values[x] == 255));
}

// ---------------------------------------------------------------- mirroring

/// `(mirror_x, mirror_y)` header flags each format reports for a mirror mode.
fn mirror_flags(format: Format, m: MirrorMode) -> (bool, bool) {
    match format {
        Format::Goo => (m.is_mirrored() && m != MirrorMode::Y, m == MirrorMode::Y),
        Format::Ctb | Format::Zip | Format::Cxdlpv4 => (m.is_mirrored(), false),
        Format::Sl1 => (m == MirrorMode::X, matches!(m, MirrorMode::Y | MirrorMode::Lcd)),
        Format::Cxdlp | Format::Anycubic(_) => (false, false),
    }
}

/// An "L": a stem at the left (x -30..-20, y -20..20) and a foot at the front
/// (x -20..10, y -20..-10), 1 mm high.
fn l_shape() -> Vec<Mesh> {
    vec![
        cuboid(Vec3::new(-25.0, 0.0, 0.5), Vec3::new(10.0, 40.0, 1.0)),
        cuboid(Vec3::new(-5.0, -15.0, 0.5), Vec3::new(30.0, 10.0, 1.0)),
    ]
}

/// Whether plate point (x, y) is inside the L; `None` close to an edge.
fn in_l(x: f32, y: f32) -> Option<bool> {
    let inside = |x0: f32, x1: f32, y0: f32, y1: f32| -> Option<bool> {
        let d = (x - x0)
            .abs()
            .min((x - x1).abs())
            .min((y - y0).abs())
            .min((y - y1).abs());
        let inside = x > x0 && x < x1 && y > y0 && y < y1;
        if d < 0.6 {
            None
        } else {
            Some(inside)
        }
    };
    match (inside(-30.0, -20.0, -20.0, 20.0), inside(-20.0, 10.0, -20.0, -10.0)) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        (Some(false), Some(false)) => Some(false),
        _ => None,
    }
}

/// Plate position of the centre of image pixel (col, row) for a mirror mode,
/// following the documented conventions: row 0 is the back (+Y) edge and
/// column 0 the left (-X) edge of the top view; `Lcd` and `Y` flip the rows
/// (the plate seen from below), `X` flips the columns.
fn pixel_to_plate(p: &PrinterProfile, col: u32, row: u32) -> (f32, f32) {
    let (w, h) = (p.resolution_x as f32, p.resolution_y as f32);
    let (mut cx, mut cy) = (col as f32 + 0.5, row as f32 + 0.5);
    if p.mirror == MirrorMode::X {
        cx = w - cx;
    }
    if matches!(p.mirror, MirrorMode::Y | MirrorMode::Lcd) {
        cy = h - cy;
    }
    (cx * p.size_x / w - p.size_x / 2.0, p.size_y / 2.0 - cy * p.size_y / h)
}

#[test]
fn mirror_modes_reach_every_format() {
    let s = test_settings();
    let shape = l_shape();
    let meshes: Vec<&Mesh> = shape.iter().collect();
    for mode in [MirrorMode::None, MirrorMode::Lcd, MirrorMode::X, MirrorMode::Y] {
        let p = small_printer(mode);
        for f in Format::ALL {
            let ctx = format!("{f:?} {mode:?}");
            let dir = TempDir::new("mirror");
            let path = dir.file(&format!("l.{}", f.extension()));
            run(&job(&meshes, &[], &p, &s, f), &path).unwrap();
            let file = open(&path).unwrap();
            let info = file.info();
            assert_eq!(
                (info.mirror_x, info.mirror_y),
                mirror_flags(f, mode),
                "{ctx}: header flags"
            );
            let layer = file.layer(10).unwrap();
            let mut checked = (0, 0);
            for row in (0..p.resolution_y).step_by(7) {
                for col in (0..p.resolution_x).step_by(7) {
                    let (x, y) = pixel_to_plate(&p, col, row);
                    let Some(solid) = in_l(x, y) else { continue };
                    let v = pixel_at(&layer, col, row);
                    assert_eq!(
                        v,
                        if solid { 255 } else { 0 },
                        "{ctx}: pixel ({col}, {row}) = plate ({x:.1}, {y:.1})"
                    );
                    if solid {
                        checked.0 += 1;
                    } else {
                        checked.1 += 1;
                    }
                }
            }
            assert!(checked.0 > 100 && checked.1 > 100, "{ctx}: {checked:?}");
        }
    }
}

#[test]
fn lcd_mirror_flips_rows_of_the_top_view() {
    // with AA off every pixel is sampled at its centre, so the images are exact mirrors
    let s = PrintSettings {
        anti_aliasing: 1,
        ..test_settings()
    };
    let shape = l_shape();
    let meshes: Vec<&Mesh> = shape.iter().collect();
    let dir = TempDir::new("flip");
    let mut images = Vec::new();
    for mode in [MirrorMode::None, MirrorMode::Lcd, MirrorMode::X] {
        let p = small_printer(mode);
        let path = dir.file(&format!("{mode:?}.goo"));
        run(&job(&meshes, &[], &p, &s, Format::Goo), &path).unwrap();
        images.push(open(&path).unwrap().layer(5).unwrap().to_gray());
    }
    let (w, h) = (400usize, 250usize);
    let px = |img: &Vec<u8>, x: usize, y: usize| img[y * w + x];
    let mut lit = 0;
    for y in 0..h {
        for x in 0..w {
            let top = px(&images[0], x, y);
            assert_eq!(px(&images[1], x, h - 1 - y), top, "LCD mirror at ({x}, {y})");
            assert_eq!(px(&images[2], w - 1 - x, y), top, "X mirror at ({x}, {y})");
            lit += (top != 0) as u32;
        }
    }
    assert!(lit > 1000);
    // in the top view the foot (front, -Y) is at the bottom, after LCD mirroring at the top
    let foot_col = ((-5.0f32 + 40.0) / 0.2) as usize;
    let foot_row_top_view = ((30.0f32 + 15.0) / 0.24) as usize;
    assert_eq!(px(&images[0], foot_col, foot_row_top_view), 255);
    assert_eq!(px(&images[1], foot_col, h - 1 - foot_row_top_view), 255);
    assert_eq!(px(&images[1], foot_col, foot_row_top_view), 0);
}

// ---------------------------------------------------------------- determinism

/// Rewrites the CTB layers of `b` with the key of `a`.
fn rekey_ctb(a: &[u8], b: &mut [u8]) {
    let (seed_a, seed_b) = (le_u32(a, 100), le_u32(b, 100));
    let table = le_u32(b, 64) as usize;
    for i in 0..le_u32(b, 68) {
        let e = table + 36 * i as usize;
        let (addr, size) = (le_u32(b, e + 12) as usize, le_u32(b, e + 16) as usize);
        let data = &mut b[addr..addr + size];
        ctb::crypt_layer(seed_b, i, data);
        ctb::crypt_layer(seed_a, i, data);
    }
    b[100..104].copy_from_slice(&seed_a.to_le_bytes());
}

fn zip_contents(path: &Path) -> Vec<(String, Vec<u8>)> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    (0..zip.len())
        .map(|i| {
            let mut e = zip.by_index(i).unwrap();
            let mut data = Vec::new();
            e.read_to_end(&mut data).unwrap();
            if e.name() == "config.ini" {
                let text = String::from_utf8(data).unwrap();
                data = text
                    .lines()
                    .filter(|l| !l.starts_with("fileCreationTimestamp"))
                    .collect::<Vec<_>>()
                    .join("\n")
                    .into_bytes();
            }
            (e.name().to_string(), data)
        })
        .collect()
}

#[test]
fn multithreaded_exports_are_deterministic() {
    let p = small_printer(MirrorMode::Lcd);
    let s = test_settings();
    let scene = Scene::standard();
    let thumb = scene.thumbnail();
    for f in Format::ALL {
        let dir = TempDir::new("det");
        let a = with_threads(1, || export(&dir, "one", f, &p, &s, &scene, Some(&thumb)));
        let b = with_threads(6, || export(&dir, "six", f, &p, &s, &scene, Some(&thumb)));
        assert_eq!(decode_all(&a.path), decode_all(&b.path), "{f:?}: decoded layers differ");
        assert_eq!(a.result.layer_areas, b.result.layer_areas, "{f:?}");
        let (ba, mut bb) = (std::fs::read(&a.path).unwrap(), std::fs::read(&b.path).unwrap());
        if !matches!(f, Format::Sl1 | Format::Zip) {
            assert_eq!(ba.len(), bb.len(), "{f:?}: sizes differ");
        }
        match f {
            Format::Goo => {
                // only the creation time (24 bytes at offset 68) may differ
                bb[68..92].copy_from_slice(&ba[68..92]);
                assert!(ba == bb, "{f:?}: bytes differ outside the timestamp");
            }
            Format::Ctb => {
                assert_ne!(le_u32(&ba, 100), 0);
                rekey_ctb(&ba, &mut bb);
                assert!(ba == bb, "{f:?}: bytes differ beyond the encryption key");
            }
            // archive entries (timestamps of the zip container itself do not matter)
            Format::Sl1 | Format::Zip => assert_eq!(zip_contents(&a.path), zip_contents(&b.path), "{f:?}"),
            Format::Cxdlpv4 => {
                // a minute timestamp in the slicer info and the trailing CRC may differ
                let n = ba.len();
                let diff: Vec<usize> = (0..n - 4).filter(|&i| ba[i] != bb[i]).collect();
                if let (Some(&first), Some(&last)) = (diff.first(), diff.last()) {
                    assert!(last - first < 4, "{f:?}: differences at {diff:?}");
                }
            }
            _ => assert!(ba == bb, "{f:?}: output depends on the thread count"),
        }
    }
}

// ---------------------------------------------------------------- job info (open_sliced_file)

/// Mirrors the app's `open_sliced_file`: the summary it shows comes from the
/// reader, so it must match what the export reported.
#[test]
fn reopened_goo_and_ctb_report_the_written_job() {
    let list = builtin_printers();
    let cube = cuboid(Vec3::new(-5.0, 0.0, 1.0), Vec3::new(10.0, 8.0, 2.0));
    let cyl = frustum(Vec3::new(8.0, 0.0, 0.0), Vec3::new(8.0, 0.0, 2.0), 3.0, 3.0, 64);
    let segments = 64.0f64;
    let cyl_area = 0.5 * segments * 9.0 * (std::f64::consts::TAU / segments).sin();
    let volume_mm3 = 160.0 + cyl_area * 2.0;
    for (format, printer) in [
        (Format::Goo, "elegoo-mars-5"),
        (Format::Ctb, "elegoo-mars"),
        (Format::Goo, "test"),
    ] {
        let p = if printer == "test" {
            small_printer(MirrorMode::Lcd)
        } else {
            find_printer(&list, printer).unwrap().clone()
        };
        let s = test_settings();
        let dir = TempDir::new("jobinfo");
        let path = dir.file(&format!("job.{}", format.extension()));
        let res = run(&job(&[&cube, &cyl], &[], &p, &s, format), &path).unwrap();
        let ctx = format!("{format:?} on {printer}");

        let file = open(&path).unwrap();
        let info = file.info().clone();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .and_then(Format::from_extension);
        assert_eq!(ext, Some(format), "{ctx}");
        assert_eq!(info.layer_count, 40, "{ctx}");
        assert_eq!(info.layer_count, res.estimate.layer_count, "{ctx}");
        let height = info.layer_count as f64 * info.layer_height as f64;
        assert!((height - res.estimate.height_mm).abs() < 1e-4, "{ctx}: height {height}");
        assert!((height - 2.0).abs() < 1e-4, "{ctx}: height {height}");
        // print time: rounded seconds of the estimate, which follows the settings
        let t = expected_print_time(&s, 40);
        assert!(
            (res.estimate.print_time_s - t).abs() < 1e-3 * t,
            "{ctx}: estimate {}",
            res.estimate.print_time_s
        );
        assert!(
            (info.print_time_s as f64 - t).abs() <= 1.0,
            "{ctx}: print time {} vs {t}",
            info.print_time_s
        );
        // volume: what was written, and close to the geometry
        assert!(
            (info.volume_ml as f64 - res.estimate.volume_ml()).abs() < 1e-5,
            "{ctx}: volume {}",
            info.volume_ml
        );
        assert!(
            (info.volume_ml as f64 * 1000.0 - volume_mm3).abs() < 0.01 * volume_mm3,
            "{ctx}: volume {} ml vs geometry {} mm³",
            info.volume_ml,
            volume_mm3
        );
        assert_eq!(std::fs::metadata(&path).unwrap().len(), res.file_size, "{ctx}");
        // the per-layer areas add up to the volume
        let total: f64 = res.layer_areas.iter().sum::<f64>() * s.layer_height as f64;
        assert!((total - res.estimate.volume_mm3).abs() < 1e-6 * total);
    }
}
