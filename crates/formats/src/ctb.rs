//! Chitubox CTB, classic layout (magic `0x12FD0086`, version 3).
//!
//! This is the long-standing, publicly documented little-endian layout with a
//! per-layer XOR keystream seeded from the header. The newer CTB container with
//! an encrypted header (magic `0x12FD0107`) is detected and reported as
//! unsupported – OpenResin does not ship vendor encryption keys.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Mutex;

use openresin_core::estimate::PrintEstimate;
use openresin_core::slicer::{LayerRaster, Run};

use crate::bin::{LeWriter, Reader};
use crate::preview::Thumbnail;
use crate::{FormatError, FormatWriter, JobInfo, LayerMeta, Result, SlicedFile, SlicedInfo, WriteSeek};

pub const MAGIC_CBDDLP: u32 = 0x12FD_0019;
pub const MAGIC_CTB: u32 = 0x12FD_0086;
pub const MAGIC_CTB_V4: u32 = 0x12FD_0106;
pub const MAGIC_CTB_ENCRYPTED: u32 = 0x12FD_0107;
const HEADER_SIZE: u32 = 112;
const PREVIEW_HEADER_SIZE: u32 = 32;
const PRINT_PARAMS_SIZE: u32 = 60;
const SLICER_INFO_SIZE: u32 = 76;
const LAYER_DEF_SIZE: u32 = 36;
const LAYER_DEF_EX_SIZE: u32 = 84;
const LARGE_PREVIEW: (u32, u32) = (400, 300);
const SMALL_PREVIEW: (u32, u32) = (200, 125);

// ---------------------------------------------------------------- codecs

/// XOR keystream used by classic CTB files for layer data. Symmetric.
pub fn crypt_layer(seed: u32, layer_index: u32, data: &mut [u8]) {
    if seed == 0 {
        return;
    }
    let init = seed.wrapping_mul(0x2d83_cdac).wrapping_add(0xd8a8_3423);
    let mut key = layer_index
        .wrapping_mul(0x1e15_30cd)
        .wrapping_add(0xec3d_47cd)
        .wrapping_mul(init);
    for (i, b) in data.iter_mut().enumerate() {
        *b ^= (key >> (8 * (i % 4))) as u8;
        if i % 4 == 3 {
            key = key.wrapping_add(init);
        }
    }
}

/// 7-bit greyscale RLE used for CTB layer images.
pub fn encode_rle(runs: &[Run]) -> Vec<u8> {
    let mut out = Vec::with_capacity(runs.len() * 2 + 4);
    let mut push = |value: u8, len: u32| {
        let code = value >> 1;
        if len == 1 {
            out.push(code);
            return;
        }
        out.push(code | 0x80);
        match len {
            0..=0x7F => out.push(len as u8),
            0x80..=0x3FFF => {
                out.push((len >> 8) as u8 | 0x80);
                out.push(len as u8);
            }
            0x4000..=0x1F_FFFF => {
                out.push((len >> 16) as u8 | 0xC0);
                out.push((len >> 8) as u8);
                out.push(len as u8);
            }
            _ => {
                out.push((len >> 24) as u8 | 0xE0);
                out.push((len >> 16) as u8);
                out.push((len >> 8) as u8);
                out.push(len as u8);
            }
        }
    };
    // merge runs that become equal after 7-bit quantisation
    let mut cur: Option<(u8, u32)> = None;
    for r in runs {
        let v = r.value >> 1;
        match &mut cur {
            Some((cv, cl)) if *cv == v => *cl += r.len,
            _ => {
                if let Some((cv, cl)) = cur.take() {
                    emit(&mut push, cv, cl);
                }
                cur = Some((v, r.len));
            }
        }
    }
    if let Some((cv, cl)) = cur {
        emit(&mut push, cv, cl);
    }
    out
}

fn emit(push: &mut impl FnMut(u8, u32), v7: u8, mut len: u32) {
    const MAX: u32 = 0x0FFF_FFFF;
    while len > 0 {
        let n = len.min(MAX);
        push(v7 << 1, n);
        len -= n;
    }
}

pub fn decode_rle(data: &[u8], expected_pixels: u64) -> Result<Vec<Run>> {
    let mut runs: Vec<Run> = Vec::new();
    let mut total = 0u64;
    let mut i = 0usize;
    let get = |i: usize| {
        data.get(i)
            .copied()
            .ok_or_else(|| FormatError::Invalid("truncated CTB layer".into()))
    };
    while i < data.len() {
        let code = data[i];
        i += 1;
        let v7 = code & 0x7F;
        let value = if v7 == 0 { 0 } else { (v7 << 1) | (v7 & 1) };
        let mut len = 1u32;
        if code & 0x80 != 0 {
            let b = get(i)?;
            i += 1;
            len = if b & 0x80 == 0 {
                b as u32
            } else if b & 0xC0 == 0x80 {
                let l = (((b & 0x3F) as u32) << 8) | get(i)? as u32;
                i += 1;
                l
            } else if b & 0xE0 == 0xC0 {
                let l = (((b & 0x1F) as u32) << 16) | ((get(i)? as u32) << 8) | get(i + 1)? as u32;
                i += 2;
                l
            } else if b & 0xF0 == 0xE0 {
                let l = (((b & 0x0F) as u32) << 24)
                    | ((get(i)? as u32) << 16)
                    | ((get(i + 1)? as u32) << 8)
                    | get(i + 2)? as u32;
                i += 3;
                l
            } else {
                return Err(FormatError::Invalid("bad CTB run length".into()));
            };
        }
        if len == 0 {
            continue;
        }
        total += len as u64;
        match runs.last_mut() {
            Some(last) if last.value == value => last.len += len,
            _ => runs.push(Run { value, len }),
        }
    }
    if total != expected_pixels {
        return Err(FormatError::Invalid(format!(
            "CTB layer decodes to {total} pixels, expected {expected_pixels}"
        )));
    }
    Ok(runs)
}

/// RGB15 RLE used for previews: 5-5-5 colour with bit 5 as repeat flag.
pub fn encode_preview(t: &Thumbnail) -> Vec<u8> {
    let mut out = Vec::new();
    let colors: Vec<u16> = t
        .rgba
        .chunks_exact(4)
        .map(|p| ((p[0] as u16 >> 3) << 11) | ((p[1] as u16 >> 3) << 6) | (p[2] as u16 >> 3))
        .collect();
    let mut i = 0;
    while i < colors.len() {
        let c = colors[i];
        let mut n = 1;
        while i + n < colors.len() && colors[i + n] == c && n < 0x1000 {
            n += 1;
        }
        if n > 1 {
            out.extend_from_slice(&(c | 0x20).to_le_bytes());
            out.extend_from_slice(&(((n - 1) as u16) | 0x3000).to_le_bytes());
        } else {
            out.extend_from_slice(&c.to_le_bytes());
        }
        i += n;
    }
    out
}

pub fn decode_preview(w: u32, h: u32, data: &[u8]) -> Thumbnail {
    let total = w as usize * h as usize;
    let mut rgba = Vec::with_capacity(total * 4);
    let mut i = 0;
    while i + 1 < data.len() && rgba.len() / 4 < total {
        let c = u16::from_le_bytes([data[i], data[i + 1]]);
        i += 2;
        let mut n = 1usize;
        if c & 0x20 != 0 && i + 1 < data.len() {
            n += (u16::from_le_bytes([data[i], data[i + 1]]) & 0x0FFF) as usize;
            i += 2;
        }
        let r = ((c >> 11) & 0x1F) as u8;
        let g = ((c >> 6) & 0x1F) as u8;
        let b = (c & 0x1F) as u8;
        for _ in 0..n.min(total - rgba.len() / 4) {
            rgba.extend_from_slice(&[r << 3, g << 3, b << 3, 255]);
        }
    }
    rgba.resize(total * 4, 0);
    Thumbnail::new(w, h, rgba)
}

// ---------------------------------------------------------------- writer

pub struct CtbWriter {
    out: Box<dyn WriteSeek>,
    seed: u32,
    header: LeWriter,
    layer_table_offset: u32,
    layer_count: u32,
    next_data_offset: u64,
    written: u32,
    print_params_offset: u32,
    machine_name: String,
    settings: Option<openresin_core::profile::PrintSettings>,
}

impl CtbWriter {
    pub fn new(out: Box<dyn WriteSeek>) -> Self {
        // any non-zero seed works; derive one from the clock
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| (d.as_nanos() as u32) | 0x100)
            .unwrap_or(0x1234_5678);
        Self {
            out,
            seed,
            header: LeWriter::default(),
            layer_table_offset: 0,
            layer_count: 0,
            next_data_offset: 0,
            written: 0,
            print_params_offset: 0,
            machine_name: String::new(),
            settings: None,
        }
    }

    fn write_header(&mut self, info: &JobInfo, est: Option<&PrintEstimate>, offsets: [u32; 4]) -> Result<()> {
        let [large, small, params, slicer] = offsets;
        let p = info.printer;
        let s = info.settings;
        let mut w = LeWriter::default();
        w.u32(MAGIC_CTB).u32(3);
        w.f32(p.size_x).f32(p.size_y).f32(p.size_z);
        w.u32(0).u32(0);
        w.f32(info.layer_count as f32 * s.layer_height);
        w.f32(s.layer_height)
            .f32(s.normal.exposure)
            .f32(s.bottom.exposure)
            .f32(s.normal.light_off_delay);
        w.u32(s.bottom_layers).u32(p.resolution_x).u32(p.resolution_y);
        w.u32(large)
            .u32(self.layer_table_offset)
            .u32(info.layer_count)
            .u32(small);
        w.u32(est.map(|e| e.print_time_s.round() as u32).unwrap_or(0));
        w.u32(p.mirror.is_mirrored() as u32);
        w.u32(params).u32(PRINT_PARAMS_SIZE);
        w.u32(s.anti_aliasing as u32);
        w.u16(s.normal.light_pwm as u16).u16(s.bottom.light_pwm as u16);
        w.u32(self.seed);
        w.u32(slicer).u32(SLICER_INFO_SIZE);
        debug_assert_eq!(w.buf.len() as u32, HEADER_SIZE);
        self.header = w;
        Ok(())
    }
}

impl FormatWriter for CtbWriter {
    fn begin(&mut self, info: &JobInfo) -> Result<()> {
        let s = info.settings;
        self.layer_count = info.layer_count;
        self.settings = Some(s.clone());
        self.machine_name = info.printer.machine_type();
        let mut body = LeWriter::default();
        let mut pos = HEADER_SIZE;
        let mut preview_offsets = [0u32; 2];
        if let Some(t) = info.thumbnail {
            for (k, (pw, ph)) in [LARGE_PREVIEW, SMALL_PREVIEW].into_iter().enumerate() {
                let data = encode_preview(&t.fit(pw, ph));
                preview_offsets[k] = pos;
                body.u32(pw)
                    .u32(ph)
                    .u32(pos + PREVIEW_HEADER_SIZE)
                    .u32(data.len() as u32);
                body.u32(0).u32(0).u32(0).u32(0).bytes(&data);
                pos += PREVIEW_HEADER_SIZE + data.len() as u32;
            }
        }
        // print parameters (volume/weight/cost patched in finish)
        self.print_params_offset = pos;
        body.f32(s.bottom.lift.total_distance())
            .f32(s.bottom.lift.speed)
            .f32(s.normal.lift.total_distance())
            .f32(s.normal.lift.speed)
            .f32(s.normal.retract.speed)
            .f32(0.0)
            .f32(0.0)
            .f32(0.0)
            .f32(s.bottom.light_off_delay)
            .f32(s.normal.light_off_delay)
            .u32(s.bottom_layers)
            .u32(0)
            .u32(0)
            .u32(0)
            .u32(0);
        pos += PRINT_PARAMS_SIZE;
        // slicer info + machine name
        let slicer_offset = pos;
        let name = self.machine_name.as_bytes();
        body.f32(s.bottom.lift.distance2)
            .f32(s.bottom.lift.speed2)
            .f32(s.normal.lift.distance2)
            .f32(s.normal.lift.speed2)
            .f32(s.normal.retract.distance2)
            .f32(s.normal.retract.speed2)
            .f32(s.normal.wait_after_lift)
            .u32(slicer_offset + SLICER_INFO_SIZE)
            .u32(name.len() as u32)
            .u8(0x07) // anti-alias flag
            .u16(0)
            .u8(0) // per-layer settings disabled: printers use the global values
            .u32(0)
            .u32(s.anti_aliasing as u32)
            .u32(0x0106_0300) // format compatible with 1.6.3
            .f32(s.normal.wait_after_retract)
            .f32(s.normal.wait_after_lift)
            .u32(s.transition_layers)
            .u32(0)
            .u32(0)
            .u32(0);
        body.bytes(name);
        pos += SLICER_INFO_SIZE + name.len() as u32;
        self.layer_table_offset = pos;
        pos += LAYER_DEF_SIZE * info.layer_count;
        self.next_data_offset = pos as u64;

        self.write_header(
            info,
            None,
            [
                preview_offsets[0],
                preview_offsets[1],
                self.print_params_offset,
                slicer_offset,
            ],
        )?;
        self.out.seek(SeekFrom::Start(0))?;
        self.out.write_all(&self.header.buf)?;
        self.out.write_all(&body.buf)?;
        // reserve the layer table
        self.out
            .write_all(&vec![0u8; (LAYER_DEF_SIZE * info.layer_count) as usize])?;
        // remember offsets for the final header rewrite
        self.header.buf.clear();
        self.header
            .u32(preview_offsets[0])
            .u32(preview_offsets[1])
            .u32(slicer_offset);
        Ok(())
    }

    fn encode_layer(&self, index: u32, raster: &LayerRaster) -> Result<Vec<u8>> {
        let mut data = encode_rle(&raster.runs);
        crypt_layer(self.seed, index, &mut data);
        Ok(data)
    }

    fn write_layer(&mut self, meta: &LayerMeta, encoded: Vec<u8>) -> Result<()> {
        let data_offset = self.next_data_offset + LAYER_DEF_EX_SIZE as u64;
        if data_offset + encoded.len() as u64 > u32::MAX as u64 {
            return Err(FormatError::Unsupported("CTB files are limited to 4 GiB".into()));
        }
        let def = |w: &mut LeWriter| {
            w.f32(meta.position_z)
                .f32(meta.exposure)
                .f32(0.0)
                .u32(data_offset as u32)
                .u32(encoded.len() as u32)
                .u32(0)
                .u32(LAYER_DEF_EX_SIZE)
                .u32(0)
                .u32(0);
        };
        // table entry
        let mut t = LeWriter::default();
        def(&mut t);
        self.out.seek(SeekFrom::Start(
            (self.layer_table_offset + LAYER_DEF_SIZE * meta.index) as u64,
        ))?;
        self.out.write_all(&t.buf)?;
        // extended definition followed by the data
        let mut ex = LeWriter::default();
        def(&mut ex);
        ex.u32(LAYER_DEF_EX_SIZE + encoded.len() as u32);
        let s = self.settings.as_ref().expect("begin() called");
        let p = if meta.is_bottom { &s.bottom } else { &s.normal };
        ex.f32(p.lift.total_distance())
            .f32(p.lift.speed)
            .f32(p.lift.distance2)
            .f32(p.lift.speed2)
            .f32(p.retract.speed)
            .f32(p.retract.distance2)
            .f32(p.retract.speed2)
            .f32(p.wait_before_lift)
            .f32(p.wait_after_lift)
            .f32(p.wait_after_retract)
            .f32(p.light_pwm as f32);
        self.out.seek(SeekFrom::Start(self.next_data_offset))?;
        self.out.write_all(&ex.buf)?;
        self.out.write_all(&encoded)?;
        self.next_data_offset = data_offset + encoded.len() as u64;
        self.written += 1;
        Ok(())
    }

    fn finish(&mut self, info: &JobInfo, est: &PrintEstimate) -> Result<()> {
        let mut r = Reader::le(&self.header.buf, 0);
        let large = r.u32()?;
        let small = r.u32()?;
        let slicer = r.u32()?;
        // patch volume / weight / cost in the print parameters
        let mut pp = LeWriter::default();
        pp.f32(est.volume_ml() as f32)
            .f32(est.weight_g as f32)
            .f32(est.cost as f32);
        self.out.seek(SeekFrom::Start(self.print_params_offset as u64 + 20))?;
        self.out.write_all(&pp.buf)?;
        let pp_off = self.print_params_offset;
        self.write_header(info, Some(est), [large, small, pp_off, slicer])?;
        self.out.seek(SeekFrom::Start(0))?;
        self.out.write_all(&self.header.buf)?;
        self.out.seek(SeekFrom::End(0))?;
        self.out.flush()?;
        if self.written != self.layer_count {
            return Err(FormatError::Invalid(format!(
                "wrote {} of {} layers",
                self.written, self.layer_count
            )));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- reader

pub struct CtbFile {
    info: SlicedInfo,
    seed: u32,
    layers: Vec<(f32, u32, u32)>,
    preview: Option<(u32, u32, u32, u32)>,
    file: Mutex<BufReader<File>>,
}

impl CtbFile {
    pub fn open(path: &Path) -> Result<Self> {
        let mut f = BufReader::new(File::open(path)?);
        let file_len = f.get_ref().metadata()?.len();
        let mut head = vec![0u8; HEADER_SIZE as usize];
        f.read_exact(&mut head)?;
        let mut r = Reader::le(&head, 0);
        let magic = r.u32()?;
        match magic {
            MAGIC_CTB | MAGIC_CTB_V4 | MAGIC_CBDDLP => {}
            MAGIC_CTB_ENCRYPTED => {
                return Err(FormatError::Unsupported(
                    "this CTB uses the encrypted container format, which OpenResin cannot read".into(),
                ))
            }
            m => return Err(FormatError::Invalid(format!("not a CTB file (magic {m:#x})"))),
        }
        let _version = r.u32()?;
        let size_x = r.f32()?;
        let size_y = r.f32()?;
        let _size_z = r.f32()?;
        r.skip(8)?;
        let _total_h = r.f32()?;
        let layer_height = r.f32()?;
        let exposure = r.f32()?;
        let bottom_exposure = r.f32()?;
        let _light_off = r.f32()?;
        let bottom_layers = r.u32()?;
        let resolution_x = r.u32()?;
        let resolution_y = r.u32()?;
        let large_preview = r.u32()?;
        let table = r.u32()?;
        let layer_count = r.u32()?;
        let _small_preview = r.u32()?;
        let print_time = r.u32()?;
        let projector = r.u32()?;
        let pp_off = r.u32()?;
        let _pp_size = r.u32()?;
        let aa = r.u32()?;
        let _pwm = r.u16()?;
        let _bpwm = r.u16()?;
        let key = r.u32()?;
        let seed = if magic == MAGIC_CBDDLP { 0 } else { key };
        let slicer_off = r.u32()?;

        let mut volume_ml = 0.0;
        if pp_off != 0 {
            f.seek(SeekFrom::Start(pp_off as u64 + 20))?;
            let mut b = [0u8; 4];
            f.read_exact(&mut b)?;
            volume_ml = f32::from_le_bytes(b);
        }
        if table as u64 + layer_count as u64 * LAYER_DEF_SIZE as u64 > file_len {
            return Err(FormatError::Invalid(format!(
                "the layer table of {layer_count} layers runs past the end of the file"
            )));
        }
        let mut layers = Vec::with_capacity(layer_count as usize);
        f.seek(SeekFrom::Start(table as u64))?;
        let mut tbl = vec![0u8; layer_count as usize * LAYER_DEF_SIZE as usize];
        f.read_exact(&mut tbl)?;
        let mut tr = Reader::le(&tbl, 0);
        for i in 0..layer_count {
            let z = tr.f32()?;
            tr.skip(8)?;
            let addr = tr.u32()?;
            let size = tr.u32()?;
            tr.skip(16)?;
            if addr as u64 + size as u64 > file_len {
                return Err(FormatError::Invalid(format!("layer {i} runs past the end of the file")));
            }
            layers.push((z, addr, size));
        }
        // a damaged preview only loses the thumbnail
        let mut preview = None;
        if large_preview != 0 && large_preview as u64 + 16 <= file_len {
            f.seek(SeekFrom::Start(large_preview as u64))?;
            let mut b = [0u8; 16];
            f.read_exact(&mut b)?;
            let mut pr = Reader::le(&b, 0);
            let (w, h, off, len) = (pr.u32()?, pr.u32()?, pr.u32()?, pr.u32()?);
            if (1..=4096).contains(&w) && (1..=4096).contains(&h) && off as u64 + len as u64 <= file_len {
                preview = Some((w, h, off, len));
            }
        }
        // machine name: address and length are stored in the slicer info
        let mut machine_name = String::new();
        if slicer_off != 0 && slicer_off as u64 + 36 <= file_len {
            f.seek(SeekFrom::Start(slicer_off as u64 + 28))?;
            let mut b = [0u8; 8];
            f.read_exact(&mut b)?;
            let mut nr = Reader::le(&b, 0);
            let (addr, len) = (nr.u32()?, nr.u32()?);
            if addr != 0 && len <= 1024 && addr as u64 + len as u64 <= file_len {
                f.seek(SeekFrom::Start(addr as u64))?;
                let mut name = vec![0u8; len as usize];
                f.read_exact(&mut name)?;
                let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
                machine_name = String::from_utf8_lossy(&name[..end]).into_owned();
            }
        }
        let info = SlicedInfo {
            format: "ctb".into(),
            machine_name,
            resolution_x,
            resolution_y,
            size_x,
            size_y,
            layer_count,
            layer_height,
            exposure,
            bottom_exposure,
            bottom_layers,
            print_time_s: print_time,
            volume_ml,
            mirror_x: projector != 0,
            mirror_y: false,
            anti_aliasing: aa,
        };
        Ok(Self {
            info,
            seed,
            layers,
            preview,
            file: Mutex::new(f),
        })
    }
}

impl SlicedFile for CtbFile {
    fn info(&self) -> &SlicedInfo {
        &self.info
    }

    fn layer(&self, index: u32) -> Result<LayerRaster> {
        let (_, addr, size) = *self
            .layers
            .get(index as usize)
            .ok_or_else(|| FormatError::Invalid(format!("layer {index} out of range")))?;
        let mut data = vec![0u8; size as usize];
        {
            let mut f = self.file.lock().unwrap();
            f.seek(SeekFrom::Start(addr as u64))?;
            f.read_exact(&mut data)?;
        }
        crypt_layer(self.seed, index, &mut data);
        let (w, h) = (self.info.resolution_x, self.info.resolution_y);
        let runs = decode_rle(&data, w as u64 * h as u64)?;
        Ok(LayerRaster::from_runs(w, h, runs))
    }

    fn layer_z(&self, index: u32) -> f32 {
        self.layers.get(index as usize).map(|l| l.0).unwrap_or(0.0)
    }

    fn thumbnail(&self) -> Option<Thumbnail> {
        let (w, h, off, len) = self.preview?;
        let mut data = vec![0u8; len as usize];
        let mut f = self.file.lock().ok()?;
        f.seek(SeekFrom::Start(off as u64)).ok()?;
        f.read_exact(&mut data).ok()?;
        Some(decode_preview(w, h, &data))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rle_roundtrip() {
        let runs = vec![
            Run { value: 0, len: 1 },
            Run { value: 255, len: 0x7F },
            Run { value: 128, len: 0x80 },
            Run { value: 0, len: 0x4000 },
            Run {
                value: 254,
                len: 0x20_0000,
            },
            Run { value: 2, len: 1 },
        ];
        let total = runs.iter().map(|r| r.len as u64).sum();
        let enc = encode_rle(&runs);
        let dec = decode_rle(&enc, total).unwrap();
        // values are quantised to 7 bits: 254 -> 255, 2 -> 3
        let vals: Vec<u8> = dec.iter().map(|r| r.value).collect();
        assert_eq!(vals, vec![0, 255, 128, 0, 255, 3]);
        let lens: Vec<u32> = dec.iter().map(|r| r.len).collect();
        assert_eq!(lens, vec![1, 0x7F, 0x80, 0x4000, 0x20_0000, 1]);
    }

    #[test]
    fn crypt_is_symmetric() {
        let mut d: Vec<u8> = (0..=255).collect();
        let orig = d.clone();
        crypt_layer(0xdead_beef, 7, &mut d);
        assert_ne!(d, orig);
        crypt_layer(0xdead_beef, 7, &mut d);
        assert_eq!(d, orig);
    }

    #[test]
    fn preview_roundtrip() {
        let mut rgba = vec![0u8; 8 * 4 * 4];
        for (i, p) in rgba.chunks_exact_mut(4).enumerate() {
            let v = if i < 20 { 255 } else { 64 };
            p.copy_from_slice(&[v, v, v, 255]);
        }
        let t = Thumbnail::new(8, 4, rgba);
        let back = decode_preview(8, 4, &encode_preview(&t));
        assert_eq!(back.rgba[0], 248);
        assert_eq!(back.rgba[20 * 4], 64);
    }
}
