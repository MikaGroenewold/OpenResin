//! Elegoo GOO format (public specification "Goo Format Spec V1.2").
//!
//! All numbers are big-endian. Layer images use a run-length scheme with an
//! 0x55 magic byte and a trailing checksum (bitwise NOT of the payload byte sum).

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Mutex;

use openresin_core::estimate::PrintEstimate;
use openresin_core::slicer::{LayerRaster, Run};

use crate::bin::{BeWriter, Reader};
use crate::preview::Thumbnail;
use crate::{FormatError, FormatWriter, JobInfo, LayerMeta, Result, SlicedFile, SlicedInfo, WriteSeek};

pub const VERSION: &[u8; 4] = b"V3.0";
pub const MAGIC: [u8; 8] = [0x07, 0x00, 0x00, 0x00, 0x44, 0x4C, 0x50, 0x00];
pub const ENDING: [u8; 11] = [0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x44, 0x4C, 0x50, 0x00];
pub const DELIMITER: [u8; 2] = [0x0D, 0x0A];
pub const SMALL_PREVIEW: u32 = 116;
pub const BIG_PREVIEW: u32 = 290;
pub const HEADER_SIZE: usize = 195_477;
const LAYER_DEF_SIZE: usize = 66;
const MAX_RUN: u32 = 0x0FFF_FFFF;

// ---------------------------------------------------------------- RLE codec

fn push_len(out: &mut Vec<u8>, kind: u8, gray: Option<u8>, len: u32) {
    debug_assert!(len > 0 && len <= MAX_RUN);
    let (flag, extra) = match len {
        0..=0xF => (0x00, 0),
        0x10..=0xFFF => (0x10, 1),
        0x1000..=0xF_FFFF => (0x20, 2),
        _ => (0x30, 3),
    };
    out.push(kind | flag | (len & 0xF) as u8);
    if let Some(g) = gray {
        out.push(g);
    }
    for k in (0..extra).rev() {
        out.push((len >> (4 + 8 * k)) as u8);
    }
}

/// Encodes runs into a GOO layer payload including magic and checksum.
pub fn encode_runs(runs: &[Run]) -> Vec<u8> {
    let mut out = Vec::with_capacity(runs.len() * 3 + 2);
    out.push(0x55);
    for r in runs {
        let mut left = r.len;
        while left > 0 {
            let n = left.min(MAX_RUN);
            match r.value {
                0 => push_len(&mut out, 0x00, None, n),
                255 => push_len(&mut out, 0xC0, None, n),
                g => push_len(&mut out, 0x40, Some(g), n),
            }
            left -= n;
        }
    }
    let sum = out[1..].iter().fold(0u8, |a, &b| a.wrapping_add(b));
    out.push(!sum);
    out
}

/// Decodes a GOO layer payload (with magic and checksum) into runs.
pub fn decode_runs(data: &[u8], expected_pixels: u64) -> Result<Vec<Run>> {
    if data.len() < 2 || data[0] != 0x55 {
        return Err(FormatError::Invalid("layer data does not start with 0x55".into()));
    }
    let payload = &data[1..data.len() - 1];
    let sum = payload.iter().fold(0u8, |a, &b| a.wrapping_add(b));
    if !sum != data[data.len() - 1] {
        return Err(FormatError::Invalid("layer checksum mismatch".into()));
    }
    let mut runs: Vec<Run> = Vec::new();
    let mut prev: u8 = 0;
    let mut total = 0u64;
    let mut i = 0usize;
    let byte = |i: usize| {
        payload
            .get(i)
            .copied()
            .ok_or_else(|| FormatError::Invalid("truncated layer data".into()))
    };
    while i < payload.len() {
        let b0 = payload[i];
        i += 1;
        let kind = b0 >> 6;
        let (value, len) = if kind == 0b10 {
            let diff = b0 & 0x0F;
            let len = if b0 & 0x10 != 0 {
                let l = byte(i)? as u32;
                i += 1;
                l
            } else {
                1
            };
            let v = if b0 & 0x20 != 0 {
                prev.wrapping_sub(diff)
            } else {
                prev.wrapping_add(diff)
            };
            (v, len)
        } else {
            let value = match kind {
                0b00 => 0,
                0b11 => 255,
                _ => {
                    let g = byte(i)?;
                    i += 1;
                    g
                }
            };
            let extra = ((b0 >> 4) & 0x3) as usize;
            let mut len = (b0 & 0x0F) as u32;
            for k in 0..extra {
                len |= (byte(i + k)? as u32) << (4 + 8 * (extra - 1 - k));
            }
            i += extra;
            (value, len)
        };
        if len == 0 {
            continue;
        }
        prev = value;
        total += len as u64;
        match runs.last_mut() {
            Some(last) if last.value == value => last.len += len,
            _ => runs.push(Run { value, len }),
        }
    }
    if total != expected_pixels {
        return Err(FormatError::Invalid(format!(
            "layer decodes to {total} pixels, expected {expected_pixels}"
        )));
    }
    Ok(runs)
}

// ---------------------------------------------------------------- header

#[derive(Debug, Clone, Default)]
pub struct GooHeader {
    pub software_info: String,
    pub software_version: String,
    pub file_time: String,
    pub printer_name: String,
    pub printer_type: String,
    pub profile_name: String,
    pub anti_aliasing: u16,
    pub grey_level: u16,
    pub blur_level: u16,
    pub small_preview: Vec<u16>,
    pub big_preview: Vec<u16>,
    pub total_layers: u32,
    pub resolution_x: u16,
    pub resolution_y: u16,
    pub mirror_x: bool,
    pub mirror_y: bool,
    pub size_x: f32,
    pub size_y: f32,
    pub size_z: f32,
    pub layer_height: f32,
    pub exposure: f32,
    pub delay_mode_static: bool,
    pub turn_off_time: f32,
    pub bottom_before_lift: f32,
    pub bottom_after_lift: f32,
    pub bottom_after_retract: f32,
    pub before_lift: f32,
    pub after_lift: f32,
    pub after_retract: f32,
    pub bottom_exposure: f32,
    pub bottom_layers: u32,
    pub bottom_lift_distance: f32,
    pub bottom_lift_speed: f32,
    pub lift_distance: f32,
    pub lift_speed: f32,
    pub bottom_retract_distance: f32,
    pub bottom_retract_speed: f32,
    pub retract_distance: f32,
    pub retract_speed: f32,
    pub bottom_lift_distance2: f32,
    pub bottom_lift_speed2: f32,
    pub lift_distance2: f32,
    pub lift_speed2: f32,
    pub bottom_retract_distance2: f32,
    pub bottom_retract_speed2: f32,
    pub retract_distance2: f32,
    pub retract_speed2: f32,
    pub bottom_light_pwm: u16,
    pub light_pwm: u16,
    pub advance_mode: bool,
    pub print_time: u32,
    pub volume: f32,
    pub weight: f32,
    pub price: f32,
    pub price_unit: String,
    pub layer_content_offset: u32,
    pub grey_scale_8bit: bool,
    pub transition_layers: u16,
}

impl GooHeader {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = BeWriter::with_capacity(HEADER_SIZE);
        w.bytes(VERSION).bytes(&MAGIC);
        w.str_fixed(&self.software_info, 32)
            .str_fixed(&self.software_version, 24)
            .str_fixed(&self.file_time, 24)
            .str_fixed(&self.printer_name, 32)
            .str_fixed(&self.printer_type, 32)
            .str_fixed(&self.profile_name, 32);
        w.u16(self.anti_aliasing).u16(self.grey_level).u16(self.blur_level);
        let preview = |w: &mut BeWriter, px: &[u16], n: u32| {
            for i in 0..(n * n) as usize {
                w.u16(px.get(i).copied().unwrap_or(0));
            }
            w.bytes(&DELIMITER);
        };
        preview(&mut w, &self.small_preview, SMALL_PREVIEW);
        preview(&mut w, &self.big_preview, BIG_PREVIEW);
        w.u32(self.total_layers).u16(self.resolution_x).u16(self.resolution_y);
        w.u8(self.mirror_x as u8).u8(self.mirror_y as u8);
        w.f32(self.size_x).f32(self.size_y).f32(self.size_z);
        w.f32(self.layer_height)
            .f32(self.exposure)
            .u8(self.delay_mode_static as u8)
            .f32(self.turn_off_time);
        w.f32(self.bottom_before_lift)
            .f32(self.bottom_after_lift)
            .f32(self.bottom_after_retract)
            .f32(self.before_lift)
            .f32(self.after_lift)
            .f32(self.after_retract);
        w.f32(self.bottom_exposure).u32(self.bottom_layers);
        for v in [
            self.bottom_lift_distance,
            self.bottom_lift_speed,
            self.lift_distance,
            self.lift_speed,
            self.bottom_retract_distance,
            self.bottom_retract_speed,
            self.retract_distance,
            self.retract_speed,
            self.bottom_lift_distance2,
            self.bottom_lift_speed2,
            self.lift_distance2,
            self.lift_speed2,
            self.bottom_retract_distance2,
            self.bottom_retract_speed2,
            self.retract_distance2,
            self.retract_speed2,
        ] {
            w.f32(v);
        }
        w.u16(self.bottom_light_pwm)
            .u16(self.light_pwm)
            .u8(self.advance_mode as u8);
        w.u32(self.print_time).f32(self.volume).f32(self.weight).f32(self.price);
        w.str_fixed(&self.price_unit, 8);
        w.u32(self.layer_content_offset)
            .u8(self.grey_scale_8bit as u8)
            .u16(self.transition_layers);
        debug_assert_eq!(w.buf.len(), HEADER_SIZE);
        w.buf
    }

    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut r = Reader::be(data, 0);
        if r.take(4)? != VERSION.as_slice() && !data.starts_with(b"V") {
            return Err(FormatError::Invalid("not a GOO file".into()));
        }
        if r.take(8)? != MAGIC {
            return Err(FormatError::Invalid("bad GOO magic".into()));
        }
        let mut h = GooHeader {
            software_info: r.str_fixed(32)?,
            software_version: r.str_fixed(24)?,
            file_time: r.str_fixed(24)?,
            printer_name: r.str_fixed(32)?,
            printer_type: r.str_fixed(32)?,
            profile_name: r.str_fixed(32)?,
            anti_aliasing: r.u16()?,
            grey_level: r.u16()?,
            blur_level: r.u16()?,
            ..Default::default()
        };
        h.small_preview = (0..SMALL_PREVIEW * SMALL_PREVIEW)
            .map(|_| r.u16())
            .collect::<Result<_>>()?;
        r.skip(2)?;
        h.big_preview = (0..BIG_PREVIEW * BIG_PREVIEW).map(|_| r.u16()).collect::<Result<_>>()?;
        r.skip(2)?;
        h.total_layers = r.u32()?;
        h.resolution_x = r.u16()?;
        h.resolution_y = r.u16()?;
        h.mirror_x = r.u8()? != 0;
        h.mirror_y = r.u8()? != 0;
        h.size_x = r.f32()?;
        h.size_y = r.f32()?;
        h.size_z = r.f32()?;
        h.layer_height = r.f32()?;
        h.exposure = r.f32()?;
        h.delay_mode_static = r.u8()? != 0;
        h.turn_off_time = r.f32()?;
        h.bottom_before_lift = r.f32()?;
        h.bottom_after_lift = r.f32()?;
        h.bottom_after_retract = r.f32()?;
        h.before_lift = r.f32()?;
        h.after_lift = r.f32()?;
        h.after_retract = r.f32()?;
        h.bottom_exposure = r.f32()?;
        h.bottom_layers = r.u32()?;
        h.bottom_lift_distance = r.f32()?;
        h.bottom_lift_speed = r.f32()?;
        h.lift_distance = r.f32()?;
        h.lift_speed = r.f32()?;
        h.bottom_retract_distance = r.f32()?;
        h.bottom_retract_speed = r.f32()?;
        h.retract_distance = r.f32()?;
        h.retract_speed = r.f32()?;
        h.bottom_lift_distance2 = r.f32()?;
        h.bottom_lift_speed2 = r.f32()?;
        h.lift_distance2 = r.f32()?;
        h.lift_speed2 = r.f32()?;
        h.bottom_retract_distance2 = r.f32()?;
        h.bottom_retract_speed2 = r.f32()?;
        h.retract_distance2 = r.f32()?;
        h.retract_speed2 = r.f32()?;
        h.bottom_light_pwm = r.u16()?;
        h.light_pwm = r.u16()?;
        h.advance_mode = r.u8()? != 0;
        h.print_time = r.u32()?;
        h.volume = r.f32()?;
        h.weight = r.f32()?;
        h.price = r.f32()?;
        h.price_unit = r.str_fixed(8)?;
        h.layer_content_offset = r.u32()?;
        h.grey_scale_8bit = r.u8()? != 0;
        h.transition_layers = r.u16()?;
        Ok(h)
    }
}

/// Per-layer definition block.
#[derive(Debug, Clone, Copy, Default)]
pub struct GooLayerDef {
    pub pause: bool,
    pub pause_z: f32,
    pub position_z: f32,
    pub exposure: f32,
    pub off_time: f32,
    pub before_lift: f32,
    pub after_lift: f32,
    pub after_retract: f32,
    pub lift_distance: f32,
    pub lift_speed: f32,
    pub lift_distance2: f32,
    pub lift_speed2: f32,
    pub retract_distance: f32,
    pub retract_speed: f32,
    pub retract_distance2: f32,
    pub retract_speed2: f32,
    pub light_pwm: u16,
}

impl GooLayerDef {
    fn encode(&self, w: &mut BeWriter) {
        w.u16(self.pause as u16);
        for v in [
            self.pause_z,
            self.position_z,
            self.exposure,
            self.off_time,
            self.before_lift,
            self.after_lift,
            self.after_retract,
            self.lift_distance,
            self.lift_speed,
            self.lift_distance2,
            self.lift_speed2,
            self.retract_distance,
            self.retract_speed,
            self.retract_distance2,
            self.retract_speed2,
        ] {
            w.f32(v);
        }
        w.u16(self.light_pwm).bytes(&DELIMITER);
    }

    fn decode(r: &mut Reader) -> Result<Self> {
        let d = GooLayerDef {
            pause: r.u16()? != 0,
            pause_z: r.f32()?,
            position_z: r.f32()?,
            exposure: r.f32()?,
            off_time: r.f32()?,
            before_lift: r.f32()?,
            after_lift: r.f32()?,
            after_retract: r.f32()?,
            lift_distance: r.f32()?,
            lift_speed: r.f32()?,
            lift_distance2: r.f32()?,
            lift_speed2: r.f32()?,
            retract_distance: r.f32()?,
            retract_speed: r.f32()?,
            retract_distance2: r.f32()?,
            retract_speed2: r.f32()?,
            light_pwm: r.u16()?,
        };
        if r.take(2)? != DELIMITER {
            return Err(FormatError::Invalid("missing layer definition delimiter".into()));
        }
        Ok(d)
    }
}

// ---------------------------------------------------------------- writer

pub struct GooWriter {
    out: Box<dyn WriteSeek>,
    header: GooHeader,
    defs: Vec<GooLayerDef>,
}

impl GooWriter {
    pub fn new(out: Box<dyn WriteSeek>) -> Self {
        Self {
            out,
            header: GooHeader::default(),
            defs: Vec::new(),
        }
    }
}

fn now_string() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    secs.to_string()
}

impl FormatWriter for GooWriter {
    fn begin(&mut self, info: &JobInfo) -> Result<()> {
        let p = info.printer;
        let s = info.settings;
        let (small, big) = match info.thumbnail {
            Some(t) => (
                t.fit(SMALL_PREVIEW, SMALL_PREVIEW).to_rgb565(),
                t.fit(BIG_PREVIEW, BIG_PREVIEW).to_rgb565(),
            ),
            None => (Vec::new(), Vec::new()),
        };
        let b = &s.bottom;
        let n = &s.normal;
        self.header = GooHeader {
            software_info: "OpenResin".into(),
            software_version: env!("CARGO_PKG_VERSION").into(),
            file_time: now_string(),
            printer_name: p.machine_type(),
            printer_type: p.machine_type(),
            profile_name: s.name.clone(),
            anti_aliasing: s.anti_aliasing as u16,
            grey_level: 0,
            blur_level: 0,
            small_preview: small,
            big_preview: big,
            total_layers: info.layer_count,
            resolution_x: p.resolution_x as u16,
            resolution_y: p.resolution_y as u16,
            // header flags follow the reference slicer: LCD mirroring is "X mirror"
            mirror_x: p.mirror.is_mirrored() && p.mirror != openresin_core::profile::MirrorMode::Y,
            mirror_y: p.mirror == openresin_core::profile::MirrorMode::Y,
            size_x: p.size_x,
            size_y: p.size_y,
            size_z: p.size_z,
            layer_height: s.layer_height,
            exposure: n.exposure,
            delay_mode_static: s.wait_mode,
            turn_off_time: n.light_off_delay,
            bottom_before_lift: b.wait_before_lift,
            bottom_after_lift: b.wait_after_lift,
            bottom_after_retract: b.wait_after_retract,
            before_lift: n.wait_before_lift,
            after_lift: n.wait_after_lift,
            after_retract: n.wait_after_retract,
            bottom_exposure: b.exposure,
            bottom_layers: s.bottom_layers,
            bottom_lift_distance: b.lift.distance,
            bottom_lift_speed: b.lift.speed,
            lift_distance: n.lift.distance,
            lift_speed: n.lift.speed,
            bottom_retract_distance: b.retract.distance,
            bottom_retract_speed: b.retract.speed,
            retract_distance: n.retract.distance,
            retract_speed: n.retract.speed,
            bottom_lift_distance2: b.lift.distance2,
            bottom_lift_speed2: b.lift.speed2,
            lift_distance2: n.lift.distance2,
            lift_speed2: n.lift.speed2,
            bottom_retract_distance2: b.retract.distance2,
            bottom_retract_speed2: b.retract.speed2,
            retract_distance2: n.retract.distance2,
            retract_speed2: n.retract.speed2,
            bottom_light_pwm: b.light_pwm as u16,
            light_pwm: n.light_pwm as u16,
            advance_mode: false,
            print_time: 0,
            volume: 0.0,
            weight: 0.0,
            price: 0.0,
            price_unit: s.currency.clone(),
            layer_content_offset: HEADER_SIZE as u32,
            grey_scale_8bit: true,
            transition_layers: s.transition_layers.min(u16::MAX as u32) as u16,
        };
        self.out.seek(SeekFrom::Start(0))?;
        self.out.write_all(&self.header.encode())?;
        Ok(())
    }

    fn encode_layer(&self, _index: u32, raster: &LayerRaster) -> Result<Vec<u8>> {
        Ok(encode_runs(&raster.runs))
    }

    fn write_layer(&mut self, meta: &LayerMeta, encoded: Vec<u8>) -> Result<()> {
        let s = &self.header;
        let bottom = meta.is_bottom;
        let pick = |b: f32, n: f32| if bottom { b } else { n };
        let def = GooLayerDef {
            pause: false,
            pause_z: meta.position_z,
            position_z: meta.position_z,
            exposure: meta.exposure,
            off_time: s.turn_off_time,
            before_lift: pick(s.bottom_before_lift, s.before_lift),
            after_lift: pick(s.bottom_after_lift, s.after_lift),
            after_retract: pick(s.bottom_after_retract, s.after_retract),
            lift_distance: pick(s.bottom_lift_distance, s.lift_distance),
            lift_speed: pick(s.bottom_lift_speed, s.lift_speed),
            lift_distance2: pick(s.bottom_lift_distance2, s.lift_distance2),
            lift_speed2: pick(s.bottom_lift_speed2, s.lift_speed2),
            retract_distance: pick(s.bottom_retract_distance, s.retract_distance),
            retract_speed: pick(s.bottom_retract_speed, s.retract_speed),
            retract_distance2: pick(s.bottom_retract_distance2, s.retract_distance2),
            retract_speed2: pick(s.bottom_retract_speed2, s.retract_speed2),
            light_pwm: if bottom { s.bottom_light_pwm } else { s.light_pwm },
        };
        let mut w = BeWriter::with_capacity(LAYER_DEF_SIZE + 4);
        def.encode(&mut w);
        w.u32(encoded.len() as u32);
        self.out.write_all(&w.buf)?;
        self.out.write_all(&encoded)?;
        self.out.write_all(&DELIMITER)?;
        self.defs.push(def);
        Ok(())
    }

    fn finish(&mut self, _info: &JobInfo, est: &PrintEstimate) -> Result<()> {
        self.out.write_all(&ENDING)?;
        self.header.total_layers = self.defs.len() as u32;
        self.header.print_time = est.print_time_s.round() as u32;
        self.header.volume = est.volume_mm3 as f32;
        self.header.weight = est.weight_g as f32;
        self.header.price = est.cost as f32;
        self.out.seek(SeekFrom::Start(0))?;
        self.out.write_all(&self.header.encode())?;
        self.out.seek(SeekFrom::End(0))?;
        self.out.flush()?;
        Ok(())
    }
}

// ---------------------------------------------------------------- reader

pub struct GooFile {
    pub header: GooHeader,
    pub layers: Vec<(GooLayerDef, u64, u32)>,
    info: SlicedInfo,
    file: Mutex<BufReader<File>>,
}

impl GooFile {
    pub fn open(path: &Path) -> Result<Self> {
        let mut f = BufReader::new(File::open(path)?);
        let file_len = f.get_ref().metadata()?.len();
        let mut head = vec![0u8; HEADER_SIZE];
        f.read_exact(&mut head)?;
        let header = GooHeader::decode(&head)?;
        // every layer needs its definition, the size, magic + checksum and a delimiter
        let min_layer = (LAYER_DEF_SIZE + 4 + 2 + 2) as u64;
        if header.layer_content_offset as u64 + header.total_layers as u64 * min_layer > file_len {
            return Err(FormatError::Invalid(format!(
                "file is too short for {} layers",
                header.total_layers
            )));
        }
        let mut layers = Vec::with_capacity(header.total_layers as usize);
        let mut pos = header.layer_content_offset as u64;
        let mut buf = [0u8; LAYER_DEF_SIZE + 4];
        for i in 0..header.total_layers {
            f.seek(SeekFrom::Start(pos))?;
            f.read_exact(&mut buf)?;
            let mut r = Reader::be(&buf, 0);
            let def = GooLayerDef::decode(&mut r)?;
            let size = r.u32()?;
            let data_pos = pos + (LAYER_DEF_SIZE + 4) as u64;
            if data_pos + size as u64 + 2 > file_len {
                return Err(FormatError::Invalid(format!("layer {i} runs past the end of the file")));
            }
            layers.push((def, data_pos, size));
            pos = data_pos + size as u64 + 2;
        }
        let info = SlicedInfo {
            format: "goo".into(),
            machine_name: header.printer_name.clone(),
            resolution_x: header.resolution_x as u32,
            resolution_y: header.resolution_y as u32,
            size_x: header.size_x,
            size_y: header.size_y,
            layer_count: header.total_layers,
            layer_height: header.layer_height,
            exposure: header.exposure,
            bottom_exposure: header.bottom_exposure,
            bottom_layers: header.bottom_layers,
            print_time_s: header.print_time,
            volume_ml: header.volume / 1000.0,
            mirror_x: header.mirror_x,
            mirror_y: header.mirror_y,
            anti_aliasing: header.anti_aliasing as u32,
        };
        Ok(Self {
            header,
            layers,
            info,
            file: Mutex::new(f),
        })
    }

    pub fn layer_bytes(&self, index: u32) -> Result<Vec<u8>> {
        let (_, pos, size) = *self
            .layers
            .get(index as usize)
            .ok_or_else(|| FormatError::Invalid(format!("layer {index} out of range")))?;
        let mut f = self.file.lock().unwrap();
        f.seek(SeekFrom::Start(pos))?;
        let mut data = vec![0u8; size as usize];
        f.read_exact(&mut data)?;
        Ok(data)
    }
}

impl SlicedFile for GooFile {
    fn info(&self) -> &SlicedInfo {
        &self.info
    }

    fn layer(&self, index: u32) -> Result<LayerRaster> {
        let data = self.layer_bytes(index)?;
        let (w, h) = (self.info.resolution_x, self.info.resolution_y);
        let runs = decode_runs(&data, w as u64 * h as u64)?;
        Ok(LayerRaster::from_runs(w, h, runs))
    }

    fn layer_z(&self, index: u32) -> f32 {
        self.layers.get(index as usize).map(|l| l.0.position_z).unwrap_or(0.0)
    }

    fn thumbnail(&self) -> Option<Thumbnail> {
        if self.header.big_preview.iter().all(|&p| p == 0) {
            return None;
        }
        Some(Thumbnail::from_rgb565(
            BIG_PREVIEW,
            BIG_PREVIEW,
            &self.header.big_preview,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_examples_decode() {
        // Examples from the specification (wrapped with magic + checksum).
        let wrap = |payload: &[u8]| {
            let mut v = vec![0x55];
            v.extend_from_slice(payload);
            let s = payload.iter().fold(0u8, |a, &b| a.wrapping_add(b));
            v.push(!s);
            v
        };
        let r = decode_runs(&wrap(&[0b0011_1111, 0x55, 0x56, 0x57]), 0x555657f).unwrap();
        assert_eq!(
            r,
            vec![Run {
                value: 0,
                len: 0x555657f
            }]
        );
        let r = decode_runs(&wrap(&[0b0111_0101, 0xaa, 0xbb, 0xcc, 0x15]), 0xbbcc155).unwrap();
        assert_eq!(
            r,
            vec![Run {
                value: 0xaa,
                len: 0xbbcc155
            }]
        );
        let r = decode_runs(&wrap(&[0b0000_0101]), 5).unwrap();
        assert_eq!(r, vec![Run { value: 0, len: 5 }]);
        // gray 0x10, +2 diff for 0xff pixels, -1 diff once
        let r = decode_runs(
            &wrap(&[0b0100_0001, 0x10, 0b1001_0010, 0xff, 0b1010_0001]),
            1 + 0xff + 1,
        )
        .unwrap();
        assert_eq!(
            r,
            vec![
                Run { value: 0x10, len: 1 },
                Run { value: 0x12, len: 0xff },
                Run { value: 0x11, len: 1 }
            ]
        );
    }

    #[test]
    fn encode_decode_roundtrip() {
        let runs = vec![
            Run { value: 0, len: 3 },
            Run { value: 255, len: 0x10 },
            Run { value: 7, len: 0x1000 },
            Run { value: 0, len: 0x12345 },
            Run { value: 200, len: 1 },
            Run {
                value: 255,
                len: 0x0FFF_FFFF + 5,
            },
        ];
        let total: u64 = runs.iter().map(|r| r.len as u64).sum();
        let enc = encode_runs(&runs);
        assert_eq!(decode_runs(&enc, total).unwrap(), runs);
    }

    #[test]
    fn header_size() {
        assert_eq!(GooHeader::default().encode().len(), HEADER_SIZE);
    }

    /// Validates the decoder against a real file produced by another slicer.
    /// Run with `OPENRESIN_GOO_SAMPLE=/path/file.goo cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn decode_reference_file() {
        let Ok(path) = std::env::var("OPENRESIN_GOO_SAMPLE") else {
            return;
        };
        let f = GooFile::open(Path::new(&path)).unwrap();
        for i in 0..f.info().layer_count {
            let l = f.layer(i).unwrap();
            assert_eq!(l.pixel_count(), (f.info().resolution_x * f.info().resolution_y) as u64);
            // re-encoding must decode to the same image
            let again = decode_runs(&encode_runs(&l.runs), l.pixel_count()).unwrap();
            assert_eq!(LayerRaster::from_runs(l.width, l.height, again).runs, l.runs);
        }
    }
}
