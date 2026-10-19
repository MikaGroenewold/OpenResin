//! Real H.264 test video (OpenH264 encoder) and RTP packetizers.

use openh264::encoder::{Encoder, EncoderConfig, RateControlMode};
use openh264::formats::{RgbSliceU8, YUVBuffer};
use openh264::OpenH264API;

pub const WIDTH: usize = 320;
pub const HEIGHT: usize = 240;

/// Colour bars (red, green, blue, white) with a small grey square moving
/// along the bottom so consecutive frames differ.
pub fn test_pattern(frame: usize) -> Vec<u8> {
    let bars = [[220u8, 20, 20], [20, 200, 20], [20, 20, 220], [235, 235, 235]];
    let mut rgb = vec![0u8; WIDTH * HEIGHT * 3];
    let sq = (frame * 16) % (WIDTH - 32);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let c = if y >= HEIGHT - 40 && x >= sq && x < sq + 32 {
                [128, 128, 128]
            } else if (100..180).contains(&y) {
                // static noise: makes the IDR frame large enough to need fragmentation
                let n = ((x as u32).wrapping_mul(7919) ^ (y as u32).wrapping_mul(104_729)).wrapping_mul(2_654_435_761)
                    >> 24;
                [n as u8, (n as u8).wrapping_mul(3), (n as u8).wrapping_mul(7)]
            } else {
                bars[x * bars.len() / WIDTH]
            };
            rgb[(y * WIDTH + x) * 3..][..3].copy_from_slice(&c);
        }
    }
    rgb
}

/// Expected colour of the bar under pixel column `x` (top rows only).
pub fn bar_color(x: usize) -> [u8; 3] {
    let bars = [[220u8, 20, 20], [20, 200, 20], [20, 20, 220], [235, 235, 235]];
    bars[x * bars.len() / WIDTH]
}

/// Splits an Annex B stream into NAL units (without start codes).
pub fn split_nals(annexb: &[u8]) -> Vec<Vec<u8>> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= annexb.len() {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::new();
    for (k, &s) in starts.iter().enumerate() {
        let mut e = if k + 1 < starts.len() {
            starts[k + 1] - 3
        } else {
            annexb.len()
        };
        while k + 1 < starts.len() && e > s && annexb[e - 1] == 0 {
            e -= 1;
        }
        out.push(annexb[s..e].to_vec());
    }
    out
}

/// Encodes `frames` frames; each access unit is a list of NAL units. The
/// first one is an IDR frame preceded by SPS and PPS.
pub fn encode_sequence(frames: usize) -> Vec<Vec<Vec<u8>>> {
    // fixed quality, no rate control: no frame is ever skipped
    let cfg = EncoderConfig::new()
        .rate_control_mode(RateControlMode::Off)
        .set_bitrate_bps(600_000)
        .max_frame_rate(10.0)
        .enable_skip_frame(false)
        .set_multiple_thread_idc(1);
    let mut enc = Encoder::with_api_config(OpenH264API::from_source(), cfg).expect("H.264 encoder");
    let mut out = Vec::new();
    for i in 0..frames {
        let rgb = test_pattern(i);
        let yuv = YUVBuffer::from_rgb_source(RgbSliceU8::new(&rgb, (WIDTH, HEIGHT)));
        let bs = enc.encode(&yuv).expect("encode frame");
        let nals = split_nals(&bs.to_vec());
        assert!(!nals.is_empty(), "encoder produced no data for frame {i}");
        out.push(nals);
    }
    let first: Vec<u8> = out[0].iter().map(|n| n[0] & 0x1F).collect();
    assert!(
        first.contains(&7) && first.contains(&8) && first.contains(&5),
        "first AU is not SPS+PPS+IDR: {first:?}"
    );
    out
}

pub fn nal_type(nal: &[u8]) -> u8 {
    nal[0] & 0x1F
}

/// Annex B bytes of an access unit.
pub fn annexb(au: &[Vec<u8>]) -> Vec<u8> {
    let mut v = Vec::new();
    for n in au {
        v.extend_from_slice(&[0, 0, 0, 1]);
        v.extend_from_slice(n);
    }
    v
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Packetization {
    /// RFC 6184 single NAL unit packets, FU-A for large NAL units.
    SingleNalFuA,
    /// RFC 6184 with SPS+PPS aggregated in one STAP-A packet.
    StapA,
    /// The printer quirk: raw Annex B in plain packets, large units in
    /// "FU-A" packets whose FU header has NAL type 0.
    RawAnnexB,
}

/// RTP payloads for one access unit.
pub fn packetize(au: &[Vec<u8>], mode: Packetization, mtu: usize) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut nals: Vec<&Vec<u8>> = au.iter().collect();
    if mode == Packetization::StapA {
        let params: Vec<&Vec<u8>> = nals.iter().copied().filter(|n| matches!(nal_type(n), 7 | 8)).collect();
        if params.len() >= 2 {
            let nri = params.iter().map(|n| n[0] & 0x60).max().unwrap();
            let mut p = vec![nri | 24];
            for n in &params {
                p.extend_from_slice(&(n.len() as u16).to_be_bytes());
                p.extend_from_slice(n);
            }
            out.push(p);
            nals.retain(|n| !matches!(nal_type(n), 7 | 8));
        }
    }
    for nal in nals {
        match mode {
            Packetization::SingleNalFuA | Packetization::StapA => {
                if nal.len() <= mtu {
                    out.push(nal.clone());
                } else {
                    let indicator = (nal[0] & 0xE0) | 28;
                    let ty = nal[0] & 0x1F;
                    let body = &nal[1..];
                    let pieces: Vec<&[u8]> = body.chunks(mtu - 2).collect();
                    for (i, piece) in pieces.iter().enumerate() {
                        let mut h = ty;
                        if i == 0 {
                            h |= 0x80;
                        }
                        if i == pieces.len() - 1 {
                            h |= 0x40;
                        }
                        let mut p = vec![indicator, h];
                        p.extend_from_slice(piece);
                        out.push(p);
                    }
                }
            }
            Packetization::RawAnnexB => {
                let mut raw = vec![0, 0, 0, 1];
                raw.extend_from_slice(nal);
                if raw.len() <= mtu {
                    out.push(raw);
                } else {
                    let pieces: Vec<&[u8]> = raw.chunks(mtu - 2).collect();
                    for (i, piece) in pieces.iter().enumerate() {
                        let mut h = 0u8;
                        if i == 0 {
                            h |= 0x80;
                        }
                        if i == pieces.len() - 1 {
                            h |= 0x40;
                        }
                        let mut p = vec![0x1C, h];
                        p.extend_from_slice(piece);
                        out.push(p);
                    }
                }
            }
        }
    }
    out
}

/// One RTP packet (version 2, payload type 96).
pub fn rtp_packet(seq: u16, ts: u32, ssrc: u32, marker: bool, payload: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(12 + payload.len());
    p.push(0x80);
    p.push(if marker { 0x80 | 96 } else { 96 });
    p.extend_from_slice(&seq.to_be_bytes());
    p.extend_from_slice(&ts.to_be_bytes());
    p.extend_from_slice(&ssrc.to_be_bytes());
    p.extend_from_slice(payload);
    p
}

pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Decodes an Annex B access unit with OpenH264; returns (width, height, rgb).
pub fn decode(dec: &mut openh264::decoder::Decoder, au: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    use openh264::formats::YUVSource;
    let yuv = dec.decode(au).ok()??;
    let (w, h) = yuv.dimensions();
    let mut rgb = vec![0u8; w * h * 3];
    yuv.write_rgb8(&mut rgb);
    Some((w, h, rgb))
}

/// Asserts that the top rows of `rgb` show the colour bars of [`test_pattern`].
pub fn assert_bars(w: usize, h: usize, rgb: &[u8], tolerance: i32) {
    assert_eq!((w, h), (WIDTH, HEIGHT));
    for bar in 0..4 {
        let x = bar * WIDTH / 4 + WIDTH / 8;
        let want = bar_color(x);
        let y = 20;
        let got = &rgb[(y * w + x) * 3..][..3];
        for c in 0..3 {
            let d = (got[c] as i32 - want[c] as i32).abs();
            assert!(d <= tolerance, "bar {bar} at ({x},{y}): got {got:?}, want {want:?}");
        }
    }
}
