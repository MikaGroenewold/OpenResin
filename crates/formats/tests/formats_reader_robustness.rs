//! Readers must reject damaged files with errors, never panic (or abort on a
//! huge allocation), and report CTB files with an encrypted header as
//! unsupported.

#[path = "formats_helpers.rs"]
mod helpers;

use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};

use helpers::*;
use openresin_core::primitives::cuboid;
use openresin_core::profile::{MirrorMode, PrintSettings};
use openresin_core::Vec3;
use openresin_formats::{ctb, goo, open, AnycubicModel, Format, FormatError};

#[derive(Debug)]
enum Outcome {
    OpenError(String),
    Opened { layers: u32, layer_errors: u32 },
    Panicked(String),
}

impl Outcome {
    fn open_error(&self) -> Option<&str> {
        match self {
            Outcome::OpenError(m) => Some(m),
            _ => None,
        }
    }

    fn any_error(&self) -> bool {
        match self {
            Outcome::OpenError(_) => true,
            Outcome::Opened { layer_errors, .. } => *layer_errors > 0,
            Outcome::Panicked(_) => false,
        }
    }
}

/// Opens `path` and touches everything a viewer would: every layer (up to
/// 200), out of range layers and the thumbnail.
fn probe(path: &Path) -> Outcome {
    let r = std::panic::catch_unwind(AssertUnwindSafe(|| match open(path) {
        Err(e) => Outcome::OpenError(e.to_string()),
        Ok(f) => {
            let n = f.info().layer_count;
            let mut layer_errors = 0;
            for i in 0..n.min(200) {
                if f.layer(i).is_err() {
                    layer_errors += 1;
                }
                let _ = f.layer_z(i);
            }
            if f.layer(n).is_ok() || f.layer(u32::MAX).is_ok() {
                layer_errors += 1000; // out of range layers must be errors
            }
            let _ = f.thumbnail();
            Outcome::Opened {
                layers: n,
                layer_errors,
            }
        }
    }));
    r.unwrap_or_else(|e| {
        let msg = e
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default();
        Outcome::Panicked(msg)
    })
}

/// A small valid file of every format (12 layers, with thumbnail).
fn sample(dir: &TempDir, format: Format) -> (PathBuf, Vec<u8>) {
    let p = small_printer(MirrorMode::Lcd);
    let s = PrintSettings {
        layer_height: 0.1,
        ..test_settings()
    };
    let scene = Scene {
        meshes: vec![
            cuboid(Vec3::new(-10.0, 5.0, 0.6), Vec3::new(12.0, 6.0, 1.2)),
            cuboid(Vec3::new(12.0, -6.0, 0.4), Vec3::new(5.0, 9.0, 0.8)),
        ],
        negatives: Vec::new(),
    };
    let thumb = scene.thumbnail();
    let ex = export(dir, "sample", format, &p, &s, &scene, Some(&thumb));
    let bytes = std::fs::read(&ex.path).unwrap();
    (ex.path, bytes)
}

fn write_variant(dir: &TempDir, format: Format, tag: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.file(&format!("{tag}.{}", format.extension()));
    std::fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn intact_samples_open_cleanly() {
    for f in Format::ALL {
        let dir = TempDir::new("intact");
        let (path, _) = sample(&dir, f);
        match probe(&path) {
            Outcome::Opened {
                layers: 12,
                layer_errors: 0,
            } => {}
            o => panic!("{f:?}: {o:?}"),
        }
    }
}

#[test]
fn truncated_files_are_errors_for_every_format() {
    let mut failures = Vec::new();
    for f in Format::ALL {
        let dir = TempDir::new("trunc");
        let (_, bytes) = sample(&dir, f);
        let n = bytes.len();
        let mut cuts = vec![
            0,
            1,
            3,
            4,
            8,
            16,
            64,
            111,
            112,
            200,
            1000,
            n / 4,
            n / 3,
            n / 2,
            2 * n / 3,
            n - 64,
            n - 5,
            n - 1,
        ];
        cuts.retain(|&c| c < n);
        cuts.sort_unstable();
        cuts.dedup();
        for cut in cuts {
            let path = write_variant(&dir, f, &format!("cut{cut}"), &bytes[..cut]);
            let o = probe(&path);
            if let Outcome::Panicked(m) = &o {
                failures.push(format!("{f:?} cut at {cut}/{n}: panicked: {m}"));
            } else if cut <= n / 2 && !o.any_error() {
                // half the file is gone: header, layer table or layer data is missing
                failures.push(format!("{f:?} cut at {cut}/{n}: no error ({o:?})"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn empty_and_missing_files_are_errors() {
    let dir = TempDir::new("empty");
    for f in Format::ALL {
        let path = write_variant(&dir, f, "empty", &[]);
        assert!(matches!(probe(&path), Outcome::OpenError(_)), "{f:?} empty file");
        let missing = dir.file(&format!("missing.{}", f.extension()));
        assert!(matches!(open(&missing), Err(FormatError::Io(_))), "{f:?} missing file");
        let mut rng = Rng::new(f.extension().len() as u64);
        let junk: Vec<u8> = (0..4096).map(|_| rng.u8()).collect();
        let path = write_variant(&dir, f, "junk", &junk);
        assert!(matches!(probe(&path), Outcome::OpenError(_)), "{f:?} random bytes");
    }
    assert!(matches!(open(&dir.file("model.stl")), Err(FormatError::Unsupported(_))));
    assert!(matches!(
        open(&dir.file("noextension")),
        Err(FormatError::Unsupported(_))
    ));
}

#[test]
fn files_with_the_wrong_extension_are_rejected() {
    let dir = TempDir::new("wrongext");
    let sources = [Format::Goo, Format::Ctb, Format::Zip, Format::Cxdlp];
    for src in sources {
        let (_, bytes) = sample(&dir, src);
        for target in Format::ALL {
            let same_container = matches!((src, target), (Format::Zip, Format::Sl1) | (Format::Sl1, Format::Zip));
            if target == src || same_container {
                continue;
            }
            let path = write_variant(&dir, target, &format!("from-{}", src.extension()), &bytes);
            match probe(&path) {
                Outcome::OpenError(_) => {}
                o => panic!("{src:?} data opened as {target:?}: {o:?}"),
            }
        }
    }
}

/// Random byte flips in the formats owned by this crate's core readers.
#[test]
fn random_corruption_never_panics_goo_ctb_zip_sl1() {
    let mut failures = Vec::new();
    for f in [Format::Goo, Format::Ctb, Format::Zip, Format::Sl1] {
        let dir = TempDir::new("flip");
        let (_, bytes) = sample(&dir, f);
        let mut rng = Rng::new(0xF11F ^ f.extension().len() as u64);
        for k in 0..150 {
            let mut bad = bytes.clone();
            let flips = 1 + rng.below(4);
            // bias towards headers and tables, where sizes and offsets live
            for _ in 0..flips {
                let at = if rng.below(2) == 0 {
                    rng.below(bad.len().min(4096) as u64)
                } else {
                    rng.below(bad.len() as u64)
                } as usize;
                bad[at] ^= 1 << rng.below(8);
                if rng.below(4) == 0 {
                    bad[at] = 0xFF;
                }
            }
            let path = write_variant(&dir, f, &format!("flip{k}"), &bad);
            if let Outcome::Panicked(m) = probe(&path) {
                failures.push(format!("{f:?} variant {k}: {m}"));
            }
            let _ = std::fs::remove_file(&path);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A named modification of a sample file.
type Patch = Box<dyn Fn(&mut Vec<u8>)>;

fn patch_be_u32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_be_bytes());
}

fn patch_le_u32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

/// Header fields that size allocations or point into the file, set to
/// extreme values. Each must give an error (open or layer), not a panic or
/// an attempt to allocate gigabytes.
#[test]
fn goo_bogus_sizes_and_offsets_are_errors() {
    let dir = TempDir::new("goo-fields");
    let (path, bytes) = sample(&dir, Format::Goo);
    let (last_size_at, last_size) = {
        let f = goo::GooFile::open(&path).unwrap();
        let (_, pos, size) = *f.layers.last().unwrap();
        (pos as usize - 4, size)
    };
    let h = goo::HEADER_SIZE;
    // offsets of fields near the end of the header
    let total_layers = 12 + 32 + 24 + 24 + 32 * 3 + 6 + 116 * 116 * 2 + 2 + 290 * 290 * 2 + 2;
    let content_offset = h - 4 - 1 - 2;
    let first_size = h + 66; // u32 size after the first layer definition
    let cases: Vec<(&str, Patch)> = vec![
        (
            "layer count u32::MAX",
            Box::new(move |b| patch_be_u32(b, total_layers, u32::MAX)),
        ),
        (
            "layer count 1 << 28",
            Box::new(move |b| patch_be_u32(b, total_layers, 1 << 28)),
        ),
        (
            "content offset past the end",
            Box::new(move |b| patch_be_u32(b, content_offset, u32::MAX - 100)),
        ),
        (
            "content offset 0",
            Box::new(move |b| patch_be_u32(b, content_offset, 0)),
        ),
        (
            "first layer size u32::MAX",
            Box::new(move |b| patch_be_u32(b, first_size, u32::MAX)),
        ),
        ("first layer size 0", Box::new(move |b| patch_be_u32(b, first_size, 0))),
        (
            "resolution 0 x 0",
            Box::new(move |b| b[total_layers + 4..total_layers + 8].fill(0)),
        ),
        (
            "resolution 65535 x 65535",
            Box::new(move |b| b[total_layers + 4..total_layers + 8].fill(0xFF)),
        ),
        (
            "last layer size grows",
            Box::new(move |b| patch_be_u32(b, last_size_at, last_size + 1_000_000)),
        ),
    ];
    for (name, patch) in cases {
        let mut b = bytes.clone();
        patch(&mut b);
        let path = write_variant(&dir, Format::Goo, "patched", &b);
        let o = probe(&path);
        assert!(o.any_error(), "GOO {name}: {o:?}");
    }
    assert_eq!(
        std::fs::metadata(dir.file("sample.goo")).unwrap().len() as usize,
        bytes.len()
    );
}

#[test]
fn ctb_bogus_sizes_and_offsets_are_errors() {
    let dir = TempDir::new("ctb-fields");
    let (_, bytes) = sample(&dir, Format::Ctb);
    let table = le_u32(&bytes, 64) as usize;
    let large = le_u32(&bytes, 60) as usize;
    let cases: Vec<(&str, Patch)> = vec![
        ("layer count u32::MAX", Box::new(|b| patch_le_u32(b, 68, u32::MAX))),
        ("layer count 1 << 27", Box::new(|b| patch_le_u32(b, 68, 1 << 27))),
        (
            "layer table past the end",
            Box::new(|b| patch_le_u32(b, 64, u32::MAX - 8)),
        ),
        ("resolution 0", Box::new(|b| patch_le_u32(b, 52, 0))),
        ("resolution u32::MAX", Box::new(|b| patch_le_u32(b, 56, u32::MAX))),
        (
            "print parameters past the end",
            Box::new(|b| patch_le_u32(b, 84, u32::MAX - 4)),
        ),
        (
            "layer 0 address past the end",
            Box::new(move |b| patch_le_u32(b, table + 12, u32::MAX - 16)),
        ),
        (
            "layer 0 size u32::MAX",
            Box::new(move |b| patch_le_u32(b, table + 16, u32::MAX)),
        ),
        (
            "last layer size grows",
            Box::new(move |b| {
                let e = table + 36 * 11;
                let size = le_u32(b, e + 16);
                patch_le_u32(b, e + 16, size + 1_000_000);
            }),
        ),
        (
            "wrong encryption key",
            Box::new(|b| {
                let k = le_u32(b, 100);
                patch_le_u32(b, 100, k ^ 0x5555_5555);
            }),
        ),
    ];
    for (name, patch) in cases {
        let mut b = bytes.clone();
        patch(&mut b);
        let path = write_variant(&dir, Format::Ctb, "patched", &b);
        let o = probe(&path);
        assert!(o.any_error(), "CTB {name}: {o:?}");
    }
    // a broken preview must not take the file down, it just has no thumbnail
    for (name, at, v) in [
        ("preview width", large, u32::MAX),
        ("preview height", large + 4, 0x10_0000),
        ("preview data offset", large + 8, u32::MAX - 2),
        ("preview data length", large + 12, u32::MAX),
        ("preview header offset", 60, u32::MAX - 8),
        ("slicer info offset", 104, u32::MAX - 8),
    ] {
        let mut b = bytes.clone();
        patch_le_u32(&mut b, at, v);
        let path = write_variant(&dir, Format::Ctb, "preview", &b);
        let o = probe(&path);
        assert!(!matches!(o, Outcome::Panicked(_)), "CTB {name}: {o:?}");
        if let Ok(f) = open(&path) {
            for i in 0..12 {
                f.layer(i).unwrap_or_else(|e| panic!("CTB {name}: layer {i}: {e}"));
            }
            if let Some(t) = f.thumbnail() {
                assert!(t.width <= 4096 && t.height <= 4096, "CTB {name}");
            }
        }
    }
}

#[test]
fn encrypted_ctb_is_reported_as_unsupported() {
    let dir = TempDir::new("ctb-enc");
    let (_, mut bytes) = sample(&dir, Format::Ctb);
    patch_le_u32(&mut bytes, 0, ctb::MAGIC_CTB_ENCRYPTED);
    for (tag, data) in [("full", bytes.clone()), ("header only", bytes[..112].to_vec())] {
        let path = write_variant(&dir, Format::Ctb, "encrypted", &data);
        match open(&path) {
            Err(FormatError::Unsupported(m)) => assert!(m.contains("encrypted"), "{tag}: {m}"),
            Err(e) => panic!("{tag}: expected Unsupported, got {e}"),
            Ok(_) => panic!("{tag}: encrypted CTB opened"),
        }
    }
    // the magic alone is enough to tell
    let mut head = vec![0u8; 112];
    patch_le_u32(&mut head, 0, ctb::MAGIC_CTB_ENCRYPTED);
    let path = write_variant(&dir, Format::Ctb, "magic", &head);
    assert!(matches!(open(&path), Err(FormatError::Unsupported(_))));
    // other magics are invalid files, not unsupported ones
    patch_le_u32(&mut head, 0, 0x12FD_0000);
    let path = write_variant(&dir, Format::Ctb, "badmagic", &head);
    assert!(matches!(open(&path), Err(FormatError::Invalid(m)) if m.contains("magic")));
}

/// CXDLP and CXDLPv4 files end with a CRC-32 over the whole file: any change is rejected.
#[test]
fn cxdlp_checksums_detect_corruption() {
    for f in [Format::Cxdlp, Format::Cxdlpv4] {
        let dir = TempDir::new("crc");
        let (_, bytes) = sample(&dir, f);
        let mut rng = Rng::new(17);
        for k in 0..40 {
            let mut bad = bytes.clone();
            let at = rng.below(bad.len() as u64) as usize;
            bad[at] ^= 1 << rng.below(8);
            let path = write_variant(&dir, f, &format!("crc{k}"), &bad);
            match probe(&path) {
                Outcome::OpenError(_) => {}
                o => panic!("{f:?}: flipped byte {at} of {}: {o:?}", bad.len()),
            }
        }
    }
}

/// Address of the `LAYERDEF` section from the file mark ("ANYCUBIC", version,
/// table count, then the header, software, preview, grey table and layer
/// definition addresses).
fn layer_defs_offset(b: &[u8]) -> usize {
    assert_eq!(&b[..8], b"ANYCUBIC");
    let at = le_u32(b, 36) as usize;
    assert_eq!(&b[at..at + 8], b"LAYERDEF");
    at
}

/// Photon Workshop files have no checksum; a damaged layer table must still
/// be an error.
#[test]
fn anycubic_damaged_layer_table_is_an_error() {
    let dir = TempDir::new("pw-table");
    for m in [AnycubicModel::Pwmx, AnycubicModel::Pws, AnycubicModel::Pm5s] {
        let f = Format::Anycubic(m);
        let (_, bytes) = sample(&dir, f);
        let defs = layer_defs_offset(&bytes);
        // the LAYERDEF section: 12 byte name, u32 length, u32 count, records
        let cases: Vec<(&str, usize, u32)> = vec![
            ("section length", defs + 12, 7),
            ("layer count + 1", defs + 16, 13),
            ("layer 0 address", defs + 20, u32::MAX - 100),
            ("layer 0 length", defs + 24, 50_000_000),
        ];
        for (name, at, v) in cases {
            let mut b = bytes.clone();
            patch_le_u32(&mut b, at, v);
            let path = write_variant(&dir, f, "patched", &b);
            let o = probe(&path);
            assert!(o.any_error(), "{f:?} {name}: {o:?}");
        }
    }
}

/// A corrupted layer count must not overflow the size computation.
#[test]
fn anycubic_huge_layer_count_is_an_error() {
    let dir = TempDir::new("pw-count");
    let f = Format::Anycubic(AnycubicModel::Pwmx);
    let (_, mut bytes) = sample(&dir, f);
    let defs = layer_defs_offset(&bytes);
    patch_le_u32(&mut bytes, defs + 16, 1 << 28);
    let path = write_variant(&dir, f, "count", &bytes);
    let o = probe(&path);
    assert!(matches!(o, Outcome::OpenError(_)), "{o:?}");
}

/// Readers that keep a layer table answer 0 for layers past the end.
#[test]
fn layer_z_past_the_end_does_not_panic() {
    for f in Format::ALL {
        if f == Format::Cxdlp {
            continue; // see cxdlp_layer_z_past_the_end_does_not_panic
        }
        let dir = TempDir::new("z");
        let (path, _) = sample(&dir, f);
        let file = open(&path).unwrap();
        assert_eq!(file.layer_z(12), 0.0, "{f:?}");
        assert_eq!(file.layer_z(u32::MAX), 0.0, "{f:?}");
    }
}

#[test]
fn cxdlp_layer_z_past_the_end_does_not_panic() {
    let dir = TempDir::new("z-cxdlp");
    let (path, _) = sample(&dir, Format::Cxdlp);
    let file = open(&path).unwrap();
    let z = std::panic::catch_unwind(AssertUnwindSafe(|| file.layer_z(u32::MAX)));
    assert!(z.is_ok(), "layer_z(u32::MAX) panicked");
}

/// Errors say what is wrong.
#[test]
fn errors_are_descriptive() {
    let dir = TempDir::new("msg");
    let (_, bytes) = sample(&dir, Format::Goo);
    let path = write_variant(&dir, Format::Goo, "short", &bytes[..bytes.len() / 2]);
    let o = probe(&path);
    let m = o.open_error().unwrap_or_else(|| panic!("{o:?}"));
    assert!(!m.is_empty());
    let mut b = bytes.clone();
    b[4] ^= 0xFF; // GOO magic
    let path = write_variant(&dir, Format::Goo, "magic", &b);
    assert!(
        probe(&path).open_error().is_some_and(|m| m.contains("magic")),
        "{:?}",
        probe(&path)
    );
}
