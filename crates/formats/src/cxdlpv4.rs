//! Creality HALOT CXDLPv4 (`.cxdlpv4`).
//!
//! A CXDLP preamble (big-endian `u32` magic length, `"CXSW3DV2\0"`, `u16`
//! version 4 and the printer model as a big-endian length-prefixed string)
//! followed by a little-endian body that mirrors the classic CTB layout:
//!
//! * header: resolution (`u16` × 2), bed size, print height, layer height,
//!   bottom layer count, addresses of the previews, the print parameters, the
//!   slicer info and the layer table, print time, projector type,
//!   anti-aliasing level, light PWM and the layer encryption key;
//! * two previews (120×120 and 300×300) in the CTB RGB15 run-length format;
//! * print parameters (68 bytes) and slicer info (76 bytes);
//! * a table of 40 byte layer records (position, exposure, light-off delay,
//!   data address and size, area);
//! * per layer a 44 byte extended record (lift/retract/wait settings, PWM)
//!   followed by the image;
//! * a big-endian CRC-32 (no pre/post inversion) of everything before it.
//!
//! Images use the CTB 7-bit grey run-length codec ([`crate::ctb::encode_rle`]),
//! except on the HALOT-X1 family (15120×6230) which uses a row based codec of
//! 3-bit pixel pairs ([`encode_pixel_pairs`]). OpenResin never encrypts layers;
//! encrypted files are read with the CTB key schedule.
//!
//! # Verification status
//!
//! The layout follows the field order documented by the UVtools project. Files
//! with the classic codec were read back with UVtools 4.4.3: header fields,
//! two-stage motion, rest times, light-off delays, PWM and the layer images
//! match (UVtools expands 7-bit greys as `v << 1 | 1`, OpenResin as
//! `v << 1 | v & 1`, so grey pixels differ by one). That version predates the
//! pixel pair codec, which was checked against the reference's codec source and
//! a row of a HALOT-X1 sample instead. No file from Creality's slicers was
//! available and nothing was printed. Not verifiable here:
//! * the printer model string, see [`model_name`];
//! * which printers use the pixel pair codec: OpenResin uses it for model
//!   `HALOT-X1`/`R009` or a resolution of 15120×6230, like the reference;
//! * the 3-bit pixel pair encoder rounds to the nearest level, the reference
//!   truncates (`value >> 5`); both decode identically;
//! * the per-layer area is documented as the largest contour area in
//!   mm² × 1000; OpenResin writes the total lit area.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Mutex;

use openresin_core::estimate::PrintEstimate;
use openresin_core::profile::{LayerParams, PrinterProfile};
use openresin_core::slicer::LayerRaster;

use crate::bin::{LeWriter, Reader};
use crate::cxdlp::{crc32_combine, crc32_update, file_crc32, MAGIC, MAGIC_PREFIX};
use crate::preview::Thumbnail;
use crate::{ctb, FormatError, FormatWriter, JobInfo, LayerMeta, Result, SlicedFile, SlicedInfo, WriteSeek};

pub const VERSION: u16 = 4;
pub const PREVIEW_SMALL: (u32, u32) = (120, 120);
pub const PREVIEW_LARGE: (u32, u32) = (300, 300);
const PREVIEW_HEAD_SIZE: u32 = 32;
const PRINT_PARAMS_SIZE: u32 = 68;
const SLICER_INFO_SIZE: u32 = 76;
const LAYER_DEF_SIZE: u32 = 40;
const LAYER_DEF_EX_SIZE: u32 = 44;

/// Layer image codec of a CXDLPv4 file (not recorded in the file).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V4Codec {
    /// CTB 7-bit grey run-length codec.
    Rle7,
    /// Rows of 3-bit pixel pairs (HALOT-X1 family).
    PixelPair,
}

/// Resolution of the HALOT-X1 family.
const X1_RESOLUTION: (u32, u32) = (15120, 6230);

/// Printer model written into the header.
///
/// As in the reference: an explicit machine type is written unchanged, the
/// HALOT-X1 family as `HALOT-X1` (the name Creality's slicers write), the
/// HALOT-MAGE and MAGE PRO by their codes `CL-103L` and `CL-103`, and any
/// other printer by its profile name (`HALOT-R6`, not `CREALITY HALOT-R6`).
pub fn model_name(p: &PrinterProfile) -> String {
    if let Some(mt) = &p.machine_type {
        return mt.clone();
    }
    let name = p.name.to_ascii_uppercase();
    let compact: String = name.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    match (p.resolution_x, p.resolution_y) {
        X1_RESOLUTION if compact.contains("HALOTX1") => "HALOT-X1".into(),
        (7680, 4320) if name.contains("MAGE") && name.contains("PRO") => "CL-103".into(),
        (7680, 4320) if name.contains("MAGE") => "CL-103L".into(),
        _ if p.name.trim().is_empty() => p.machine_type(),
        _ => p.name.trim().to_string(),
    }
}

/// Codec used for a printer.
pub fn codec_for(p: &PrinterProfile) -> V4Codec {
    if p.resolution_x % 2 != 0 {
        return V4Codec::Rle7;
    }
    let model = model_name(p);
    if model.eq_ignore_ascii_case("HALOT-X1")
        || model.eq_ignore_ascii_case("R009")
        || (p.resolution_x, p.resolution_y) == X1_RESOLUTION
    {
        V4Codec::PixelPair
    } else {
        V4Codec::Rle7
    }
}

// ---------------------------------------------------------------- pixel pair codec

const UNIT_FLAG: u8 = 0x80;
const RESERVED_FLAG: u8 = 0x40;

/// 8-bit grey of a 3-bit level (0 -> 0, 7 -> 255).
const fn expand3(g: u8) -> u8 {
    (g << 5) | (g << 2) | (g >> 1)
}

/// Nearest 3-bit level of every 8-bit grey.
const LEVEL3: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut v = 0;
    while v < 256 {
        let mut best = 0u8;
        let mut g = 1u8;
        while g < 8 {
            let d = (expand3(g) as i32 - v as i32).abs();
            let db = (expand3(best) as i32 - v as i32).abs();
            if d < db {
                best = g;
            }
            g += 1;
        }
        t[v] = best;
        v += 1;
    }
    t
};

/// The value a pixel has after a pixel pair round trip.
pub fn quantize_pixel_pair(v: u8) -> u8 {
    expand3(LEVEL3[v as usize])
}

#[inline]
fn unit_at(row: &[u8], u: usize) -> u8 {
    UNIT_FLAG | LEVEL3[row[2 * u] as usize] | (LEVEL3[row[2 * u + 1] as usize] << 3)
}

fn push_unit(out: &mut Vec<u8>, unit: u8, count: usize) {
    out.push(unit);
    let repeat = count - 1;
    if repeat == 0 {
        return;
    }
    let mut groups = 1;
    while repeat >> (7 * groups) != 0 {
        groups += 1;
    }
    for g in (0..groups).rev() {
        out.push(((repeat >> (7 * g)) & 0x7F) as u8);
    }
}

/// Encodes a layer as rows of pixel pairs.
///
/// A byte with bit 7 set is a unit of two horizontally adjacent pixels, the
/// 3-bit level of the left pixel in bits 0-2 and of the right one in bits 3-5
/// (bit 6 is never set). Following bytes with bit 7 clear are the big-endian
/// 7-bit groups of a repeat count: the unit is written `repeat + 1` times. Runs
/// never cross a row, so the width must be even.
pub fn encode_pixel_pairs(raster: &LayerRaster) -> Result<Vec<u8>> {
    let w = raster.width as usize;
    if w % 2 != 0 {
        return Err(FormatError::Unsupported(
            "the pixel pair codec needs an even width".into(),
        ));
    }
    let units = w / 2;
    let mut out = Vec::with_capacity(raster.height as usize * 3 + raster.runs.len() * 2);
    raster.for_each_row(|_, row| {
        let mut u = 0;
        while u < units {
            let unit = unit_at(row, u);
            let mut n = 1;
            while u + n < units && unit_at(row, u + n) == unit {
                n += 1;
            }
            push_unit(&mut out, unit, n);
            u += n;
        }
    });
    Ok(out)
}

/// Decodes pixel pair data into a `width` × `height` grey image.
pub fn decode_pixel_pairs(data: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let w = width as usize;
    if w % 2 != 0 {
        return Err(FormatError::Invalid("pixel pair layer with an odd width".into()));
    }
    // every row starts with a unit byte; checked before the image buffer is
    // allocated from the (untrusted) resolution
    if data.len() < height as usize {
        return Err(FormatError::Invalid(format!(
            "pixel pair layer of {} bytes is too short for {height} rows",
            data.len()
        )));
    }
    let units_per_row = w / 2;
    let total = units_per_row * height as usize;
    let mut px = vec![0u8; w * height as usize];
    let mut unit = 0usize;
    let mut n = 0usize;
    while n < data.len() {
        let code = data[n];
        n += 1;
        if code & UNIT_FLAG == 0 || code & RESERVED_FLAG != 0 {
            return Err(FormatError::Invalid(format!("bad pixel pair code {code:#04x}")));
        }
        if unit >= total {
            return Err(FormatError::Invalid("pixel pair layer runs past the image end".into()));
        }
        let remaining = units_per_row - unit % units_per_row;
        let mut repeat = 0usize;
        while n < data.len() && data[n] & UNIT_FLAG == 0 {
            repeat = (repeat << 7) | data[n] as usize;
            n += 1;
            if repeat >= remaining {
                return Err(FormatError::Invalid("pixel pair run crosses a row".into()));
            }
        }
        let count = repeat + 1;
        let (l, r) = (expand3(code & 7), expand3((code >> 3) & 7));
        for pair in px[unit * 2..(unit + count) * 2].chunks_exact_mut(2) {
            pair[0] = l;
            pair[1] = r;
        }
        unit += count;
    }
    if unit != total {
        return Err(FormatError::Invalid(format!(
            "pixel pair layer covers {unit} of {total} pixel pairs"
        )));
    }
    Ok(px)
}

/// Whether `data` is a complete pixel pair image of the given size (used to
/// detect the codec, as it is not recorded in the file).
pub fn is_pixel_pairs(data: &[u8], width: u32, height: u32) -> bool {
    if width == 0 || width % 2 != 0 || height == 0 {
        return false;
    }
    let units_per_row = width as u64 / 2;
    let total = units_per_row * height as u64;
    let mut unit = 0u64;
    let mut n = 0usize;
    while n < data.len() {
        let code = data[n];
        n += 1;
        if code & UNIT_FLAG == 0 || code & RESERVED_FLAG != 0 || unit >= total {
            return false;
        }
        let remaining = units_per_row - unit % units_per_row;
        let mut repeat = 0u64;
        while n < data.len() && data[n] & UNIT_FLAG == 0 {
            repeat = (repeat << 7) | data[n] as u64;
            n += 1;
            if repeat >= remaining {
                return false;
            }
        }
        unit += repeat + 1;
    }
    unit == total
}

// ---------------------------------------------------------------- tables

/// The file header (after the big-endian preamble everything is little-endian).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct V4Header {
    pub version: u16,
    pub model: String,
    pub resolution_x: u16,
    pub resolution_y: u16,
    pub bed_x: f32,
    pub bed_y: f32,
    pub bed_z: f32,
    pub print_height: f32,
    pub layer_height: f32,
    pub bottom_layers: u32,
    pub preview_small: u32,
    pub layer_defs: u32,
    pub layer_count: u32,
    pub preview_large: u32,
    pub print_time_s: u32,
    pub projector_type: u32,
    pub print_params: u32,
    pub print_params_size: u32,
    pub anti_aliasing: u32,
    pub light_pwm: u16,
    pub bottom_light_pwm: u16,
    pub encryption_key: u32,
    pub slicer_info: u32,
    pub slicer_info_size: u32,
}

impl V4Header {
    fn size(model: &str) -> u32 {
        4 + MAGIC.len() as u32 + 2 + 4 + model.len() as u32 + 1 + 4 + 5 * 4 + 10 * 4 + 4 + 3 * 4
    }

    fn encode(&self, w: &mut LeWriter) {
        w.bytes(&(MAGIC.len() as u32).to_be_bytes()).bytes(MAGIC);
        w.bytes(&self.version.to_be_bytes());
        w.bytes(&(self.model.len() as u32 + 1).to_be_bytes())
            .bytes(self.model.as_bytes())
            .u8(0);
        w.u16(self.resolution_x).u16(self.resolution_y);
        w.f32(self.bed_x)
            .f32(self.bed_y)
            .f32(self.bed_z)
            .f32(self.print_height)
            .f32(self.layer_height);
        w.u32(self.bottom_layers)
            .u32(self.preview_small)
            .u32(self.layer_defs)
            .u32(self.layer_count)
            .u32(self.preview_large)
            .u32(self.print_time_s)
            .u32(self.projector_type)
            .u32(self.print_params)
            .u32(self.print_params_size)
            .u32(self.anti_aliasing);
        w.u16(self.light_pwm).u16(self.bottom_light_pwm);
        w.u32(self.encryption_key)
            .u32(self.slicer_info)
            .u32(self.slicer_info_size);
    }

    fn decode(data: &[u8]) -> Result<Self> {
        let mut be = Reader::be(data, 0);
        let magic_len = be.u32()? as usize;
        if !(6..=64).contains(&magic_len) || !be.take(magic_len)?.starts_with(MAGIC_PREFIX) {
            return Err(FormatError::Invalid("not a CXDLPv4 file".into()));
        }
        let version = be.u16()?;
        let model_len = be.u32()? as usize;
        if model_len > 1024 {
            return Err(FormatError::Invalid("bad CXDLPv4 printer model".into()));
        }
        let model = be.str_fixed(model_len)?;
        let mut r = Reader::le(data, be.pos);
        Ok(V4Header {
            version,
            model,
            resolution_x: r.u16()?,
            resolution_y: r.u16()?,
            bed_x: r.f32()?,
            bed_y: r.f32()?,
            bed_z: r.f32()?,
            print_height: r.f32()?,
            layer_height: r.f32()?,
            bottom_layers: r.u32()?,
            preview_small: r.u32()?,
            layer_defs: r.u32()?,
            layer_count: r.u32()?,
            preview_large: r.u32()?,
            print_time_s: r.u32()?,
            projector_type: r.u32()?,
            print_params: r.u32()?,
            print_params_size: r.u32()?,
            anti_aliasing: r.u32()?,
            light_pwm: r.u16()?,
            bottom_light_pwm: r.u16()?,
            encryption_key: r.u32()?,
            slicer_info: r.u32()?,
            slicer_info_size: r.u32()?,
        })
    }
}

/// Entry of the layer table.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct V4LayerDef {
    pub position_z: f32,
    pub exposure: f32,
    pub light_off_delay: f32,
    /// Address of the extended record that precedes the image.
    pub data_address: u32,
    /// Size of the extended record plus the image.
    pub data_size: u32,
    pub data_type: u32,
    /// Lit area, mm² × 1000.
    pub area: u32,
}

impl V4LayerDef {
    fn encode(&self, w: &mut LeWriter) {
        w.f32(self.position_z).f32(self.exposure).f32(self.light_off_delay);
        w.u32(self.data_address).u32(self.data_size).u32(self.data_type);
        w.u32(0).u32(self.area).u32(0).u32(0);
    }

    fn decode(r: &mut Reader) -> Result<Self> {
        let d = V4LayerDef {
            position_z: r.f32()?,
            exposure: r.f32()?,
            light_off_delay: r.f32()?,
            data_address: r.u32()?,
            data_size: r.u32()?,
            data_type: r.u32()?,
            area: {
                r.skip(4)?;
                r.u32()?
            },
        };
        r.skip(8)?;
        Ok(d)
    }
}

/// Rest times (before lift, after lift, after retract) and light-off delay;
/// the format uses either the rest times or the light-off delay.
fn waits(p: &LayerParams, wait_mode: bool) -> ([f32; 3], f32) {
    if wait_mode {
        ([p.wait_before_lift, p.wait_after_lift, p.wait_after_retract], 0.0)
    } else {
        ([0.0; 3], p.light_off_delay)
    }
}

// ---------------------------------------------------------------- writer

pub struct CxdlpV4Writer {
    out: Box<dyn WriteSeek>,
    codec_override: Option<V4Codec>,
    codec: V4Codec,
    previews: [Vec<u8>; 2],
    params: [LayerParams; 2],
    wait_mode: bool,
    layer_count: u32,
    head_len: u64,
    next_data: u64,
    last_z: f32,
    defs: Vec<V4LayerDef>,
    crc: u32,
    data_len: u64,
    timestamp_min: u32,
}

impl CxdlpV4Writer {
    pub fn new(out: Box<dyn WriteSeek>) -> Self {
        let p = openresin_core::profile::PrintSettings::default().normal;
        Self {
            out,
            codec_override: None,
            codec: V4Codec::Rle7,
            previews: [Vec::new(), Vec::new()],
            params: [p, p],
            wait_mode: true,
            layer_count: 0,
            head_len: 0,
            next_data: 0,
            last_z: 0.0,
            defs: Vec::new(),
            crc: 0,
            data_len: 0,
            timestamp_min: 0,
        }
    }

    /// Forces a layer codec instead of choosing it from the printer.
    pub fn with_codec(mut self, codec: V4Codec) -> Self {
        self.codec_override = Some(codec);
        self
    }

    fn build_head(&self, info: &JobInfo, est: Option<&PrintEstimate>) -> Vec<u8> {
        let p = info.printer;
        let s = info.settings;
        let (b, n) = (&s.bottom, &s.normal);
        let model = model_name(p);
        let mut pos = V4Header::size(&model);
        let mut body = LeWriter::default();
        let mut preview_addr = [0u32; 2];
        for (k, (pw, ph)) in [PREVIEW_SMALL, PREVIEW_LARGE].into_iter().enumerate() {
            let data = &self.previews[k];
            preview_addr[k] = pos;
            body.u32(pw).u32(ph).u32(pos + PREVIEW_HEAD_SIZE).u32(data.len() as u32);
            body.bytes(&[0u8; 16]).bytes(data);
            pos += PREVIEW_HEAD_SIZE + data.len() as u32;
        }
        let params_addr = pos;
        let (_, b_off) = waits(b, s.wait_mode);
        let (n_rest, n_off) = waits(n, s.wait_mode);
        body.f32(b.lift.total_distance())
            .f32(b.lift.speed)
            .f32(n.lift.total_distance())
            .f32(n.lift.speed)
            .f32(n.retract.speed)
            .f32(est.map(|e| e.volume_ml() as f32).unwrap_or(0.0))
            .f32(est.map(|e| e.weight_g as f32).unwrap_or(0.0))
            .f32(est.map(|e| e.cost as f32).unwrap_or(0.0))
            .f32(b_off)
            .f32(n_off)
            .u32(s.bottom_layers)
            .f32(n.exposure)
            .f32(b.exposure)
            .bytes(&[0u8; 16]);
        pos += PRINT_PARAMS_SIZE;
        let slicer_addr = pos;
        body.f32(b.lift.distance2)
            .f32(b.lift.speed2)
            .f32(n.lift.distance2)
            .f32(n.lift.speed2)
            .f32(n.retract.distance2)
            .f32(n.retract.speed2)
            .f32(n_rest[1])
            .u32(0) // per-layer settings off: printers use the global values
            .u32(self.timestamp_min)
            .u32(s.anti_aliasing as u32)
            .u32(0) // software version
            .f32(n_rest[2])
            .f32(n_rest[0])
            .f32(n.exposure)
            .f32(b.exposure)
            .f32(n_rest[1])
            .u32(s.transition_layers)
            .u32(0)
            .u32(0);
        pos += SLICER_INFO_SIZE;
        let defs_addr = pos;
        for i in 0..self.layer_count as usize {
            self.defs.get(i).copied().unwrap_or_default().encode(&mut body);
        }
        let header = V4Header {
            version: VERSION,
            model,
            resolution_x: p.resolution_x as u16,
            resolution_y: p.resolution_y as u16,
            bed_x: p.size_x,
            bed_y: p.size_y,
            bed_z: p.size_z,
            print_height: if self.last_z > 0.0 {
                self.last_z
            } else {
                info.layer_count as f32 * s.layer_height
            },
            layer_height: s.layer_height,
            bottom_layers: s.bottom_layers,
            preview_small: preview_addr[0],
            layer_defs: defs_addr,
            layer_count: self.layer_count,
            preview_large: preview_addr[1],
            print_time_s: est.map(|e| e.print_time_s.round() as u32).unwrap_or(0),
            projector_type: p.mirror.is_mirrored() as u32,
            print_params: params_addr,
            print_params_size: PRINT_PARAMS_SIZE,
            anti_aliasing: s.anti_aliasing as u32,
            light_pwm: n.light_pwm as u16,
            bottom_light_pwm: b.light_pwm as u16,
            encryption_key: 0,
            slicer_info: slicer_addr,
            slicer_info_size: SLICER_INFO_SIZE,
        };
        let mut w = LeWriter::default();
        header.encode(&mut w);
        debug_assert_eq!(w.buf.len() as u32, V4Header::size(&header.model));
        w.bytes(&body.buf);
        w.buf
    }
}

impl FormatWriter for CxdlpV4Writer {
    fn begin(&mut self, info: &JobInfo) -> Result<()> {
        let p = info.printer;
        let s = info.settings;
        if p.resolution_x > u16::MAX as u32 || p.resolution_y > u16::MAX as u32 {
            return Err(FormatError::Unsupported(
                "CXDLPv4 resolution is limited to 65535 pixels".into(),
            ));
        }
        self.codec = self.codec_override.unwrap_or_else(|| codec_for(p));
        if self.codec == V4Codec::PixelPair && p.resolution_x % 2 != 0 {
            return Err(FormatError::Unsupported(
                "the pixel pair codec needs an even width".into(),
            ));
        }
        self.params = [s.bottom, s.normal];
        self.wait_mode = s.wait_mode;
        self.layer_count = info.layer_count;
        self.defs = Vec::with_capacity(info.layer_count as usize);
        self.last_z = 0.0;
        self.timestamp_min = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| (d.as_secs() / 60) as u32)
            .unwrap_or(0);
        let black = Thumbnail::new(1, 1, vec![0, 0, 0, 255]);
        let thumb = info.thumbnail.unwrap_or(&black);
        self.previews = [
            ctb::encode_preview(&thumb.fit(PREVIEW_SMALL.0, PREVIEW_SMALL.1)),
            ctb::encode_preview(&thumb.fit(PREVIEW_LARGE.0, PREVIEW_LARGE.1)),
        ];
        let head = self.build_head(info, None);
        self.head_len = head.len() as u64;
        self.next_data = self.head_len;
        self.crc = 0;
        self.data_len = 0;
        self.out.seek(SeekFrom::Start(0))?;
        self.out.write_all(&head)?;
        Ok(())
    }

    fn encode_layer(&self, _index: u32, raster: &LayerRaster) -> Result<Vec<u8>> {
        match self.codec {
            V4Codec::Rle7 => Ok(ctb::encode_rle(&raster.runs)),
            V4Codec::PixelPair => encode_pixel_pairs(raster),
        }
    }

    fn write_layer(&mut self, meta: &LayerMeta, encoded: Vec<u8>) -> Result<()> {
        let addr = self.next_data;
        let size = LAYER_DEF_EX_SIZE as u64 + encoded.len() as u64;
        if addr + size > u32::MAX as u64 {
            return Err(FormatError::Unsupported("CXDLPv4 files are limited to 4 GiB".into()));
        }
        let p = &self.params[if meta.is_bottom { 0 } else { 1 }];
        let (rest, off) = waits(p, self.wait_mode);
        let mut ex = LeWriter::default();
        ex.f32(p.lift.total_distance())
            .f32(p.lift.speed)
            .f32(p.lift.distance2)
            .f32(p.lift.speed2)
            .f32(p.retract.speed)
            .f32(p.retract.distance2)
            .f32(p.retract.speed2)
            .f32(rest[0])
            .f32(rest[1])
            .f32(rest[2])
            .f32(p.light_pwm as f32);
        self.out.write_all(&ex.buf)?;
        self.out.write_all(&encoded)?;
        self.crc = crc32_update(crc32_update(self.crc, &ex.buf), &encoded);
        self.data_len += size;
        self.defs.push(V4LayerDef {
            position_z: meta.position_z,
            exposure: meta.exposure,
            light_off_delay: off,
            data_address: addr as u32,
            data_size: size as u32,
            data_type: 0,
            area: (meta.area_mm2 * 1000.0).round().clamp(0.0, u32::MAX as f64) as u32,
        });
        self.last_z = meta.position_z;
        self.next_data = addr + size;
        Ok(())
    }

    fn finish(&mut self, info: &JobInfo, est: &PrintEstimate) -> Result<()> {
        if self.defs.len() as u32 != self.layer_count {
            return Err(FormatError::Invalid(format!(
                "wrote {} of {} layers",
                self.defs.len(),
                self.layer_count
            )));
        }
        let head = self.build_head(info, Some(est));
        if head.len() as u64 != self.head_len {
            return Err(FormatError::Invalid("CXDLPv4 header changed size".into()));
        }
        self.out.seek(SeekFrom::Start(0))?;
        self.out.write_all(&head)?;
        let crc = crc32_combine(crc32_update(0, &head), self.crc, self.data_len);
        self.out.seek(SeekFrom::End(0))?;
        self.out.write_all(&crc.to_be_bytes())?;
        self.out.flush()?;
        Ok(())
    }
}

// ---------------------------------------------------------------- reader

pub struct CxdlpV4File {
    pub header: V4Header,
    pub codec: V4Codec,
    pub layers: Vec<V4LayerDef>,
    /// (width, height, offset, length) of the preview used as thumbnail.
    preview: Option<(u32, u32, u32, u32)>,
    info: SlicedInfo,
    file: Mutex<BufReader<File>>,
}

fn read_at(f: &mut BufReader<File>, pos: u64, len: usize) -> Result<Vec<u8>> {
    f.seek(SeekFrom::Start(pos))?;
    let mut buf = vec![0u8; len];
    f.read_exact(&mut buf)?;
    Ok(buf)
}

impl CxdlpV4File {
    pub fn open(path: &Path) -> Result<Self> {
        let mut f = BufReader::new(File::open(path)?);
        let len = f.get_ref().metadata()?.len();
        if len < 4 {
            return Err(FormatError::Invalid("not a CXDLPv4 file".into()));
        }
        let head = read_at(&mut f, 0, len.min(2048) as usize)?;
        let header = V4Header::decode(&head)?;
        if header.version < 4 {
            return Err(FormatError::Unsupported(format!(
                "CXDLP version {} (use the .cxdlp reader)",
                header.version
            )));
        }
        let stored = u32::from_be_bytes(read_at(&mut f, len - 4, 4)?.try_into().unwrap());
        let expected = file_crc32(&mut f, len - 4, false)?;
        if stored != expected {
            return Err(FormatError::Invalid(format!(
                "CXDLPv4 checksum mismatch (stored {stored:#x}, computed {expected:#x})"
            )));
        }
        let n = header.layer_count;
        if n as u64 * LAYER_DEF_SIZE as u64 > len {
            return Err(FormatError::Invalid(format!("CXDLPv4 file cannot hold {n} layers")));
        }
        let table = read_at(&mut f, header.layer_defs as u64, (n * LAYER_DEF_SIZE) as usize)?;
        let mut r = Reader::le(&table, 0);
        let layers = (0..n).map(|_| V4LayerDef::decode(&mut r)).collect::<Result<Vec<_>>>()?;
        for (i, d) in layers.iter().enumerate() {
            if d.data_size < LAYER_DEF_EX_SIZE || d.data_address as u64 + d.data_size as u64 > len {
                return Err(FormatError::Invalid(format!("layer {i} has an invalid data range")));
            }
        }
        let (mut exposure, mut bottom_exposure, mut volume_ml) = (0.0, 0.0, 0.0);
        if header.print_params != 0 {
            let pp = read_at(&mut f, header.print_params as u64, PRINT_PARAMS_SIZE as usize)?;
            let mut r = Reader::le(&pp, 20);
            volume_ml = r.f32()?;
            r.skip(4 * 5)?;
            exposure = r.f32()?;
            bottom_exposure = r.f32()?;
        }
        let mut preview = None;
        for addr in [header.preview_large, header.preview_small] {
            if addr != 0 && preview.is_none() {
                let b = read_at(&mut f, addr as u64, 16)?;
                let mut r = Reader::le(&b, 0);
                let p = (r.u32()?, r.u32()?, r.u32()?, r.u32()?);
                if p.0 > 0 && p.1 > 0 && p.0 <= 4096 && p.1 <= 4096 {
                    preview = Some(p);
                }
            }
        }
        let (w, h) = (header.resolution_x as u32, header.resolution_y as u32);
        let mut codec = V4Codec::Rle7;
        if let Some(d) = layers.first() {
            let mut data = read_at(
                &mut f,
                (d.data_address + LAYER_DEF_EX_SIZE) as u64,
                (d.data_size - LAYER_DEF_EX_SIZE) as usize,
            )?;
            ctb::crypt_layer(header.encryption_key, 0, &mut data);
            if d.data_type == 0 && is_pixel_pairs(&data, w, h) {
                codec = V4Codec::PixelPair;
            }
        }
        let info = SlicedInfo {
            format: "cxdlpv4".into(),
            machine_name: header.model.clone(),
            resolution_x: w,
            resolution_y: h,
            size_x: header.bed_x,
            size_y: header.bed_y,
            layer_count: n,
            layer_height: header.layer_height,
            exposure,
            bottom_exposure,
            bottom_layers: header.bottom_layers,
            print_time_s: header.print_time_s,
            volume_ml,
            mirror_x: header.projector_type != 0,
            mirror_y: false,
            anti_aliasing: header.anti_aliasing.max(1),
        };
        Ok(Self {
            header,
            codec,
            layers,
            preview,
            info,
            file: Mutex::new(f),
        })
    }
}

impl SlicedFile for CxdlpV4File {
    fn info(&self) -> &SlicedInfo {
        &self.info
    }

    fn layer(&self, index: u32) -> Result<LayerRaster> {
        let d = *self
            .layers
            .get(index as usize)
            .ok_or_else(|| FormatError::Invalid(format!("layer {index} out of range")))?;
        if d.data_type != 0 {
            return Err(FormatError::Unsupported(format!(
                "CXDLPv4 layer data type {}",
                d.data_type
            )));
        }
        let mut data = {
            let mut f = self.file.lock().unwrap();
            read_at(
                &mut f,
                (d.data_address + LAYER_DEF_EX_SIZE) as u64,
                (d.data_size - LAYER_DEF_EX_SIZE) as usize,
            )?
        };
        ctb::crypt_layer(self.header.encryption_key, index, &mut data);
        let (w, h) = (self.info.resolution_x, self.info.resolution_y);
        match self.codec {
            V4Codec::Rle7 => Ok(LayerRaster::from_runs(
                w,
                h,
                ctb::decode_rle(&data, w as u64 * h as u64)?,
            )),
            V4Codec::PixelPair => Ok(LayerRaster::from_gray(w, h, &decode_pixel_pairs(&data, w, h)?)),
        }
    }

    fn layer_z(&self, index: u32) -> f32 {
        self.layers.get(index as usize).map(|d| d.position_z).unwrap_or(0.0)
    }

    fn thumbnail(&self) -> Option<Thumbnail> {
        let (w, h, off, len) = self.preview?;
        let mut f = self.file.lock().ok()?;
        let data = read_at(&mut f, off as u64, len as usize).ok()?;
        Some(ctb::decode_preview(w, h, &data))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openresin_core::profile::{MirrorMode, PrintSettings};
    use openresin_core::slicer::Run;

    #[test]
    fn pixel_pair_known_bytes() {
        // one row of 6 pixels: (255,255) (255,255) (0,36)
        let r = LayerRaster::from_gray(6, 1, &[255, 255, 255, 255, 0, 36]);
        assert_eq!(
            encode_pixel_pairs(&r).unwrap(),
            vec![0x80 | 0x3F, 0x01, 0x80 | (1 << 3)]
        );
        // 200 equal units: repeat 199 = 0b1_1000111 -> groups 0x01 0x47
        let r = LayerRaster::empty(400, 2);
        assert_eq!(
            encode_pixel_pairs(&r).unwrap(),
            vec![0x80, 0x01, 0x47, 0x80, 0x01, 0x47]
        );
        assert_eq!(
            decode_pixel_pairs(&[0x80, 0x01, 0x47, 0x80, 0x01, 0x47], 400, 2).unwrap(),
            vec![0; 800]
        );
        // a run may not cross its row, nor stop short of the image
        assert!(decode_pixel_pairs(&[0x80, 0x01, 0x48, 0x80, 0x01, 0x46], 400, 2).is_err());
        assert!(decode_pixel_pairs(&[0x80, 0x01, 0x47], 400, 2).is_err());
        assert!(decode_pixel_pairs(&[0xC0], 2, 1).is_err());
        assert!(encode_pixel_pairs(&LayerRaster::empty(3, 1)).is_err());
        // too short for the rows: rejected before a 4 GiB buffer is allocated
        let e = decode_pixel_pairs(&[0x80, 0x01, 0x47], 65534, 65535).unwrap_err();
        assert!(e.to_string().contains("too short for 65535 rows"), "{e}");
    }

    #[test]
    fn pixel_pair_quantisation_and_roundtrip() {
        assert_eq!(quantize_pixel_pair(0), 0);
        assert_eq!(quantize_pixel_pair(17), 0);
        assert_eq!(quantize_pixel_pair(19), 36);
        assert_eq!(quantize_pixel_pair(128), 146);
        assert_eq!(quantize_pixel_pair(255), 255);
        for v in 0..=255u8 {
            assert!((quantize_pixel_pair(v) as i32 - v as i32).abs() <= 19, "{v}");
        }
        let (w, h) = (300u32, 40u32);
        let px: Vec<u8> = (0..w * h)
            .map(|i| if i % 97 < 40 { (i % 256) as u8 } else { 0 })
            .collect();
        let r = LayerRaster::from_gray(w, h, &px);
        let enc = encode_pixel_pairs(&r).unwrap();
        assert!(is_pixel_pairs(&enc, w, h));
        assert!(!is_pixel_pairs(&enc, w, h + 1));
        let want: Vec<u8> = px.iter().map(|&v| quantize_pixel_pair(v)).collect();
        assert_eq!(decode_pixel_pairs(&enc, w, h).unwrap(), want);
        // the classic codec is not mistaken for pixel pairs
        assert!(!is_pixel_pairs(&ctb::encode_rle(&r.runs), w, h));
        assert!(!is_pixel_pairs(&ctb::encode_rle(&[Run { value: 0, len: w * h }]), w, h));
    }

    /// A row of a HALOT-X1 sample, as published with the reference's codec
    /// tests: black, single pairs of every level, a run of (7, 7), black.
    #[test]
    fn pixel_pair_sample_row() {
        const W: u32 = 15120;
        #[rustfmt::skip]
        let row = [
            0x80, 0x19, 0x1A, 0x88, 0x9A, 0xA3, 0xAD, 0xB6, 0xBF, 0x0B, 0xB6, 0xAD, 0x9C, 0x93, 0x81,
            0x80, 0x21, 0x56,
        ];
        let empty_row = [0x80, 0x3B, 0x07];
        let data = [&row[..], &empty_row[..]].concat();
        // (left level, right level, pairs)
        let runs = [
            (0, 0, 3227),
            (0, 1, 1),
            (2, 3, 1),
            (3, 4, 1),
            (5, 5, 1),
            (6, 6, 1),
            (7, 7, 12),
            (6, 6, 1),
            (5, 5, 1),
            (4, 3, 1),
            (3, 2, 1),
            (1, 0, 1),
            (0, 0, 4311),
        ];
        let mut want = Vec::new();
        for (l, r, n) in runs {
            for _ in 0..n {
                want.extend([expand3(l), expand3(r)]);
            }
        }
        assert_eq!(want.len(), W as usize);
        want.resize(2 * W as usize, 0);
        assert!(is_pixel_pairs(&data, W, 2));
        let px = decode_pixel_pairs(&data, W, 2).unwrap();
        assert_eq!(px, want);
        assert_eq!(encode_pixel_pairs(&LayerRaster::from_gray(W, 2, &px)).unwrap(), data);
    }

    #[test]
    fn codec_selection() {
        let mut p = PrinterProfile::generic();
        assert_eq!(codec_for(&p), V4Codec::Rle7);
        (p.resolution_x, p.resolution_y) = (15120, 6230);
        assert_eq!(codec_for(&p), V4Codec::PixelPair);
        (p.resolution_x, p.resolution_y) = (8520, 4320);
        p.name = "HALOT-X1 Neo".into();
        assert_eq!(codec_for(&p), V4Codec::Rle7);
        p.machine_type = Some("HALOT-X1".into());
        assert_eq!(codec_for(&p), V4Codec::PixelPair);
    }

    #[test]
    fn model_names() {
        let printer = |name: &str, (resolution_x, resolution_y): (u32, u32)| PrinterProfile {
            vendor: "CREALITY".into(),
            name: name.into(),
            resolution_x,
            resolution_y,
            ..PrinterProfile::generic()
        };
        assert_eq!(model_name(&printer("HALOT-X1", (15120, 6230))), "HALOT-X1");
        assert_eq!(model_name(&printer("HALOT-X1 Max", (15120, 6230))), "HALOT-X1");
        assert_eq!(model_name(&printer("HALOT-X1 Neo", (8520, 4320))), "HALOT-X1 Neo");
        assert_eq!(model_name(&printer("HALOT-SKY 2025", (15120, 6230))), "HALOT-SKY 2025");
        assert_eq!(model_name(&printer("HALOT-MAGE PRO", (7680, 4320))), "CL-103");
        assert_eq!(model_name(&printer("HALOT-MAGE", (7680, 4320))), "CL-103L");
        // not the HALOT-ONE code (CL-60) although the resolution is the same
        assert_eq!(model_name(&printer("HALOT-R6", (1620, 2560))), "HALOT-R6");
        let mut p = printer("HALOT-X1", (15120, 6230));
        p.machine_type = Some("R009".into());
        assert_eq!(model_name(&p), "R009");
        // the vendor is not prepended (it was: "CREALITY HALOT-X1")
        let list = openresin_core::profile::builtin_printers();
        let get = |id: &str| model_name(openresin_core::profile::find_printer(&list, id).unwrap());
        assert_eq!(get("creality-halot-x1"), "HALOT-X1");
        assert_eq!(get("creality-halot-mage-pro"), "CL-103");
        assert_eq!(get("creality-halot-r6"), "HALOT-R6");
    }

    /// Writes a small file with the pixel pair codec and reads it back; the
    /// reader must detect the codec from the data.
    #[test]
    fn pixel_pair_file_roundtrip() {
        let path = std::env::temp_dir().join(format!("openresin-v4-{}.cxdlpv4", std::process::id()));
        let p = PrinterProfile {
            id: "t".into(),
            vendor: "Creality".into(),
            name: "HALOT test".into(),
            resolution_x: 64,
            resolution_y: 32,
            size_x: 32.0,
            size_y: 16.0,
            size_z: 50.0,
            mirror: MirrorMode::Lcd,
            formats: vec!["cxdlpv4".into()],
            native_formats: Vec::new(),
            network: None,
            machine_type: None,
        };
        let s = PrintSettings::default();
        let info = JobInfo {
            printer: &p,
            settings: &s,
            layer_count: 3,
            thumbnail: None,
            job_name: "t",
        };
        let mut w = CxdlpV4Writer::new(Box::new(File::create(&path).unwrap())).with_codec(V4Codec::PixelPair);
        w.begin(&info).unwrap();
        let mut images = Vec::new();
        for i in 0..3u32 {
            let px: Vec<u8> = (0..64 * 32u32)
                .map(|k| if (k + i) % 13 < 5 { 255 } else { (k % 7) as u8 * 40 })
                .collect();
            let r = LayerRaster::from_gray(64, 32, &px);
            let enc = w.encode_layer(i, &r).unwrap();
            let meta = LayerMeta {
                index: i,
                position_z: (i + 1) as f32 * 0.05,
                exposure: s.exposure_for_layer(i),
                is_bottom: i < s.bottom_layers,
                area_mm2: r.lit_area_px() * 0.25,
            };
            w.write_layer(&meta, enc).unwrap();
            images.push(px);
        }
        w.finish(&info, &PrintEstimate::default()).unwrap();
        drop(w);
        let f = CxdlpV4File::open(&path).unwrap();
        assert_eq!(f.codec, V4Codec::PixelPair);
        assert_eq!(f.header.model, "HALOT test");
        assert_eq!(f.info().layer_count, 3);
        for (i, px) in images.iter().enumerate() {
            let want: Vec<u8> = px.iter().map(|&v| quantize_pixel_pair(v)).collect();
            assert_eq!(f.layer(i as u32).unwrap().to_gray(), want);
        }
        assert!((f.layer_z(2) - 0.15).abs() < 1e-6);
        assert!(f.thumbnail().is_some());
        // a damaged byte is caught by the checksum
        let mut data = std::fs::read(&path).unwrap();
        let k = data.len() / 2;
        data[k] ^= 0x10;
        std::fs::write(&path, &data).unwrap();
        assert!(CxdlpV4File::open(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }
}
