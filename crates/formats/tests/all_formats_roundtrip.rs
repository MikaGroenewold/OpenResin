//! End-to-end round trip of every output format.
//!
//! For every [`Format`] a known scene (cuboid with a subtracted through-hole,
//! a cone frustum, an overhanging bar) is exported with `slice_to_file` for a
//! built-in printer that lists the format, read back with `openresin_formats::open`
//! and compared with the core slicer: header metadata, every layer pixel for
//! pixel (after the format's documented quantisation), layer heights and
//! previews. GOO, CTB, SL1 and PNG zip files are additionally parsed field by
//! field.

#[path = "formats_helpers.rs"]
mod helpers;

use std::io::Read;
use std::path::Path;

use helpers::*;
use openresin_core::estimate::PrintEstimate;
use openresin_core::profile::{builtin_printers, find_printer, MirrorMode, PrintSettings, PrinterProfile};
use openresin_formats::anycubic::AnycubicModel;
use openresin_formats::goo::{self, GooFile};
use openresin_formats::preview::Thumbnail;
use openresin_formats::{ctb, open, Format, SlicedInfo};

// ---------------------------------------------------------------- expectations

/// What a format stores, as documented by its writer/reader.
struct Stored {
    print_time: bool,
    volume: bool,
    anti_aliasing: bool,
    /// `(mirror_x, mirror_y)` reported for a printer mirror mode.
    mirror: fn(MirrorMode) -> (bool, bool),
    /// Exposures are stored in steps of this many seconds (0: as f32).
    exposure_step: f32,
    bottom_exposure_step: f32,
    /// Size of the preview returned by `SlicedFile::thumbnail`.
    thumbnail: (u32, u32),
    /// Per-channel tolerance of the preview (RGB565/RGB555 storage).
    thumbnail_tol: u8,
}

fn stored(format: Format) -> Stored {
    let flagged: fn(MirrorMode) -> (bool, bool) = |m| (m.is_mirrored(), false);
    let none: fn(MirrorMode) -> (bool, bool) = |_| (false, false);
    let base = Stored {
        print_time: true,
        volume: true,
        anti_aliasing: true,
        mirror: flagged,
        exposure_step: 0.0,
        bottom_exposure_step: 0.0,
        thumbnail: (0, 0),
        thumbnail_tol: 8,
    };
    match format {
        Format::Goo => Stored {
            // LCD mirroring is flagged as "X mirror" like the reference slicer
            mirror: |m| (m.is_mirrored() && m != MirrorMode::Y, m == MirrorMode::Y),
            thumbnail: (goo::BIG_PREVIEW, goo::BIG_PREVIEW),
            ..base
        },
        Format::Ctb => Stored {
            thumbnail: (400, 300),
            ..base
        },
        Format::Zip => Stored {
            thumbnail: (400, 300),
            thumbnail_tol: 0,
            ..base
        },
        Format::Sl1 => Stored {
            anti_aliasing: false,
            mirror: |m| (m == MirrorMode::X, matches!(m, MirrorMode::Y | MirrorMode::Lcd)),
            thumbnail: (800, 480),
            thumbnail_tol: 0,
            ..base
        },
        Format::Cxdlp => Stored {
            print_time: false,
            volume: false,
            anti_aliasing: false,
            mirror: none,
            exposure_step: 0.1,
            bottom_exposure_step: 1.0,
            thumbnail: (290, 290),
            ..base
        },
        Format::Cxdlpv4 => Stored {
            thumbnail: (300, 300),
            ..base
        },
        Format::Anycubic(_) => Stored {
            mirror: none,
            thumbnail: (224, 168),
            ..base
        },
    }
}

fn stepped(v: f32, step: f32) -> f32 {
    if step == 0.0 {
        v
    } else {
        (v / step).round() * step
    }
}

fn check_info(
    format: Format,
    printer: &PrinterProfile,
    s: &PrintSettings,
    est: &PrintEstimate,
    info: &SlicedInfo,
    ctx: &str,
) {
    let st = stored(format);
    assert_eq!(info.format, format.extension(), "{ctx}: format");
    assert!(!info.machine_name.trim().is_empty(), "{ctx}: machine name is empty");
    if matches!(format, Format::Goo | Format::Ctb | Format::Zip | Format::Sl1) {
        assert_eq!(info.machine_name, printer.machine_type(), "{ctx}: machine name");
    }
    assert_eq!(
        (info.resolution_x, info.resolution_y),
        (printer.resolution_x, printer.resolution_y),
        "{ctx}: resolution"
    );
    // plate size: exact, except text with 2 decimals (CXDLP) and old Photon
    // Workshop files, which only store one (square) pixel size
    let size_tol = match format {
        Format::Cxdlp => 0.006,
        Format::Anycubic(m) if m.version_for(printer) < anycubic_v516() => {
            let (px, py) = printer.pixel_size_um();
            if (px - py).abs() > 0.01 * px.max(py) {
                f32::INFINITY // non-square pixels cannot be represented
            } else {
                0.005 * printer.size_x.max(printer.size_y)
            }
        }
        _ => 1e-3,
    };
    assert_close(info.size_x, printer.size_x, size_tol, &format!("{ctx}: size_x"));
    assert_close(info.size_y, printer.size_y, size_tol, &format!("{ctx}: size_y"));
    assert_eq!(info.layer_count, STANDARD_LAYERS, "{ctx}: layer count");
    assert_close(info.layer_height, s.layer_height, 1e-6, &format!("{ctx}: layer height"));
    assert_close(
        info.exposure,
        stepped(s.normal.exposure, st.exposure_step),
        1e-4,
        &format!("{ctx}: exposure"),
    );
    assert_close(
        info.bottom_exposure,
        stepped(s.bottom.exposure, st.bottom_exposure_step),
        1e-4,
        &format!("{ctx}: bottom exposure"),
    );
    assert_eq!(info.bottom_layers, s.bottom_layers, "{ctx}: bottom layers");
    if st.print_time {
        let want = est.print_time_s.round();
        assert!(
            (info.print_time_s as f64 - want).abs() <= 1.0,
            "{ctx}: print time {} vs estimate {}",
            info.print_time_s,
            est.print_time_s
        );
    } else {
        assert_eq!(info.print_time_s, 0, "{ctx}: print time is not stored");
    }
    if st.volume {
        let want = est.volume_ml();
        assert!(
            (info.volume_ml as f64 - want).abs() <= 1e-3 + 1e-4 * want,
            "{ctx}: volume {} ml vs estimate {want} ml",
            info.volume_ml
        );
    } else {
        assert_eq!(info.volume_ml, 0.0, "{ctx}: volume is not stored");
    }
    assert_eq!(
        (info.mirror_x, info.mirror_y),
        (st.mirror)(printer.mirror),
        "{ctx}: mirror flags for {:?}",
        printer.mirror
    );
    let aa = if st.anti_aliasing { s.anti_aliasing as u32 } else { 1 };
    assert_eq!(info.anti_aliasing, aa, "{ctx}: anti-aliasing");
}

fn anycubic_v516() -> u32 {
    openresin_formats::anycubic::VERSION_516
}

// ---------------------------------------------------------------- the round trip

fn roundtrip(format: Format, printer: &PrinterProfile) {
    roundtrip_with(format, printer, &test_settings());
}

fn roundtrip_with(format: Format, printer: &PrinterProfile, s: &PrintSettings) {
    let ctx = format!(
        "{format:?} on {} ({}x{}, AA {}, wait mode {})",
        printer.id, printer.resolution_x, printer.resolution_y, s.anti_aliasing, s.wait_mode
    );
    let dir = TempDir::new(&format!("rt-{}", format.extension()));
    let s = s.clone();
    let scene = Scene::standard();
    let thumb = scene.thumbnail();
    let ex = export(&dir, "scene", format, printer, &s, &scene, Some(&thumb));

    // export result and pipeline bookkeeping
    let n = STANDARD_LAYERS;
    assert_eq!(ex.result.format, format, "{ctx}");
    assert_eq!(ex.result.path, ex.path, "{ctx}");
    assert_eq!(ex.result.layer_areas.len() as u32, n, "{ctx}: layer areas");
    assert_eq!(ex.result.estimate.layer_count, n, "{ctx}: estimate layers");
    assert_eq!(
        ex.result.file_size,
        std::fs::metadata(&ex.path).unwrap().len(),
        "{ctx}: file size"
    );
    let name = ex.path.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(dir.entries(), vec![name], "{ctx}: only the output file is left");
    let last = ex.progress.last().copied().expect("progress reported");
    assert_eq!((last.done, last.total), (n, n), "{ctx}: final progress");
    assert!(
        ex.progress.windows(2).all(|w| w[0].done < w[1].done && w[1].total == n),
        "{ctx}: progress not monotonic: {:?}",
        ex.progress
    );
    assert_estimate_consistent(&ex.result.estimate, &ex.result.layer_areas, &s);

    // reference rasterization
    let slicer = scene.slicer(printer, &s);
    assert_eq!(slicer.layer_count() as u32, n, "{ctx}: reference layer count");
    let opts = printer.raster_options(s.anti_aliasing);
    let (pw, ph) = opts.pixel_size();

    let file = open(&ex.path).unwrap_or_else(|e| panic!("{ctx}: open failed: {e}"));
    check_info(format, printer, &s, &ex.result.estimate, file.info(), &ctx);

    let q = quantizer(format, printer, s.anti_aliasing);
    let mut prev_z = 0.0f32;
    for i in 0..n {
        let lctx = format!("{ctx} layer {i}");
        let expected = slicer.rasterize(i as usize);
        // the pipeline reported the area of exactly this raster
        let area = expected.lit_area_px() * (pw * ph) as f64;
        assert!(
            approx(ex.result.layer_areas[i as usize], area, 1e-9 * area.max(1.0)),
            "{lctx}: reported area"
        );
        assert!(!expected.is_blank(), "{lctx}: scene layer is empty");
        let got = file.layer(i).unwrap_or_else(|e| panic!("{lctx}: decode failed: {e}"));
        assert_layer_matches(&expected, &got, &*q, &lctx);
        // independent of the rasterizer: known solid and empty points
        for (x, y, first, last) in STANDARD_PROBES {
            let (px, py) = opts.to_pixel(x, y);
            let v = pixel_at(&got, px as u32, py as u32);
            let solid = first <= i && i <= last;
            assert_eq!(v, if solid { 255 } else { 0 }, "{lctx}: probe ({x}, {y})");
        }
        let z = file.layer_z(i);
        assert_close(z, (i + 1) as f32 * s.layer_height, 1e-4, &format!("{lctx}: z"));
        assert!(z > prev_z, "{lctx}: z {z} not above {prev_z}");
        prev_z = z;
    }
    assert!(file.layer(n).is_err(), "{ctx}: layer past the end must be an error");

    let st = stored(format);
    let t = file.thumbnail().unwrap_or_else(|| panic!("{ctx}: no thumbnail"));
    assert_preview(&t, &thumb, st.thumbnail, st.thumbnail_tol, &format!("{ctx}: thumbnail"));

    match format {
        Format::Goo => goo_details(&ex.path, printer, &s, &ex.result.estimate, &thumb),
        Format::Ctb => ctb_details(&ex.path, printer, &s, &ex.result.estimate, &thumb),
        Format::Zip => zip_details(&ex.path, printer, &s, &ex.result.estimate, &thumb),
        Format::Sl1 => sl1_details(&ex.path, printer, &s, &ex.result.estimate, &thumb),
        _ => {}
    }
}

// ---------------------------------------------------------------- GOO fields

fn goo_details(path: &Path, p: &PrinterProfile, s: &PrintSettings, est: &PrintEstimate, thumb: &Thumbnail) {
    let g = GooFile::open(path).unwrap();
    let h = &g.header;
    let (b, n) = (&s.bottom, &s.normal);
    assert_eq!(h.software_info, "OpenResin");
    assert!(!h.software_version.is_empty());
    assert!(h.file_time.parse::<u64>().is_ok(), "file time {:?}", h.file_time);
    assert_eq!(h.printer_name, p.machine_type());
    assert_eq!(h.printer_type, p.machine_type());
    assert_eq!(h.profile_name, s.name);
    assert_eq!(h.anti_aliasing, s.anti_aliasing as u16);
    assert_eq!(h.total_layers, STANDARD_LAYERS);
    assert_eq!(
        (h.resolution_x as u32, h.resolution_y as u32),
        (p.resolution_x, p.resolution_y)
    );
    assert_eq!((h.size_x, h.size_y, h.size_z), (p.size_x, p.size_y, p.size_z));
    assert_eq!(h.layer_height, s.layer_height);
    assert_eq!(h.exposure, n.exposure);
    assert_eq!(h.bottom_exposure, b.exposure);
    assert_eq!(h.bottom_layers, s.bottom_layers);
    assert_eq!(h.delay_mode_static, s.wait_mode);
    assert_eq!(h.turn_off_time, n.light_off_delay);
    assert_eq!(
        (h.bottom_before_lift, h.bottom_after_lift, h.bottom_after_retract),
        (b.wait_before_lift, b.wait_after_lift, b.wait_after_retract)
    );
    assert_eq!(
        (h.before_lift, h.after_lift, h.after_retract),
        (n.wait_before_lift, n.wait_after_lift, n.wait_after_retract)
    );
    assert_eq!(
        [
            h.bottom_lift_distance,
            h.bottom_lift_speed,
            h.bottom_lift_distance2,
            h.bottom_lift_speed2
        ],
        [b.lift.distance, b.lift.speed, b.lift.distance2, b.lift.speed2]
    );
    assert_eq!(
        [h.lift_distance, h.lift_speed, h.lift_distance2, h.lift_speed2],
        [n.lift.distance, n.lift.speed, n.lift.distance2, n.lift.speed2]
    );
    assert_eq!(
        [
            h.bottom_retract_distance,
            h.bottom_retract_speed,
            h.bottom_retract_distance2,
            h.bottom_retract_speed2
        ],
        [
            b.retract.distance,
            b.retract.speed,
            b.retract.distance2,
            b.retract.speed2
        ]
    );
    assert_eq!(
        [
            h.retract_distance,
            h.retract_speed,
            h.retract_distance2,
            h.retract_speed2
        ],
        [
            n.retract.distance,
            n.retract.speed,
            n.retract.distance2,
            n.retract.speed2
        ]
    );
    assert_eq!(
        (h.bottom_light_pwm, h.light_pwm),
        (b.light_pwm as u16, n.light_pwm as u16)
    );
    assert_eq!(h.print_time, est.print_time_s.round() as u32);
    assert_eq!(h.volume, est.volume_mm3 as f32);
    assert_eq!(h.weight, est.weight_g as f32);
    assert_eq!(h.price, est.cost as f32);
    assert_eq!(h.price_unit, s.currency);
    assert_eq!(h.layer_content_offset as usize, goo::HEADER_SIZE);
    assert!(h.grey_scale_8bit);
    assert_eq!(h.transition_layers as u32, s.transition_layers);
    let small = Thumbnail::from_rgb565(goo::SMALL_PREVIEW, goo::SMALL_PREVIEW, &h.small_preview);
    assert_preview(
        &small,
        thumb,
        (goo::SMALL_PREVIEW, goo::SMALL_PREVIEW),
        8,
        "GOO small preview",
    );

    // per-layer definitions and the file layout
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(g.layers.len() as u32, STANDARD_LAYERS);
    let mut expect_pos = goo::HEADER_SIZE as u64;
    for (i, (d, pos, size)) in g.layers.iter().enumerate() {
        let i = i as u32;
        let lp = if i < s.bottom_layers { b } else { n };
        assert!(!d.pause, "layer {i}");
        let z = (i + 1) as f32 * s.layer_height;
        assert_close(d.position_z, z, 1e-5, &format!("GOO layer {i} z"));
        assert_eq!(d.pause_z, d.position_z, "layer {i}");
        assert_eq!(d.exposure, s.exposure_for_layer(i), "layer {i} exposure");
        assert_eq!(d.off_time, n.light_off_delay, "layer {i}");
        assert_eq!(
            (d.before_lift, d.after_lift, d.after_retract),
            (lp.wait_before_lift, lp.wait_after_lift, lp.wait_after_retract),
            "layer {i}"
        );
        assert_eq!(
            [d.lift_distance, d.lift_speed, d.lift_distance2, d.lift_speed2],
            [lp.lift.distance, lp.lift.speed, lp.lift.distance2, lp.lift.speed2],
            "layer {i}"
        );
        assert_eq!(
            [
                d.retract_distance,
                d.retract_speed,
                d.retract_distance2,
                d.retract_speed2
            ],
            [
                lp.retract.distance,
                lp.retract.speed,
                lp.retract.distance2,
                lp.retract.speed2
            ],
            "layer {i}"
        );
        assert_eq!(d.light_pwm, lp.light_pwm as u16, "layer {i}");
        // definition (66 bytes) + u32 size, then the data and a CR LF
        assert_eq!(*pos, expect_pos + 70, "layer {i} position");
        let data = &bytes[*pos as usize..(*pos + *size as u64) as usize];
        assert_eq!(data[0], 0x55, "layer {i} magic");
        let sum = data[1..data.len() - 1].iter().fold(0u8, |a, &v| a.wrapping_add(v));
        assert_eq!(!sum, data[data.len() - 1], "layer {i} checksum");
        let end = (*pos + *size as u64) as usize;
        assert_eq!(&bytes[end..end + 2], &goo::DELIMITER, "layer {i} delimiter");
        expect_pos = end as u64 + 2;
    }
    assert_eq!(&bytes[expect_pos as usize..], &goo::ENDING, "GOO file ending");
}

// ---------------------------------------------------------------- CTB fields

fn ctb_details(path: &Path, p: &PrinterProfile, s: &PrintSettings, est: &PrintEstimate, thumb: &Thumbnail) {
    let f = std::fs::read(path).unwrap();
    let (b, n) = (&s.bottom, &s.normal);
    let layers = STANDARD_LAYERS;
    assert_eq!(le_u32(&f, 0), ctb::MAGIC_CTB);
    assert_eq!(le_u32(&f, 4), 3, "version");
    assert_eq!(
        [le_f32(&f, 8), le_f32(&f, 12), le_f32(&f, 16)],
        [p.size_x, p.size_y, p.size_z]
    );
    assert_close(le_f32(&f, 28), layers as f32 * s.layer_height, 1e-4, "total height");
    assert_eq!(le_f32(&f, 32), s.layer_height);
    assert_eq!(le_f32(&f, 36), n.exposure);
    assert_eq!(le_f32(&f, 40), b.exposure);
    assert_eq!(le_f32(&f, 44), n.light_off_delay);
    assert_eq!(le_u32(&f, 48), s.bottom_layers);
    assert_eq!((le_u32(&f, 52), le_u32(&f, 56)), (p.resolution_x, p.resolution_y));
    let large = le_u32(&f, 60) as usize;
    let table = le_u32(&f, 64) as usize;
    assert_eq!(le_u32(&f, 68), layers);
    let small = le_u32(&f, 72) as usize;
    assert_eq!(le_u32(&f, 76), est.print_time_s.round() as u32, "print time");
    assert_eq!(le_u32(&f, 80), p.mirror.is_mirrored() as u32, "projector type");
    let pp = le_u32(&f, 84) as usize;
    assert_eq!(le_u32(&f, 88), 60, "print parameter size");
    assert_eq!(le_u32(&f, 92), s.anti_aliasing as u32);
    assert_eq!(
        (le_u16(&f, 96), le_u16(&f, 98)),
        (n.light_pwm as u16, b.light_pwm as u16)
    );
    let seed = le_u32(&f, 100);
    assert_ne!(seed, 0, "layers must be encrypted with a non-zero seed");
    let slicer = le_u32(&f, 104) as usize;
    assert_eq!(le_u32(&f, 108), 76, "slicer info size");

    // previews: (width, height, data offset, data length) + 16 reserved bytes
    for (off, size) in [(large, (400u32, 300u32)), (small, (200, 125))] {
        assert!(off >= 112, "preview offset");
        assert_eq!((le_u32(&f, off), le_u32(&f, off + 4)), size);
        let (data, len) = (le_u32(&f, off + 8) as usize, le_u32(&f, off + 12) as usize);
        assert_eq!(data, off + 32);
        let t = ctb::decode_preview(size.0, size.1, &f[data..data + len]);
        assert_preview(&t, thumb, size, 8, "CTB preview");
    }

    // print parameters
    let total = |m: &openresin_core::profile::Motion| m.distance + m.distance2;
    assert_eq!(
        [
            le_f32(&f, pp),
            le_f32(&f, pp + 4),
            le_f32(&f, pp + 8),
            le_f32(&f, pp + 12),
            le_f32(&f, pp + 16)
        ],
        [
            total(&b.lift),
            b.lift.speed,
            total(&n.lift),
            n.lift.speed,
            n.retract.speed
        ]
    );
    assert_eq!(le_f32(&f, pp + 20), est.volume_ml() as f32, "volume");
    assert_eq!(le_f32(&f, pp + 24), est.weight_g as f32, "weight");
    assert_eq!(le_f32(&f, pp + 28), est.cost as f32, "cost");
    assert_eq!(
        (le_f32(&f, pp + 32), le_f32(&f, pp + 36)),
        (b.light_off_delay, n.light_off_delay)
    );
    assert_eq!(le_u32(&f, pp + 40), s.bottom_layers);

    // slicer info
    assert_eq!(
        [
            le_f32(&f, slicer),
            le_f32(&f, slicer + 4),
            le_f32(&f, slicer + 8),
            le_f32(&f, slicer + 12),
            le_f32(&f, slicer + 16),
            le_f32(&f, slicer + 20),
        ],
        [
            b.lift.distance2,
            b.lift.speed2,
            n.lift.distance2,
            n.lift.speed2,
            n.retract.distance2,
            n.retract.speed2
        ]
    );
    assert_eq!(le_f32(&f, slicer + 24), n.wait_after_lift);
    let (name_at, name_len) = (le_u32(&f, slicer + 28) as usize, le_u32(&f, slicer + 32) as usize);
    assert_eq!(
        String::from_utf8_lossy(&f[name_at..name_at + name_len]),
        p.machine_type()
    );
    assert_eq!(le_u32(&f, slicer + 44), s.anti_aliasing as u32);
    assert_eq!(le_f32(&f, slicer + 52), n.wait_after_retract);
    assert_eq!(le_u32(&f, slicer + 60), s.transition_layers);

    // layer table (36 bytes each) and extended definitions in front of the data
    let mut prev_end = table + 36 * layers as usize;
    for i in 0..layers {
        let e = table + 36 * i as usize;
        let lp = if i < s.bottom_layers { b } else { n };
        assert_close(
            le_f32(&f, e),
            (i + 1) as f32 * s.layer_height,
            1e-5,
            &format!("CTB layer {i} z"),
        );
        assert_eq!(le_f32(&f, e + 4), s.exposure_for_layer(i), "layer {i} exposure");
        let (addr, size) = (le_u32(&f, e + 12) as usize, le_u32(&f, e + 16) as usize);
        assert_eq!(le_u32(&f, e + 24), 84, "layer {i} table size");
        let ex = addr - 84;
        assert_eq!(ex, prev_end, "layer {i}: layers are stored in order without gaps");
        assert_eq!(
            &f[ex..ex + 36],
            &f[e..e + 36],
            "layer {i}: extended definition repeats the entry"
        );
        assert_eq!(le_u32(&f, ex + 36) as usize, 84 + size, "layer {i} total size");
        let got: Vec<f32> = (0..11).map(|k| le_f32(&f, ex + 40 + 4 * k)).collect();
        assert_eq!(
            got,
            vec![
                total(&lp.lift),
                lp.lift.speed,
                lp.lift.distance2,
                lp.lift.speed2,
                lp.retract.speed,
                lp.retract.distance2,
                lp.retract.speed2,
                lp.wait_before_lift,
                lp.wait_after_lift,
                lp.wait_after_retract,
                lp.light_pwm as f32
            ],
            "layer {i} motion"
        );
        prev_end = addr + size;
    }
    assert_eq!(prev_end, f.len(), "CTB ends with the last layer");
}

// ---------------------------------------------------------------- zip / SL1 fields

fn zip_entries(path: &Path) -> (Vec<String>, zip::ZipArchive<std::fs::File>) {
    let zip = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let names = zip.file_names().map(str::to_string).collect();
    (names, zip)
}

fn zip_text(zip: &mut zip::ZipArchive<std::fs::File>, name: &str) -> String {
    let mut s = String::new();
    zip.by_name(name).unwrap().read_to_string(&mut s).unwrap();
    s
}

fn zip_png(zip: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Thumbnail {
    let mut d = Vec::new();
    zip.by_name(name).unwrap().read_to_end(&mut d).unwrap();
    Thumbnail::from_png(&d).unwrap()
}

fn kv(text: &str, sep: char) -> std::collections::HashMap<String, String> {
    text.lines()
        .filter_map(|l| {
            let (k, v) = l.trim_start_matches(';').split_once(sep)?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

fn zip_details(path: &Path, p: &PrinterProfile, s: &PrintSettings, est: &PrintEstimate, thumb: &Thumbnail) {
    let (names, mut zip) = zip_entries(path);
    let n = STANDARD_LAYERS;
    for i in 1..=n {
        assert!(names.contains(&format!("{i}.png")), "missing {i}.png");
    }
    assert_eq!(names.len() as u32, n + 3, "{names:?}");
    assert_preview(&zip_png(&mut zip, "preview.png"), thumb, (400, 300), 0, "preview.png");
    assert_preview(
        &zip_png(&mut zip, "preview_cropping.png"),
        thumb,
        (200, 125),
        0,
        "preview_cropping.png",
    );
    let g = zip_text(&mut zip, "run.gcode");
    assert!(!g.contains("{MOTION") && !g.contains("{PWM"), "unexpanded placeholders");
    let k = kv(&g, ':');
    let num = |key: &str| -> f64 { k.get(key).unwrap_or_else(|| panic!("missing {key}")).parse().unwrap() };
    let (b, nl) = (&s.bottom, &s.normal);
    assert_eq!(k["fileName"], "test job");
    assert_eq!(k["machineType"], p.machine_type());
    assert_eq!(k["resin"], s.name);
    assert_eq!(num("totalLayer") as u32, n);
    assert_eq!(
        (num("resolutionX") as u32, num("resolutionY") as u32),
        (p.resolution_x, p.resolution_y)
    );
    for (key, want) in [
        ("machineX", p.size_x),
        ("machineY", p.size_y),
        ("machineZ", p.size_z),
        ("layerHeight", s.layer_height),
        ("normalExposureTime", nl.exposure),
        ("bottomLayExposureTime", b.exposure),
        ("bottomLayerExposureTime", b.exposure),
        ("normalDropSpeed", nl.retract.speed),
        ("normalLayerLiftHeight", nl.lift.distance + nl.lift.distance2),
        ("normalLayerLiftSpeed", nl.lift.speed),
        ("bottomLayerLiftHeight", b.lift.distance + b.lift.distance2),
        ("bottomLayerLiftSpeed", b.lift.speed),
        ("bottomLightOffTime", b.light_off_delay),
        ("lightOffTime", nl.light_off_delay),
        ("bottomPWMLight", b.light_pwm as f32),
        ("normalPWMLight", nl.light_pwm as f32),
    ] {
        assert_close(num(key) as f32, want, 1e-5, key);
    }
    assert_eq!(num("bottomLayCount") as u32, s.bottom_layers);
    assert_eq!(num("bottomLayerCount") as u32, s.bottom_layers);
    assert_eq!(num("antiAliasLevel") as u8, s.anti_aliasing);
    assert_eq!(num("mirror") != 0.0, p.mirror.is_mirrored());
    assert_eq!(
        k["projectType"],
        if p.mirror.is_mirrored() { "mirror_LCD" } else { "normal" }
    );
    assert!((num("estimatedPrintTime") - est.print_time_s).abs() <= 0.5 + 1e-9);
    assert!((num("volume") - est.volume_ml()).abs() <= 5e-4 + 1e-9);
    assert!((num("weight") - est.weight_g).abs() <= 5e-4 + 1e-9);
    assert!((num("price") - est.cost).abs() <= 5e-4 + 1e-9);

    // one block per layer, in order, with the layer's own motion and exposure
    let blocks: Vec<&str> = g.split(";LAYER_START:").skip(1).collect();
    assert_eq!(blocks.len() as u32, n);
    for (i, blk) in blocks.iter().enumerate() {
        let i = i as u32;
        let lp = if i < s.bottom_layers { b } else { nl };
        let z = (i + 1) as f32 * s.layer_height;
        let lines: Vec<&str> = blk.lines().collect();
        assert_eq!(lines[0], i.to_string());
        assert_eq!(lines[1], format!(";currPos:{z:.4}"));
        assert_eq!(lines[2], format!("M6054 \"{}.png\";show Image", i + 1));
        assert_eq!(
            lines[3],
            format!("G0 Z{:.4} F{};", z + lp.lift.total_distance(), lp.lift.speed)
        );
        assert_eq!(lines[4], format!("G0 Z{z:.4} F{};", lp.retract.speed));
        let rest = if s.wait_mode {
            lp.wait_after_retract
        } else {
            lp.light_off_delay
        };
        assert_eq!(lines[5], format!("G4 P{};", (rest * 1000.0).round() as u32));
        assert_eq!(lines[6], format!("M106 S{};light on", lp.light_pwm));
        assert_eq!(
            lines[7],
            format!("G4 P{};", (s.exposure_for_layer(i) * 1000.0).round() as u32)
        );
        assert_eq!(lines[8], "M106 S0;light off");
        assert_eq!(lines[9], ";LAYER_END");
    }
}

fn sl1_details(path: &Path, p: &PrinterProfile, s: &PrintSettings, est: &PrintEstimate, thumb: &Thumbnail) {
    let (names, mut zip) = zip_entries(path);
    let n = STANDARD_LAYERS;
    for i in 0..n {
        assert!(
            names.contains(&format!("test_job{i:05}.png")),
            "missing layer {i} in {names:?}"
        );
    }
    for (w, h) in [(400, 400), (800, 480)] {
        let name = format!("thumbnail/thumbnail{w}x{h}.png");
        assert_preview(&zip_png(&mut zip, &name), thumb, (w, h), 0, &name);
    }
    assert_eq!(names.len() as u32, n + 4, "{names:?}");
    let c = kv(&zip_text(&mut zip, "config.ini"), '=');
    assert_eq!(c["action"], "print");
    assert_eq!(c["jobDir"], "test_job");
    assert_eq!(c["materialName"], s.name);
    assert_eq!(c["printerModel"], p.machine_type());
    let num = |m: &std::collections::HashMap<String, String>, k: &str| -> f64 { m[k].parse().unwrap() };
    assert_close(num(&c, "expTime") as f32, s.normal.exposure, 1e-6, "expTime");
    assert_close(num(&c, "expTimeFirst") as f32, s.bottom.exposure, 1e-6, "expTimeFirst");
    assert_close(num(&c, "layerHeight") as f32, s.layer_height, 1e-6, "layerHeight");
    assert_eq!(num(&c, "numFade") as u32, s.bottom_layers);
    assert_eq!(num(&c, "numFast") as u32 + num(&c, "numSlow") as u32, n);
    assert!((num(&c, "printTime") - est.print_time_s).abs() <= 0.5 + 1e-9);
    assert!((num(&c, "usedMaterial") - est.volume_ml()).abs() <= 5e-4 + 1e-9);
    let pi = kv(&zip_text(&mut zip, "prusaslicer.ini"), '=');
    assert_close(num(&pi, "display_width") as f32, p.size_x, 1e-5, "display_width");
    assert_close(num(&pi, "display_height") as f32, p.size_y, 1e-5, "display_height");
    assert_eq!(num(&pi, "display_pixels_x") as u32, p.resolution_x);
    assert_eq!(num(&pi, "display_pixels_y") as u32, p.resolution_y);
    assert_close(num(&pi, "max_print_height") as f32, p.size_z, 1e-5, "max_print_height");
}

// ---------------------------------------------------------------- one test per format

macro_rules! format_cases {
    ($($(#[$attr:meta])* $name:ident => $format:expr;)*) => {
        $(
            $(#[$attr])*
            #[test]
            fn $name() {
                let format = $format;
                roundtrip(format, &profile_for(format));
            }
        )*
        /// Formats with a dedicated test above.
        const CASES: &[Format] = &[$($format),*];
    };
}

use AnycubicModel as M;

format_cases! {
    goo => Format::Goo;
    ctb => Format::Ctb;
    sl1 => Format::Sl1;
    png_zip => Format::Zip;
    cxdlp => Format::Cxdlp;
    cxdlpv4 => Format::Cxdlpv4;
    pwmx => Format::Anycubic(M::Pwmx);
    pwma => Format::Anycubic(M::Pwma);
    pwmb => Format::Anycubic(M::Pwmb);
    pwmo => Format::Anycubic(M::Pwmo);
    pwms => Format::Anycubic(M::Pwms);
    pmsq => Format::Anycubic(M::Pmsq);
    pmx2 => Format::Anycubic(M::Pmx2);
    px6s => Format::Anycubic(M::Px6s);
    pm3 => Format::Anycubic(M::Pm3);
    pm3m => Format::Anycubic(M::Pm3m);
    pm3r => Format::Anycubic(M::Pm3r);
    pm3n => Format::Anycubic(M::Pm3n);
    pm4n => Format::Anycubic(M::Pm4n);
    pm5 => Format::Anycubic(M::Pm5);
    pm5s => Format::Anycubic(M::Pm5s);
    m5sp => Format::Anycubic(M::M5sp);
    dlp => Format::Anycubic(M::Dlp);
    dl2p => Format::Anycubic(M::Dl2p);
    pwx => Format::Anycubic(M::Pwx);
    pw0 => Format::Anycubic(M::Pw0);
    pws => Format::Anycubic(M::Pws);
}

#[test]
fn every_format_has_a_dedicated_case() {
    for f in Format::ALL {
        assert!(
            CASES.contains(&f),
            "{f:?} has no round trip test; add it to format_cases!"
        );
    }
    assert_eq!(CASES.len(), Format::ALL.len());
}

/// Every format (including ones added later) on a small printer with
/// non-square pixels and the standard LCD mirroring.
#[test]
fn every_format_roundtrips_on_a_small_printer() {
    let p = small_printer(MirrorMode::Lcd);
    for f in Format::ALL {
        roundtrip(f, &p);
    }
}

/// Every format at the other anti-aliasing levels (quantisation depends on the level).
#[test]
fn every_format_at_every_anti_aliasing_level() {
    let p = small_printer(MirrorMode::Lcd);
    for aa in [1u8, 2, 8, 16] {
        let s = PrintSettings {
            anti_aliasing: aa,
            ..test_settings()
        };
        for f in Format::ALL {
            roundtrip_with(f, &p, &s);
        }
    }
}

/// Light-off delay instead of rest times, a single bottom layer and no
/// transition layers.
#[test]
fn every_format_with_light_off_delay_and_one_bottom_layer() {
    let p = small_printer(MirrorMode::None);
    let s = PrintSettings {
        wait_mode: false,
        bottom_layers: 1,
        transition_layers: 0,
        ..test_settings()
    };
    for f in Format::ALL {
        roundtrip_with(f, &p, &s);
    }
}

/// The Photon Mono X 6K shares `.pwmb` with the M3 Plus but uses file version 516.
#[test]
fn pwmb_for_mono_x_6k() {
    let list = builtin_printers();
    let p = find_printer(&list, "anycubic-photon-mono-x-6k").unwrap().clone();
    assert_eq!(AnycubicModel::Pwmb.version_for(&p), anycubic_v516());
    roundtrip(Format::Anycubic(M::Pwmb), &p);
}

/// HALOT-X1 class printers use the pixel pair codec in CXDLPv4 files.
#[test]
fn cxdlpv4_pixel_pair_codec() {
    let list = builtin_printers();
    let mut p = find_printer(&list, "creality-halot-x1").unwrap().clone();
    assert_eq!(
        openresin_formats::cxdlpv4::codec_for(&p),
        openresin_formats::cxdlpv4::V4Codec::PixelPair
    );
    // a quarter of the resolution keeps the test fast; the machine type keeps the codec
    p.resolution_x /= 4;
    p.resolution_y = (p.resolution_y / 4) & !1;
    p.machine_type = Some("HALOT-X1".into());
    assert_eq!(
        openresin_formats::cxdlpv4::codec_for(&p),
        openresin_formats::cxdlpv4::V4Codec::PixelPair
    );
    roundtrip(Format::Cxdlpv4, &p);
}

/// GOO and CTB on printers with LCD mirroring (the smallest GOO printer has none).
#[test]
fn goo_and_ctb_on_mirrored_printers() {
    let list = builtin_printers();
    let goo_printer = find_printer(&list, "elegoo-mars-5").unwrap().clone();
    assert_eq!(goo_printer.mirror, MirrorMode::Lcd);
    roundtrip(Format::Goo, &goo_printer);
    let mut ctb_printer = profile_for(Format::Ctb);
    ctb_printer.mirror = MirrorMode::None;
    roundtrip(Format::Ctb, &ctb_printer);
}

/// Exports without a thumbnail still decode; the formats OpenResin owns report no preview.
#[test]
fn export_without_thumbnail() {
    let p = small_printer(MirrorMode::Lcd);
    let s = test_settings();
    let scene = Scene::standard();
    let slicer = scene.slicer(&p, &s);
    for f in Format::ALL {
        let dir = TempDir::new("nothumb");
        let ex = export(&dir, "nothumb", f, &p, &s, &scene, None);
        let file = open(&ex.path).unwrap();
        assert_eq!(file.info().layer_count, STANDARD_LAYERS, "{f:?}");
        let q = quantizer(f, &p, s.anti_aliasing);
        for i in [0u32, 30, 59] {
            assert_layer_matches(
                &slicer.rasterize(i as usize),
                &file.layer(i).unwrap(),
                &*q,
                &format!("{f:?} {i}"),
            );
        }
        let t = file.thumbnail();
        match f {
            Format::Goo | Format::Ctb | Format::Zip | Format::Sl1 => assert!(t.is_none(), "{f:?}"),
            // fixed size previews are written black
            _ => {
                if let Some(t) = t {
                    assert!(model_fraction(&t) == 0.0, "{f:?}: preview should be empty");
                }
            }
        }
    }
}
