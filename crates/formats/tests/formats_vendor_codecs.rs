//! Layer codecs of the vendor formats in isolation (Photon Workshop PW0 and
//! bit planes, CXDLP vertical lines, CXDLPv4 pixel pairs): edge cases and
//! random images must round-trip within each codec's documented quantisation.

#[path = "formats_helpers.rs"]
mod helpers;

use helpers::*;
use openresin_core::slicer::{LayerRaster, Run};
use openresin_formats::{anycubic, cxdlp, cxdlpv4};

const W: u32 = 98; // even: the pixel pair codec needs an even width
const H: u32 = 61;

fn images() -> Vec<(String, Vec<u8>)> {
    let n = (W * H) as usize;
    let mut v: Vec<(String, Vec<u8>)> = vec![
        ("black".into(), vec![0; n]),
        ("white".into(), vec![255; n]),
        (
            "alternating".into(),
            (0..n).map(|i| if i % 2 == 0 { 255 } else { 0 }).collect(),
        ),
        ("all levels".into(), (0..n).map(|i| (i % 256) as u8).collect()),
        (
            "columns".into(),
            (0..n)
                .map(|i| if (i % W as usize) % 3 == 0 { 200 } else { 0 })
                .collect(),
        ),
    ];
    for at in [0usize, n - 1, n / 2] {
        let mut img = vec![0u8; n];
        img[at] = 255;
        v.push((format!("pixel {at}"), img));
    }
    let mut rng = Rng::new(0xAC);
    for k in 0..60 {
        let mut img = Vec::with_capacity(n);
        while img.len() < n {
            let value = match rng.below(3) {
                0 => 0,
                1 => 255,
                _ => rng.u8(),
            };
            let len = (rng.below(2 * W as u64) + 1) as usize;
            img.extend(std::iter::repeat_n(value, len.min(n - img.len())));
        }
        v.push((format!("random {k}"), img));
    }
    v
}

type Quantizer = Box<dyn Fn(u8) -> u8>;

fn quantised(img: &[u8], q: &dyn Fn(u8) -> u8) -> Vec<u8> {
    img.iter().map(|&v| q(v)).collect()
}

#[test]
fn quantisers_are_monotonic_idempotent_and_bounded() {
    let mut cases: Vec<(String, Quantizer, u8)> = vec![
        ("pw0".into(), Box::new(anycubic::quantize_pw0), 8),
        ("pixel pair".into(), Box::new(cxdlpv4::quantize_pixel_pair), 18),
    ];
    // aa 16: see planes_aa16_quantisation_is_idempotent
    for aa in [1u8, 2, 4, 8] {
        cases.push((
            format!("planes aa {aa}"),
            Box::new(move |v| anycubic::quantize_planes(v, aa)),
            (256 / aa as u32).min(255) as u8,
        ));
    }
    for (name, q, bound) in cases {
        assert_eq!(q(0), 0, "{name}");
        assert_eq!(q(255), 255, "{name}");
        let mut prev = 0;
        for v in 0..=255u8 {
            let x = q(v);
            assert!(x >= prev, "{name}: not monotonic at {v}");
            assert_eq!(q(x), x, "{name}: not idempotent at {v}");
            assert!(x.abs_diff(v) <= bound, "{name}: {v} -> {x} (bound {bound})");
            prev = x;
        }
    }
}

#[test]
fn planes_aa16_is_monotonic_and_bounded() {
    let mut prev = 0;
    for v in 0..=255u8 {
        let x = anycubic::quantize_planes(v, 16);
        assert!(x >= prev && x.abs_diff(v) <= 16, "{v} -> {x}");
        prev = x;
    }
}

/// Re-saving a decoded image must not change it again.
#[test]
fn planes_aa16_quantisation_is_idempotent() {
    for v in 0..=255u8 {
        let x = anycubic::quantize_planes(v, 16);
        assert_eq!(anycubic::quantize_planes(x, 16), x, "{v} -> {x}");
    }
}

#[test]
fn pw0_roundtrips_within_16_levels() {
    for (name, img) in images() {
        let r = LayerRaster::from_gray(W, H, &img);
        let dec = anycubic::decode_pw0(&anycubic::encode_pw0(&r.runs), (W * H) as u64)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let got = LayerRaster::from_runs(W, H, dec).to_gray();
        assert_eq!(got, quantised(&img, &anycubic::quantize_pw0), "{name}");
    }
}

#[test]
fn bit_planes_roundtrip_for_every_aa_level() {
    for aa in [1u8, 2, 4, 8, 16] {
        for (name, img) in images() {
            let r = LayerRaster::from_gray(W, H, &img);
            let enc = anycubic::encode_planes(&r.runs, aa);
            let dec =
                anycubic::decode_planes(&enc, aa, (W * H) as u64).unwrap_or_else(|e| panic!("aa {aa} {name}: {e}"));
            let got = LayerRaster::from_runs(W, H, dec).to_gray();
            assert_eq!(
                got,
                quantised(&img, &|v| anycubic::quantize_planes(v, aa)),
                "aa {aa} {name}"
            );
        }
    }
}

#[test]
fn cxdlp_lines_are_lossless() {
    for (name, img) in images() {
        let r = LayerRaster::from_gray(W, H, &img);
        let bytes: Vec<u8> = cxdlp::encode_lines(&r).into_iter().flat_map(|l| l.to_bytes()).collect();
        assert_eq!(cxdlp::decode_lines(&bytes, W, H), img, "{name}");
    }
}

#[test]
fn pixel_pairs_roundtrip_within_3_bits() {
    for (name, img) in images() {
        let r = LayerRaster::from_gray(W, H, &img);
        let enc = cxdlpv4::encode_pixel_pairs(&r).unwrap();
        assert!(
            cxdlpv4::is_pixel_pairs(&enc, W, H),
            "{name}: not detected as pixel pairs"
        );
        let got = cxdlpv4::decode_pixel_pairs(&enc, W, H).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(got, quantised(&img, &cxdlpv4::quantize_pixel_pair), "{name}");
    }
    let odd = LayerRaster::from_gray(3, 1, &[0, 255, 0]);
    assert!(cxdlpv4::encode_pixel_pairs(&odd).is_err(), "odd widths are refused");
}

#[test]
fn vendor_decoders_reject_garbage_without_panicking() {
    let mut rng = Rng::new(0xDEC0);
    let px = (W * H) as u64;
    for _ in 0..3000 {
        let len = rng.below(64) as usize;
        let data: Vec<u8> = (0..len).map(|_| rng.u8()).collect();
        let _ = anycubic::decode_pw0(&data, px);
        let _ = anycubic::decode_planes(&data, (1 + rng.below(16)) as u8, px);
        let _ = cxdlpv4::decode_pixel_pairs(&data, W, H);
        let _ = cxdlpv4::is_pixel_pairs(&data, W, H);
        let lines = &data[..len - len % 6];
        assert_eq!(cxdlp::decode_lines(lines, W, H).len(), px as usize);
    }
    // runs past the image end are errors
    assert!(anycubic::decode_pw0(&[0x0F, 0xFF], 100).is_err());
    assert!(anycubic::decode_planes(&[0xFD], 1, 100).is_err());
    assert!(anycubic::decode_planes(&[], 0, 100).is_err(), "aa 0");
    let mut r = Rng::new(1);
    let img: Vec<u8> = (0..(W * H)).map(|_| r.u8()).collect();
    let enc = cxdlpv4::encode_pixel_pairs(&LayerRaster::from_gray(W, H, &img)).unwrap();
    assert!(
        cxdlpv4::decode_pixel_pairs(&enc[..enc.len() / 2], W, H).is_err(),
        "truncated"
    );
    assert!(cxdlpv4::decode_pixel_pairs(&enc, W, H - 1).is_err(), "too much data");
}

#[test]
fn vendor_codecs_handle_runs_longer_than_their_length_fields() {
    let runs = vec![
        Run {
            value: 0,
            len: 0x1_0000,
        },
        Run { value: 255, len: 5000 },
        Run { value: 128, len: 1000 },
        Run { value: 0, len: 300 },
    ];
    let total: u64 = runs.iter().map(|r| r.len as u64).sum();
    let dec = anycubic::decode_pw0(&anycubic::encode_pw0(&runs), total).unwrap();
    assert_eq!(canonical(&dec, &|v| v), canonical(&runs, &anycubic::quantize_pw0));
    for aa in [1u8, 4] {
        let dec = anycubic::decode_planes(&anycubic::encode_planes(&runs, aa), aa, total).unwrap();
        assert_eq!(
            canonical(&dec, &|v| v),
            canonical(&runs, &|v| anycubic::quantize_planes(v, aa))
        );
    }
}
