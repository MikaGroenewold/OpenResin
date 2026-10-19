//! GOO and CTB codecs in isolation: run-length encoders, the GOO checksum, the
//! CTB XOR keystream and the CTB preview codec, on edge cases and random images.

#[path = "formats_helpers.rs"]
mod helpers;

use helpers::*;
use openresin_core::slicer::{LayerRaster, Run};
use openresin_formats::preview::Thumbnail;
use openresin_formats::{ctb, goo};

const W: u32 = 97; // odd width so runs straddle row ends
const H: u32 = 61;

fn runs_of(pixels: &[u8]) -> Vec<Run> {
    LayerRaster::from_gray(W, H, pixels).runs
}

fn total(runs: &[Run]) -> u64 {
    runs.iter().map(|r| r.len as u64).sum()
}

/// Edge-case images (as full pixel buffers of W x H).
fn edge_images() -> Vec<(&'static str, Vec<u8>)> {
    let n = (W * H) as usize;
    let mut v = vec![("black", vec![0u8; n]), ("white", vec![255u8; n])];
    for (name, at) in [
        ("first pixel", 0usize),
        ("last pixel", n - 1),
        ("middle pixel", n / 2 + 3),
    ] {
        let mut img = vec![0u8; n];
        img[at] = 255;
        v.push((name, img));
        let mut img = vec![255u8; n];
        img[at] = 0;
        v.push((name, img));
    }
    v.push((
        "alternating",
        (0..n).map(|i| if i % 2 == 0 { 255 } else { 0 }).collect(),
    ));
    v.push((
        "alternating grey",
        (0..n).map(|i| if i % 2 == 0 { 3 } else { 250 }).collect(),
    ));
    v.push(("all grey levels", (0..n).map(|i| (i % 256) as u8).collect()));
    v.push((
        "grey ramp per row",
        (0..n)
            .map(|i| ((i % W as usize) * 255 / (W as usize - 1)) as u8)
            .collect(),
    ));
    v.push((
        "checker of AA values",
        (0..n)
            .map(|i| [0u8, 64, 128, 191, 255][(i / 7 + i / W as usize) % 5])
            .collect(),
    ));
    v.push((
        "near black/white",
        (0..n).map(|i| [1u8, 254, 0, 255, 2, 253][i % 6]).collect(),
    ));
    v
}

fn random_image(rng: &mut Rng) -> Vec<u8> {
    let n = (W * H) as usize;
    let mut img = Vec::with_capacity(n);
    while img.len() < n {
        let value = match rng.below(4) {
            0 => 0,
            1 => 255,
            _ => rng.u8(),
        };
        let len = match rng.below(3) {
            0 => 1,
            1 => rng.below(20) + 1,
            _ => rng.below(3 * W as u64) + 1,
        } as usize;
        img.extend(std::iter::repeat_n(value, len.min(n - img.len())));
    }
    img
}

/// Runs whose lengths exceed every length field (no pixel buffers needed).
fn long_runs() -> Vec<Vec<Run>> {
    let r = |value: u8, len: u32| Run { value, len };
    vec![
        vec![r(0, 0x0FFF_FFFF)],
        vec![r(0, 0x1000_0000)],
        vec![r(255, 0x1000_0001), r(0, 1)],
        vec![r(128, 0x2000_0005)],
        vec![r(255, u32::MAX)],
        vec![
            r(7, 0xF),
            r(7, 0x10),
            r(9, 0xFFF),
            r(9, 0x1000),
            r(0, 0xF_FFFF),
            r(0, 0x10_0000),
        ],
        // CTB length field boundaries
        vec![
            r(254, 0x7F),
            r(0, 0x80),
            r(254, 0x3FFF),
            r(0, 0x4000),
            r(254, 0x1F_FFFF),
            r(0, 0x20_0000),
            r(200, 2),
        ],
    ]
}

// ---------------------------------------------------------------- GOO

fn assert_goo_layer(data: &[u8], ctx: &str) {
    assert_eq!(data[0], 0x55, "{ctx}: magic");
    let sum = data[1..data.len() - 1].iter().fold(0u8, |a, &b| a.wrapping_add(b));
    assert_eq!(data[data.len() - 1], !sum, "{ctx}: checksum");
}

#[test]
fn goo_roundtrips_edge_cases_losslessly() {
    for (name, img) in edge_images() {
        let runs = runs_of(&img);
        let enc = goo::encode_runs(&runs);
        assert_goo_layer(&enc, name);
        let dec = goo::decode_runs(&enc, (W * H) as u64).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(canonical(&dec, &|v| v), canonical(&runs, &|v| v), "{name}");
        assert_eq!(LayerRaster::from_runs(W, H, dec).to_gray(), img, "{name}");
    }
}

#[test]
fn goo_roundtrips_random_images() {
    let mut rng = Rng::new(0x600D);
    for k in 0..200 {
        let img = random_image(&mut rng);
        let runs = runs_of(&img);
        let enc = goo::encode_runs(&runs);
        assert_goo_layer(&enc, "random");
        let dec = goo::decode_runs(&enc, (W * H) as u64).unwrap_or_else(|e| panic!("image {k}: {e}"));
        assert_eq!(LayerRaster::from_runs(W, H, dec).to_gray(), img, "image {k}");
    }
}

#[test]
fn goo_splits_runs_longer_than_28_bits() {
    for runs in long_runs() {
        let enc = goo::encode_runs(&runs);
        assert_goo_layer(&enc, "long");
        let dec = goo::decode_runs(&enc, total(&runs)).unwrap();
        assert_eq!(canonical(&dec, &|v| v), canonical(&runs, &|v| v));
    }
}

#[test]
fn goo_known_encodings() {
    let enc = |runs: &[Run]| {
        let e = goo::encode_runs(runs);
        e[1..e.len() - 1].to_vec()
    };
    // black: 0b00, 4-bit length
    assert_eq!(enc(&[Run { value: 0, len: 5 }]), vec![0x05]);
    // white: 0b11, 12-bit length (low nibble first, then the high byte)
    assert_eq!(enc(&[Run { value: 255, len: 0x123 }]), vec![0xD3, 0x12]);
    // grey: 0b01, value byte, 20-bit length
    assert_eq!(
        enc(&[Run {
            value: 0x80,
            len: 0x12345
        }]),
        vec![0x65, 0x80, 0x12, 0x34]
    );
    // 28-bit length
    assert_eq!(
        enc(&[Run {
            value: 0,
            len: 0x0ABC_DEF1
        }]),
        vec![0x31, 0xAB, 0xCD, 0xEF]
    );
    // a 1-pixel grey run
    assert_eq!(enc(&[Run { value: 7, len: 1 }]), vec![0x41, 0x07]);
}

#[test]
fn goo_rejects_corrupted_payloads() {
    let mut rng = Rng::new(0xBAD);
    let img = random_image(&mut rng);
    let enc = goo::encode_runs(&runs_of(&img));
    let px = (W * H) as u64;
    // every single-byte change is detected (checksum or pixel count), never a panic
    for at in 0..enc.len() {
        for delta in [1u8, 0x10, 0x80] {
            let mut bad = enc.clone();
            bad[at] = bad[at].wrapping_add(delta);
            match goo::decode_runs(&bad, px) {
                Err(_) => {}
                Ok(runs) => panic!("corruption at byte {at} (+{delta}) decoded to {} pixels", total(&runs)),
            }
        }
    }
    // truncation and wrong expectations
    for cut in [0, 1, 2, enc.len() / 2, enc.len() - 1] {
        assert!(goo::decode_runs(&enc[..cut], px).is_err(), "cut at {cut}");
    }
    assert!(goo::decode_runs(&enc, px + 1).is_err());
    assert!(goo::decode_runs(&enc, px - 1).is_err());
    assert!(goo::decode_runs(&[], 0).is_err());
}

#[test]
fn goo_decoder_survives_garbage() {
    let mut rng = Rng::new(42);
    for _ in 0..2000 {
        let len = rng.below(64) as usize + 2;
        let mut data: Vec<u8> = (0..len).map(|_| rng.u8()).collect();
        data[0] = 0x55;
        let sum = data[1..len - 1].iter().fold(0u8, |a, &b| a.wrapping_add(b));
        data[len - 1] = !sum; // valid checksum, random content
        let _ = goo::decode_runs(&data, rng.below(1 << 20));
    }
}

// ---------------------------------------------------------------- CTB RLE

#[test]
fn ctb_roundtrips_edge_cases_with_7bit_quantisation() {
    for (name, img) in edge_images() {
        let runs = runs_of(&img);
        let enc = ctb::encode_rle(&runs);
        let dec = ctb::decode_rle(&enc, (W * H) as u64).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(canonical(&dec, &|v| v), canonical(&runs, &quantize_ctb7), "{name}");
        let want: Vec<u8> = img.iter().map(|&v| quantize_ctb7(v)).collect();
        assert_eq!(LayerRaster::from_runs(W, H, dec).to_gray(), want, "{name}");
    }
}

#[test]
fn ctb_quantisation_is_documented_7bit() {
    for v in 0..=255u8 {
        let q = quantize_ctb7(v);
        assert!(q.abs_diff(v) <= 1, "{v} -> {q}");
        assert_eq!(quantize_ctb7(q), q, "idempotent");
        let runs = [Run { value: v, len: 3 }];
        let dec = ctb::decode_rle(&ctb::encode_rle(&runs), 3).unwrap();
        assert_eq!(dec, vec![Run { value: q, len: 3 }], "value {v}");
    }
    assert_eq!((quantize_ctb7(0), quantize_ctb7(1), quantize_ctb7(255)), (0, 0, 255));
}

#[test]
fn ctb_roundtrips_random_images() {
    let mut rng = Rng::new(0xC7B);
    for k in 0..200 {
        let img = random_image(&mut rng);
        let runs = runs_of(&img);
        let dec = ctb::decode_rle(&ctb::encode_rle(&runs), (W * H) as u64).unwrap_or_else(|e| panic!("{k}: {e}"));
        assert_eq!(canonical(&dec, &|v| v), canonical(&runs, &quantize_ctb7), "image {k}");
    }
}

#[test]
fn ctb_splits_long_runs_and_uses_all_length_widths() {
    for runs in long_runs() {
        let enc = ctb::encode_rle(&runs);
        let dec = ctb::decode_rle(&enc, total(&runs)).unwrap();
        assert_eq!(canonical(&dec, &|v| v), canonical(&runs, &quantize_ctb7));
    }
    let enc = |len: u32| ctb::encode_rle(&[Run { value: 255, len }]);
    assert_eq!(enc(1), vec![0x7F]);
    assert_eq!(enc(0x7F), vec![0xFF, 0x7F]);
    assert_eq!(enc(0x80), vec![0xFF, 0x80, 0x80]);
    assert_eq!(enc(0x3FFF), vec![0xFF, 0xBF, 0xFF]);
    assert_eq!(enc(0x4000), vec![0xFF, 0xC0, 0x40, 0x00]);
    assert_eq!(enc(0x20_0000), vec![0xFF, 0xE0, 0x20, 0x00, 0x00]);
}

#[test]
fn ctb_rle_decoder_survives_garbage() {
    let mut rng = Rng::new(7);
    for _ in 0..5000 {
        let len = rng.below(48) as usize;
        let data: Vec<u8> = (0..len).map(|_| rng.u8()).collect();
        if let Ok(runs) = ctb::decode_rle(&data, rng.below(1 << 30)) {
            assert!(runs.iter().all(|r| r.len > 0));
        }
    }
    // a run longer than the image and a truncated length are errors
    assert!(ctb::decode_rle(&[0xFF, 0x7F], 10).is_err());
    assert!(ctb::decode_rle(&[0xFF, 0xC0, 0x40], 0x4000).is_err());
    assert!(ctb::decode_rle(&[0xFF, 0xF0], 1).is_err(), "invalid length prefix");
}

// ---------------------------------------------------------------- CTB keystream

/// Reference implementation of the classic CTB layer cipher, written from the
/// published algorithm independently of `ctb::crypt_layer`.
fn reference_crypt(seed: u32, layer: u32, data: &mut [u8]) {
    if seed == 0 {
        return;
    }
    let init = seed.wrapping_mul(0x2d83_cdac).wrapping_add(0xd8a8_3423);
    let mut key = layer
        .wrapping_mul(0x1e15_30cd)
        .wrapping_add(0xec3d_47cd)
        .wrapping_mul(init);
    let mut index = 0;
    for b in data.iter_mut() {
        let k = (key >> (8 * index)) as u8;
        index += 1;
        if index & 3 == 0 {
            key = key.wrapping_add(init);
            index = 0;
        }
        *b ^= k;
    }
}

#[test]
fn ctb_keystream_matches_reference_and_roundtrips() {
    let mut rng = Rng::new(0x5EED);
    for _ in 0..200 {
        let seed = rng.next_u64() as u32 | 1;
        let layer = rng.below(100_000) as u32;
        let len = rng.below(300) as usize; // includes lengths that are not multiples of 4
        let plain: Vec<u8> = (0..len).map(|_| rng.u8()).collect();
        let mut a = plain.clone();
        ctb::crypt_layer(seed, layer, &mut a);
        let mut b = plain.clone();
        reference_crypt(seed, layer, &mut b);
        assert_eq!(a, b, "seed {seed:#x} layer {layer}");
        ctb::crypt_layer(seed, layer, &mut a);
        assert_eq!(a, plain, "decryption");
    }
    // seed 0 disables encryption
    let mut d = vec![1u8, 2, 3, 4, 5];
    ctb::crypt_layer(0, 3, &mut d);
    assert_eq!(d, vec![1, 2, 3, 4, 5]);
}

#[test]
fn ctb_encrypted_layers_decode_identically_for_any_seed() {
    let mut rng = Rng::new(99);
    let img = random_image(&mut rng);
    let runs = runs_of(&img);
    let plain = ctb::encode_rle(&runs);
    let want = ctb::decode_rle(&plain, (W * H) as u64).unwrap();
    let mut ciphertexts = Vec::new();
    for seed in [1u32, 0x100, 0xdead_beef, 0x1234_5678, u32::MAX] {
        for layer in [0u32, 1, 2, 1000] {
            let mut data = plain.clone();
            ctb::crypt_layer(seed, layer, &mut data);
            ciphertexts.push(data.clone());
            ctb::crypt_layer(seed, layer, &mut data);
            assert_eq!(ctb::decode_rle(&data, (W * H) as u64).unwrap(), want);
        }
    }
    // different seeds and layers give different key streams
    for i in 0..ciphertexts.len() {
        for j in i + 1..ciphertexts.len() {
            assert_ne!(ciphertexts[i], ciphertexts[j], "ciphertexts {i} and {j} are equal");
        }
    }
}

// ---------------------------------------------------------------- CTB previews

fn gradient(w: u32, h: u32) -> Thumbnail {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            rgba.extend_from_slice(&[
                (x * 255 / w.max(1)) as u8,
                (y * 255 / h.max(1)) as u8,
                ((x + y) % 256) as u8,
                255,
            ]);
        }
    }
    Thumbnail::new(w, h, rgba)
}

#[test]
fn ctb_preview_roundtrip_is_rgb555() {
    let uniform = Thumbnail::new(400, 300, [200u8, 100, 50, 255].repeat(400 * 300));
    let mut rng = Rng::new(3);
    let noise = Thumbnail::new(
        64,
        32,
        (0..64 * 32).flat_map(|_| [rng.u8(), rng.u8(), rng.u8(), 255]).collect(),
    );
    for t in [gradient(200, 125), gradient(1, 1), uniform, noise] {
        let enc = ctb::encode_preview(&t);
        let back = ctb::decode_preview(t.width, t.height, &enc);
        assert_eq!((back.width, back.height), (t.width, t.height));
        for (a, b) in t.rgba.chunks_exact(4).zip(back.rgba.chunks_exact(4)) {
            assert_eq!([b[0], b[1], b[2], b[3]], [a[0] & 0xF8, a[1] & 0xF8, a[2] & 0xF8, 255]);
        }
    }
}

#[test]
fn ctb_preview_long_runs_are_split_at_4096() {
    // 120000 equal pixels: 29 full runs of 4096 + one of 1216
    let t = Thumbnail::new(400, 300, [255u8, 255, 255, 255].repeat(400 * 300));
    let enc = ctb::encode_preview(&t);
    assert_eq!(enc.len(), 30 * 4);
    for chunk in enc.chunks_exact(4).take(29) {
        assert_eq!(u16::from_le_bytes([chunk[2], chunk[3]]), 0x3000 | 0xFFF);
    }
    // a truncated or empty stream decodes to a full-size image (padded black)
    let short = ctb::decode_preview(400, 300, &enc[..10]);
    assert_eq!(short.rgba.len(), 400 * 300 * 4);
    let empty = ctb::decode_preview(4, 4, &[]);
    assert!(empty.rgba.iter().all(|&v| v == 0));
}
