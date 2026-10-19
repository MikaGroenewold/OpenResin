//! Field-level checks of the vendor containers (Photon Workshop, CXDLP,
//! CXDLPv4) through their public reader structs: print parameters, per-layer
//! records and every stored preview.
//!
//! Kept separate from `all_formats_roundtrip.rs` because it depends on the
//! public fields of `anycubic`, `cxdlp` and `cxdlpv4`.

#[path = "formats_helpers.rs"]
mod helpers;

use helpers::*;
use openresin_core::profile::{PrintSettings, PrinterProfile};
use openresin_formats::anycubic::{self, AnycubicFile, AnycubicModel, LayerCodec};
use openresin_formats::cxdlp::{self, CxdlpFile};
use openresin_formats::cxdlpv4::{self, CxdlpV4File};
use openresin_formats::preview::Thumbnail;
use openresin_formats::{ctb, open, ExportResult, Format};

struct Case {
    _dir: TempDir,
    path: std::path::PathBuf,
    printer: PrinterProfile,
    settings: PrintSettings,
    result: ExportResult,
    thumb: Thumbnail,
    scene: Scene,
}

fn case(format: Format) -> Case {
    case_on(format, profile_for(format))
}

fn case_on(format: Format, printer: PrinterProfile) -> Case {
    let dir = TempDir::new(&format!("vendor-{}", format.extension()));
    let settings = test_settings();
    let scene = Scene::standard();
    let thumb = scene.thumbnail();
    let ex = export(&dir, "scene", format, &printer, &settings, &scene, Some(&thumb));
    Case {
        _dir: dir,
        path: ex.path,
        printer,
        settings,
        result: ex.result,
        thumb,
        scene,
    }
}

fn rgb565_le(w: u32, h: u32, data: &[u8]) -> Thumbnail {
    let px: Vec<u16> = data.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    Thumbnail::from_rgb565(w, h, &px)
}

fn rgb565_be(w: u32, h: u32, data: &[u8]) -> Thumbnail {
    let px: Vec<u16> = data.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
    Thumbnail::from_rgb565(w, h, &px)
}

// ---------------------------------------------------------------- Photon Workshop

#[test]
fn photon_workshop_headers_layers_and_previews() {
    for m in AnycubicModel::ALL {
        let f = Format::Anycubic(m);
        let c = case(f);
        let (s, p, est) = (&c.settings, &c.printer, &c.result.estimate);
        let ctx = format!("{f:?} on {}", p.id);
        let file = AnycubicFile::open(&c.path, m).unwrap_or_else(|e| panic!("{ctx}: {e}"));
        let version = m.version_for(p);
        assert_eq!(file.mark.version, version, "{ctx}: version");
        assert_eq!(file.codec, m.codec(), "{ctx}: codec");

        let h = &file.header;
        let (px, py) = p.pixel_size_um();
        assert_close(h.pixel_size_um, px.max(py), 1e-3, &format!("{ctx}: pixel size"));
        assert_eq!(
            (h.resolution_x, h.resolution_y),
            (p.resolution_x, p.resolution_y),
            "{ctx}"
        );
        assert_eq!(h.layer_height, s.layer_height, "{ctx}");
        assert_eq!(h.exposure, s.normal.exposure, "{ctx}");
        assert_eq!(h.bottom_exposure, s.bottom.exposure, "{ctx}");
        assert_eq!(h.bottom_layers, s.bottom_layers as f32, "{ctx}");
        assert_eq!(h.anti_aliasing, s.anti_aliasing as u32, "{ctx}");
        // lift height is the total of both stages, speeds are stored in mm/s
        assert_close(
            h.lift_height,
            s.normal.lift.total_distance(),
            1e-6,
            &format!("{ctx}: lift"),
        );
        assert_close(
            h.lift_speed,
            s.normal.lift.speed / 60.0,
            1e-5,
            &format!("{ctx}: lift speed"),
        );
        assert_close(
            h.retract_speed,
            s.normal.retract.speed / 60.0,
            1e-5,
            &format!("{ctx}: retract speed"),
        );
        assert_eq!(h.print_time_s, est.print_time_s.round() as u32, "{ctx}");
        assert_close(h.volume_ml, est.volume_ml() as f32, 1e-5, &format!("{ctx}: volume"));
        assert_close(h.weight_g, est.weight_g as f32, 1e-5, &format!("{ctx}: weight"));
        assert_close(h.price, est.cost as f32, 1e-6, &format!("{ctx}: price"));
        assert_eq!(h.transition_layers, s.transition_layers, "{ctx}");

        // layer records
        let slicer = c.scene.slicer(p, s);
        let q = quantizer(f, p, s.anti_aliasing);
        assert_eq!(file.layers.len() as u32, STANDARD_LAYERS, "{ctx}");
        let mut end = 0u64;
        for (i, d) in file.layers.iter().enumerate() {
            let i = i as u32;
            let lp = if i < s.bottom_layers { &s.bottom } else { &s.normal };
            assert_eq!(d.exposure, s.exposure_for_layer(i), "{ctx}: layer {i} exposure");
            assert_close(
                d.layer_height,
                s.layer_height,
                1e-6,
                &format!("{ctx}: layer {i} thickness"),
            );
            assert_close(
                d.lift_height,
                lp.lift.total_distance(),
                1e-6,
                &format!("{ctx}: layer {i} lift"),
            );
            assert_close(
                d.lift_speed,
                lp.lift.speed / 60.0,
                1e-5,
                &format!("{ctx}: layer {i} lift speed"),
            );
            let lit: u64 = slicer
                .rasterize(i as usize)
                .runs
                .iter()
                .filter(|r| q(r.value) != 0)
                .map(|r| r.len as u64)
                .sum();
            assert_eq!(d.non_zero_pixels as u64, lit, "{ctx}: layer {i} non-zero pixels");
            assert!(
                d.data_address as u64 >= end,
                "{ctx}: layer {i} overlaps the previous one"
            );
            end = d.data_address as u64 + d.data_length as u64;
        }
        assert_eq!(
            end,
            std::fs::metadata(&c.path).unwrap().len(),
            "{ctx}: file ends with the last layer"
        );

        // previews: section head (12 + 4), width, 4 bytes, height, RGB565 pixels
        let bytes = std::fs::read(&c.path).unwrap();
        let mut previews = vec![(file.mark.preview, "PREVIEW", anycubic::PREVIEW_SIZE)];
        if version >= anycubic::VERSION_518 {
            previews.push((file.mark.preview2, "PREVIEW2", anycubic::PREVIEW2_SIZE));
        } else {
            assert_eq!(file.mark.preview2, 0, "{ctx}");
        }
        for (addr, name, (w, hh)) in previews {
            let a = addr as usize;
            assert!(a > 0, "{ctx}: {name} missing");
            assert_eq!(fixed_str(&bytes[a..a + 12]), name, "{ctx}");
            assert_eq!(
                (le_u32(&bytes, a + 16), le_u32(&bytes, a + 24)),
                (w, hh),
                "{ctx}: {name} size"
            );
            let data = &bytes[a + 28..a + 28 + (w * hh * 2) as usize];
            assert_preview(&rgb565_le(w, hh, data), &c.thumb, (w, hh), 8, &format!("{ctx}: {name}"));
        }
        // MACHINE (v516+) names the layer codec
        if version >= anycubic::VERSION_516 {
            let a = file.mark.machine as usize;
            assert_eq!(fixed_str(&bytes[a..a + 12]), "MACHINE", "{ctx}");
            let codec = fixed_str(&bytes[a + 16 + 96..a + 16 + 112]);
            let want = match m.codec() {
                LayerCodec::Pw0 => "pw0Img",
                LayerCodec::Pws => "pwsImg",
            };
            assert_eq!(codec, want, "{ctx}");
        }
    }
}

// ---------------------------------------------------------------- CXDLP

#[test]
fn cxdlp_parameters_and_previews() {
    let c = case(Format::Cxdlp);
    let (s, p) = (&c.settings, &c.printer);
    let file = CxdlpFile::open(&c.path).unwrap();
    assert_eq!(file.version, cxdlp::VERSION);
    assert_eq!(file.model, cxdlp::model_code(p));
    assert_eq!(file.model, "CL-60", "HALOT-ONE model code");
    assert!(file.software.starts_with("OpenResin"), "{}", file.software);
    assert_eq!(file.material, s.name);
    // integers: exposure in 0.1 s; bottom exposure, rests and lift heights in
    // whole seconds/mm (rests and heights at least 1); speeds in whole mm/s (at least 1)
    let whole = |v: f32| v.round() as u16;
    let speed = |mm_min: f32| whole(mm_min / 60.0).max(1);
    let pr = &file.params;
    assert_eq!(
        pr.display_width.parse::<f32>().unwrap(),
        (p.size_x * 100.0).round() / 100.0
    );
    assert_eq!(
        pr.display_height.parse::<f32>().unwrap(),
        (p.size_y * 100.0).round() / 100.0
    );
    assert_eq!(pr.layer_height.parse::<f32>().unwrap(), s.layer_height);
    assert_eq!(pr.exposure, whole(s.normal.exposure * 10.0));
    assert_eq!(pr.wait_before_cure, whole(s.normal.wait_after_retract).max(1));
    assert_eq!(pr.bottom_exposure, whole(s.bottom.exposure));
    assert_eq!(pr.bottom_layers as u32, s.bottom_layers);
    assert_eq!(pr.bottom_lift_height, whole(s.bottom.lift.total_distance()).max(1));
    assert_eq!(pr.bottom_lift_speed, speed(s.bottom.lift.speed));
    assert_eq!(pr.lift_height, whole(s.normal.lift.total_distance()).max(1));
    assert_eq!(pr.lift_speed, speed(s.normal.lift.speed));
    assert_eq!(pr.retract_speed, speed(s.normal.retract.speed));
    assert_eq!(
        (pr.bottom_light_pwm, pr.light_pwm),
        (s.bottom.light_pwm as u16, s.normal.light_pwm as u16)
    );

    // three big-endian RGB565 previews after the fixed header, each followed by CR LF
    let b = std::fs::read(&c.path).unwrap();
    assert_eq!(be_u32(&b, 0), 9);
    assert_eq!(&b[4..13], cxdlp::MAGIC);
    assert_eq!(be_u16(&b, 13), cxdlp::VERSION);
    let model_len = be_u32(&b, 15) as usize;
    let mut at = 19 + model_len;
    assert_eq!(be_u16(&b, at) as u32, STANDARD_LAYERS);
    assert_eq!(
        (be_u16(&b, at + 2) as u32, be_u16(&b, at + 4) as u32),
        (p.resolution_x, p.resolution_y)
    );
    at += 6 + 64;
    for (w, h) in cxdlp::PREVIEWS {
        let n = (w * h * 2) as usize;
        assert_preview(
            &rgb565_be(w, h, &b[at..at + n]),
            &c.thumb,
            (w, h),
            8,
            &format!("CXDLP {w}x{h}"),
        );
        assert_eq!(&b[at + n..at + n + 2], b"\r\n");
        at += n + 2;
    }
    // footer: magic again, then the checksum
    let n = b.len();
    assert_eq!(&b[n - 4 - 9..n - 4], cxdlp::MAGIC);
    assert_eq!(be_u32(&b, n - 17), 9);
}

// ---------------------------------------------------------------- CXDLPv4

#[test]
fn cxdlpv4_header_parameters_layers_and_previews() {
    let list = openresin_core::profile::builtin_printers();
    for printer in ["creality-halot-r6", "creality-halot-mage"] {
        let p = openresin_core::profile::find_printer(&list, printer).unwrap().clone();
        let c = case_on(Format::Cxdlpv4, p);
        let (s, p, est) = (&c.settings, &c.printer, &c.result.estimate);
        let ctx = format!("CXDLPv4 on {}", p.id);
        let file = CxdlpV4File::open(&c.path).unwrap();
        let h = &file.header;
        assert_eq!(h.version, cxdlpv4::VERSION, "{ctx}");
        // the model string is chosen by the writer (under review against the reference
        // implementation); it must be present and reported by the reader
        assert!(!h.model.is_empty(), "{ctx}");
        assert_eq!(open(&c.path).unwrap().info().machine_name, h.model, "{ctx}");
        assert_eq!(file.codec, cxdlpv4::codec_for(p), "{ctx}");
        assert_eq!(
            (h.resolution_x as u32, h.resolution_y as u32),
            (p.resolution_x, p.resolution_y),
            "{ctx}"
        );
        assert_eq!((h.bed_x, h.bed_y, h.bed_z), (p.size_x, p.size_y, p.size_z), "{ctx}");
        assert_close(h.print_height, STANDARD_LAYERS as f32 * s.layer_height, 1e-4, &ctx);
        assert_eq!(h.layer_height, s.layer_height, "{ctx}");
        assert_eq!(h.bottom_layers, s.bottom_layers, "{ctx}");
        assert_eq!(h.layer_count, STANDARD_LAYERS, "{ctx}");
        assert_eq!(h.print_time_s, est.print_time_s.round() as u32, "{ctx}");
        assert_eq!(h.projector_type, p.mirror.is_mirrored() as u32, "{ctx}");
        assert_eq!(h.anti_aliasing, s.anti_aliasing as u32, "{ctx}");
        assert_eq!(
            (h.light_pwm, h.bottom_light_pwm),
            (s.normal.light_pwm as u16, s.bottom.light_pwm as u16)
        );

        let b = std::fs::read(&c.path).unwrap();
        // print parameters (CTB-like, speeds in mm/min)
        let pp = h.print_params as usize;
        let got: Vec<f32> = (0..5).map(|k| le_f32(&b, pp + 4 * k)).collect();
        assert_eq!(
            got,
            vec![
                s.bottom.lift.total_distance(),
                s.bottom.lift.speed,
                s.normal.lift.total_distance(),
                s.normal.lift.speed,
                s.normal.retract.speed
            ],
            "{ctx}: motion"
        );
        assert_close(
            le_f32(&b, pp + 20),
            est.volume_ml() as f32,
            1e-6,
            &format!("{ctx}: volume"),
        );
        assert_close(
            le_f32(&b, pp + 24),
            est.weight_g as f32,
            1e-6,
            &format!("{ctx}: weight"),
        );
        assert_close(le_f32(&b, pp + 28), est.cost as f32, 1e-7, &format!("{ctx}: cost"));
        assert_eq!(le_u32(&b, pp + 40), s.bottom_layers, "{ctx}");
        assert_eq!(
            (le_f32(&b, pp + 44), le_f32(&b, pp + 48)),
            (s.normal.exposure, s.bottom.exposure),
            "{ctx}"
        );

        // previews: (width, height, data offset, length) + 16 reserved bytes, CTB RLE data
        for (addr, size) in [
            (h.preview_small, cxdlpv4::PREVIEW_SMALL),
            (h.preview_large, cxdlpv4::PREVIEW_LARGE),
        ] {
            let a = addr as usize;
            assert_eq!((le_u32(&b, a), le_u32(&b, a + 4)), size, "{ctx}");
            let (off, len) = (le_u32(&b, a + 8) as usize, le_u32(&b, a + 12) as usize);
            let t = ctb::decode_preview(size.0, size.1, &b[off..off + len]);
            assert_preview(&t, &c.thumb, size, 8, &format!("{ctx}: preview {size:?}"));
        }

        // layer records
        assert_eq!(file.layers.len() as u32, STANDARD_LAYERS, "{ctx}");
        for (i, d) in file.layers.iter().enumerate() {
            let i = i as u32;
            assert_close(
                d.position_z,
                (i + 1) as f32 * s.layer_height,
                1e-5,
                &format!("{ctx}: layer {i} z"),
            );
            assert_eq!(d.exposure, s.exposure_for_layer(i), "{ctx}: layer {i} exposure");
            assert_eq!(d.data_type, 0, "{ctx}: layer {i}");
            let area = (c.result.layer_areas[i as usize] * 1000.0).round() as u32;
            assert_eq!(d.area, area, "{ctx}: layer {i} area");
        }
        assert!(open(&c.path).is_ok());
    }
}
