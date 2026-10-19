//! Creality HALOT CXDLP (`.cxdlp`, header version 3).
//!
//! A sequential, big-endian file:
//!
//! 1. header: `u32` magic length (9), `"CXSW3DV2\0"`, `u16` version, printer
//!    model (`u32` length + NUL terminated string), `u16` layer count, `u16`
//!    resolution X and Y, 64 reserved bytes;
//! 2. three RGB565 (big-endian) previews of 116×116, 290×290 and 290×290
//!    pixels, each followed by CR LF;
//! 3. slicer info: display width, display height and layer height as UTF-16BE
//!    strings with a `u32` byte length, then eleven `u16` print parameters;
//! 4. one `u32` area per layer, CR LF;
//! 5. (version 3) software and material name, compensation settings, CR LF;
//! 6. the layers: `u32` area, `u32` line count, 6 byte lines, CR LF;
//! 7. footer: `u32` 9, `"CXSW3DV2\0"`, and a `u32` checksum of everything
//!    before it (CRC-32 without pre/post inversion; version 2 used a byte XOR).
//!
//! Layer images are stored as vertical lines: for every column (left to
//! right) each run of equal non-black pixels (top to bottom) becomes a line of
//! 13-bit start row, 13-bit end row (inclusive), 14-bit column and a grey byte.
//! Grey levels are stored losslessly.
//!
//! # Verification status
//!
//! The layout follows the field order documented by the UVtools project. Files
//! were read back with UVtools 4.4.3 (header, print parameters and layer images
//! match, the checksum is accepted) and re-encoding them with UVtools gives the
//! same line data. No file from Creality's slicers was available and nothing
//! was printed. Not verifiable here:
//! * the printer model string: printers are identified by codes such as
//!   `CL-60` (HALOT-ONE); the code is chosen from the printer resolution and
//!   name using the reference table and falls back to the machine type;
//! * the per-layer area is documented as the area of the largest contour in
//!   mm² × 1000; OpenResin writes the total lit area (an upper bound);
//! * the multi-byte compensation fields of the version 3 block are written
//!   little-endian, as the reference implementation does.
//!
//! Print parameters are integers: exposure in 0.1 s, bottom exposure, waits and
//! lift heights in whole seconds/millimetres and speeds in whole mm/s.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Mutex;

use openresin_core::estimate::PrintEstimate;
use openresin_core::profile::{LayerParams, PrinterProfile};
use openresin_core::slicer::LayerRaster;

use crate::bin::BeWriter;
use crate::preview::Thumbnail;
use crate::{FormatError, FormatWriter, JobInfo, LayerMeta, Result, SlicedFile, SlicedInfo, WriteSeek};

pub const MAGIC: &[u8; 9] = b"CXSW3DV2\0";
/// Common prefix of all CXDLP magics.
pub const MAGIC_PREFIX: &[u8; 6] = b"CXSW3D";
pub const VERSION: u16 = 3;
pub const PREVIEWS: [(u32, u32); 3] = [(116, 116), (290, 290), (290, 290)];
const CRLF: [u8; 2] = [0x0D, 0x0A];
/// Resolution limits implied by the 14-bit column / 13-bit row coordinates.
pub const MAX_RESOLUTION: (u32, u32) = (1 << 14, 1 << 13);

// ---------------------------------------------------------------- CRC-32

const CRC_TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut v = i as u32;
        let mut j = 0;
        while j < 8 {
            v = if v & 1 != 0 { (v >> 1) ^ 0xEDB8_8320 } else { v >> 1 };
            j += 1;
        }
        t[i] = v;
        i += 1;
    }
    t
};

/// Continues a reflected CRC-32 (polynomial 0xEDB88320) without the usual
/// pre/post inversion; start with 0.
pub(crate) fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    for &b in data {
        crc = CRC_TABLE[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc
}

fn gf2_times(mat: &[u32; 32], mut vec: u32) -> u32 {
    let mut sum = 0;
    let mut i = 0;
    while vec != 0 {
        if vec & 1 != 0 {
            sum ^= mat[i];
        }
        vec >>= 1;
        i += 1;
    }
    sum
}

fn gf2_square(square: &mut [u32; 32], mat: &[u32; 32]) {
    for n in 0..32 {
        square[n] = gf2_times(mat, mat[n]);
    }
}

/// CRC of `A ‖ B` from `crc(A)`, `crc(B)` and `len(B)`, so parts of a file
/// written out of order can be checksummed without reading them back.
pub(crate) fn crc32_combine(mut crc1: u32, crc2: u32, mut len2: u64) -> u32 {
    if len2 == 0 {
        return crc1;
    }
    let mut even = [0u32; 32];
    let mut odd = [0u32; 32];
    // operator for one zero bit
    odd[0] = 0xEDB8_8320;
    let mut row = 1u32;
    for v in odd.iter_mut().skip(1) {
        *v = row;
        row <<= 1;
    }
    gf2_square(&mut even, &odd); // two zero bits
    gf2_square(&mut odd, &even); // four zero bits
    loop {
        gf2_square(&mut even, &odd);
        if len2 & 1 != 0 {
            crc1 = gf2_times(&even, crc1);
        }
        len2 >>= 1;
        if len2 == 0 {
            break;
        }
        gf2_square(&mut odd, &even);
        if len2 & 1 != 0 {
            crc1 = gf2_times(&odd, crc1);
        }
        len2 >>= 1;
        if len2 == 0 {
            break;
        }
    }
    crc1 ^ crc2
}

/// Checksums the first `len` bytes of a file.
pub(crate) fn file_crc32(f: &mut BufReader<File>, len: u64, xor_only: bool) -> Result<u32> {
    f.seek(SeekFrom::Start(0))?;
    let mut buf = vec![0u8; 1 << 20];
    let mut left = len;
    let mut crc = 0u32;
    while left > 0 {
        let n = left.min(buf.len() as u64) as usize;
        f.read_exact(&mut buf[..n])?;
        crc = if xor_only {
            buf[..n].iter().fold(crc, |a, &b| a ^ b as u32)
        } else {
            crc32_update(crc, &buf[..n])
        };
        left -= n as u64;
    }
    Ok(crc)
}

// ---------------------------------------------------------------- line codec

/// A vertical line: column `x`, rows `y0..=y1`, grey `value` (never 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Line {
    pub x: u16,
    pub y0: u16,
    pub y1: u16,
    pub value: u8,
}

impl Line {
    pub fn to_bytes(self) -> [u8; 6] {
        let (sy, ey, x) = (self.y0 as u32, self.y1 as u32, self.x as u32);
        [
            (sy >> 5) as u8,
            ((sy << 3) | (ey >> 10)) as u8,
            (ey >> 2) as u8,
            ((ey << 6) | (x >> 8)) as u8,
            x as u8,
            self.value,
        ]
    }

    pub fn from_bytes(b: &[u8]) -> Self {
        let y0 = (((b[0] as u32) << 8 | b[1] as u32) >> 3) & 0x1FFF;
        let y1 = (((b[1] as u32) << 16 | (b[2] as u32) << 8 | b[3] as u32) >> 6) & 0x1FFF;
        let x = ((b[3] as u32) << 8 | b[4] as u32) & 0x3FFF;
        Line {
            x: x as u16,
            y0: y0 as u16,
            y1: y1 as u16,
            value: b[5],
        }
    }
}

/// Splits a layer into vertical lines, ordered by column then row.
pub fn encode_lines(raster: &LayerRaster) -> Vec<Line> {
    let Some((x0, y0, x1, y1)) = raster.bbox else {
        return Vec::new();
    };
    debug_assert!(raster.width <= MAX_RESOLUTION.0 && raster.height <= MAX_RESOLUTION.1);
    let (x0, x1) = (x0 as usize, x1 as usize);
    // (start row, value) of the line currently open in every column of the bbox
    let mut open = vec![(0u32, 0u8); x1 - x0];
    let mut lines = Vec::new();
    let line = |x: usize, start: u32, end: u32, value: u8| Line {
        x: x as u16,
        y0: start as u16,
        y1: end as u16,
        value,
    };
    raster.for_each_row(|y, row| {
        if y < y0 || y >= y1 {
            return;
        }
        for (i, (&v, o)) in row[x0..x1].iter().zip(open.iter_mut()).enumerate() {
            if v != o.1 {
                if o.1 != 0 {
                    lines.push(line(x0 + i, o.0, y - 1, o.1));
                }
                *o = (y, v);
            }
        }
    });
    for (i, o) in open.iter().enumerate() {
        if o.1 != 0 {
            lines.push(line(x0 + i, o.0, y1 - 1, o.1));
        }
    }
    lines.sort_unstable_by_key(|l| (l.x, l.y0));
    lines
}

/// Draws encoded lines (6 bytes each) into a `width` × `height` grey image.
/// Lines are clipped to the image.
pub fn decode_lines(data: &[u8], width: u32, height: u32) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let mut px = vec![0u8; w * h];
    for b in data.chunks_exact(6) {
        let l = Line::from_bytes(b);
        let (mut a, mut z) = (l.y0 as usize, l.y1 as usize);
        if a > z {
            std::mem::swap(&mut a, &mut z);
        }
        let x = l.x as usize;
        if x >= w || a >= h {
            continue;
        }
        for y in a..=z.min(h - 1) {
            px[y * w + x] = l.value;
        }
    }
    px
}

// ---------------------------------------------------------------- writer

/// Model code written into the header. HALOT printers are identified by
/// codes such as `CL-60`; the table follows the reference implementation.
pub fn model_code(p: &PrinterProfile) -> String {
    let mt = p.machine_type();
    if mt.starts_with("CL") || mt.starts_with("CT") {
        return mt;
    }
    let name = format!("{} {}", p.name, mt).to_ascii_uppercase();
    let code = match (p.resolution_x, p.resolution_y) {
        (1620, 2560) => Some("CL-60"),
        (2560, 2400) => Some("CL-70"),
        (4320, 2560) => Some("CL-79"),
        (3840, 2400) if name.contains("LITE") => Some("CL-89L"),
        (3840, 2400) if name.contains("SKY") => Some("CL-89"),
        (5760, 3600) if name.contains("RAY") => Some("CL925"),
        (5760, 3600) if name.contains("SKY") => Some("CL-92"),
        (3840, 2160) if name.contains("MAX") => Some("CL-133"),
        (7680, 4320) if name.contains("PRO") => Some("CL-103"),
        (7680, 4320) if name.contains("MAGE") => Some("CL-103L"),
        _ => None,
    };
    code.map(str::to_string).unwrap_or(mt)
}

fn put_cstr(w: &mut BeWriter, s: &str) {
    w.u32(s.len() as u32 + 1).bytes(s.as_bytes()).u8(0);
}

fn put_utf16(w: &mut BeWriter, s: &str) {
    let units: Vec<u16> = s.encode_utf16().collect();
    w.u32(units.len() as u32 * 2);
    for u in units {
        w.u16(u);
    }
}

fn num_string(v: f32, decimals: i32) -> String {
    let f = 10f32.powi(decimals);
    format!("{}", (v * f).round() / f)
}

fn round_u16(v: f32) -> u16 {
    v.round().clamp(0.0, u16::MAX as f32) as u16
}

fn wait_before_cure(p: &LayerParams, wait_mode: bool) -> f32 {
    if wait_mode {
        p.wait_after_retract
    } else {
        p.light_off_delay
    }
}

pub struct CxdlpWriter {
    out: Box<dyn WriteSeek>,
    head: Vec<u8>,
    area_table: usize,
    areas: Vec<u32>,
    pixel_area_mm2: f64,
    layer_count: u32,
    /// CRC and length of everything written after the head.
    crc: u32,
    data_len: u64,
}

impl CxdlpWriter {
    pub fn new(out: Box<dyn WriteSeek>) -> Self {
        Self {
            out,
            head: Vec::new(),
            area_table: 0,
            areas: Vec::new(),
            pixel_area_mm2: 0.0,
            layer_count: 0,
            crc: 0,
            data_len: 0,
        }
    }

    fn build_head(info: &JobInfo) -> (Vec<u8>, usize) {
        let p = info.printer;
        let s = info.settings;
        let (b, n) = (&s.bottom, &s.normal);
        let mut w = BeWriter::with_capacity(400_000);
        w.u32(MAGIC.len() as u32).bytes(MAGIC).u16(VERSION);
        put_cstr(&mut w, &model_code(p));
        w.u16(info.layer_count as u16)
            .u16(p.resolution_x as u16)
            .u16(p.resolution_y as u16)
            .bytes(&[0u8; 64]);
        for (pw, ph) in PREVIEWS {
            match info.thumbnail {
                Some(t) => {
                    for px in t.fit(pw, ph).to_rgb565() {
                        w.u16(px);
                    }
                }
                None => {
                    w.bytes(&vec![0u8; (pw * ph * 2) as usize]);
                }
            }
            w.bytes(&CRLF);
        }
        put_utf16(&mut w, &num_string(p.size_x, 2));
        put_utf16(&mut w, &num_string(p.size_y, 2));
        put_utf16(&mut w, &num_string(s.layer_height, 4));
        // speeds are whole mm/s, at least 1; waits at least 1 s (0 is refused by printers)
        let speed = |mm_min: f32| round_u16(mm_min / 60.0).max(1);
        w.u16(round_u16(n.exposure * 10.0))
            .u16(round_u16(wait_before_cure(n, s.wait_mode)).max(1))
            .u16(round_u16(b.exposure))
            .u16(s.bottom_layers.min(u16::MAX as u32) as u16)
            .u16(round_u16(b.lift.total_distance()).max(1))
            .u16(speed(b.lift.speed))
            .u16(round_u16(n.lift.total_distance()).max(1))
            .u16(speed(n.lift.speed))
            .u16(speed(n.retract.speed))
            .u16(b.light_pwm as u16)
            .u16(n.light_pwm as u16);
        let area_table = w.buf.len();
        w.bytes(&vec![0u8; info.layer_count as usize * 4]).bytes(&CRLF);
        put_cstr(&mut w, &format!("OpenResin {}", env!("CARGO_PKG_VERSION")));
        put_cstr(&mut w, &s.name);
        // compensation settings (written little-endian like the reference)
        w.u8(0) // distortion compensation off
            .bytes(&600u32.to_le_bytes())
            .bytes(&300_000u32.to_le_bytes())
            .u8(1) // XY profile compensation
            .bytes(&0u16.to_le_bytes())
            .u8(0) // Z penetration compensation off
            .bytes(&1000u16.to_le_bytes())
            .u8((s.anti_aliasing > 1) as u8)
            .u8(1) // grey min
            .u8(255) // grey max
            .u8(0) // blur off
            .u8(2) // blur level
            .bytes(&CRLF);
        (w.buf, area_table)
    }
}

impl FormatWriter for CxdlpWriter {
    fn begin(&mut self, info: &JobInfo) -> Result<()> {
        let p = info.printer;
        if p.resolution_x > MAX_RESOLUTION.0 || p.resolution_y > MAX_RESOLUTION.1 {
            return Err(FormatError::Unsupported(format!(
                "CXDLP supports at most {}x{} pixels",
                MAX_RESOLUTION.0, MAX_RESOLUTION.1
            )));
        }
        if info.layer_count > u16::MAX as u32 {
            return Err(FormatError::Unsupported(
                "CXDLP files are limited to 65535 layers".into(),
            ));
        }
        self.layer_count = info.layer_count;
        self.pixel_area_mm2 = (p.size_x as f64 / p.resolution_x as f64) * (p.size_y as f64 / p.resolution_y as f64);
        let (head, area_table) = Self::build_head(info);
        self.head = head;
        self.area_table = area_table;
        self.areas = Vec::with_capacity(info.layer_count as usize);
        self.crc = 0;
        self.data_len = 0;
        self.out.seek(SeekFrom::Start(0))?;
        self.out.write_all(&self.head)?;
        Ok(())
    }

    fn encode_layer(&self, _index: u32, raster: &LayerRaster) -> Result<Vec<u8>> {
        let lines = encode_lines(raster);
        let area = (raster.lit_area_px() * self.pixel_area_mm2 * 1000.0)
            .round()
            .min(u32::MAX as f64) as u32;
        let mut out = Vec::with_capacity(10 + lines.len() * 6);
        out.extend_from_slice(&area.to_be_bytes());
        out.extend_from_slice(&(lines.len() as u32).to_be_bytes());
        for l in lines {
            out.extend_from_slice(&l.to_bytes());
        }
        out.extend_from_slice(&CRLF);
        Ok(out)
    }

    fn write_layer(&mut self, _meta: &LayerMeta, encoded: Vec<u8>) -> Result<()> {
        if encoded.len() < 10 {
            return Err(FormatError::Invalid("layer was not encoded by this writer".into()));
        }
        self.areas.push(u32::from_be_bytes(encoded[0..4].try_into().unwrap()));
        self.out.write_all(&encoded)?;
        self.crc = crc32_update(self.crc, &encoded);
        self.data_len += encoded.len() as u64;
        Ok(())
    }

    fn finish(&mut self, _info: &JobInfo, _est: &PrintEstimate) -> Result<()> {
        if self.areas.len() as u32 != self.layer_count {
            return Err(FormatError::Invalid(format!(
                "wrote {} of {} layers",
                self.areas.len(),
                self.layer_count
            )));
        }
        let mut footer = BeWriter::default();
        footer.u32(MAGIC.len() as u32).bytes(MAGIC);
        self.out.write_all(&footer.buf)?;
        self.crc = crc32_update(self.crc, &footer.buf);
        self.data_len += footer.buf.len() as u64;
        for (i, a) in self.areas.iter().enumerate() {
            let at = self.area_table + i * 4;
            self.head[at..at + 4].copy_from_slice(&a.to_be_bytes());
        }
        self.out.seek(SeekFrom::Start(0))?;
        self.out.write_all(&self.head)?;
        let crc = crc32_combine(crc32_update(0, &self.head), self.crc, self.data_len);
        self.out.seek(SeekFrom::End(0))?;
        self.out.write_all(&crc.to_be_bytes())?;
        self.out.flush()?;
        Ok(())
    }
}

// ---------------------------------------------------------------- reader

/// Big-endian sequential reads from a buffered file.
pub(crate) struct SeqReader<'a> {
    pub f: &'a mut BufReader<File>,
    pub pos: u64,
    pub len: u64,
}

impl SeqReader<'_> {
    pub fn bytes(&mut self, n: usize) -> Result<Vec<u8>> {
        if self.pos + n as u64 > self.len {
            return Err(FormatError::Invalid(format!(
                "unexpected end of file at offset {}",
                self.pos
            )));
        }
        let mut b = vec![0u8; n];
        self.f.read_exact(&mut b)?;
        self.pos += n as u64;
        Ok(b)
    }
    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.bytes(2)?.try_into().unwrap()))
    }
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.bytes(4)?.try_into().unwrap()))
    }
    pub fn skip(&mut self, n: u64) -> Result<()> {
        if self.pos + n > self.len {
            return Err(FormatError::Invalid(format!(
                "unexpected end of file at offset {}",
                self.pos
            )));
        }
        self.f.seek_relative(n as i64)?;
        self.pos += n;
        Ok(())
    }
    /// `u32` length followed by a NUL terminated string.
    pub fn cstr(&mut self) -> Result<String> {
        let n = self.u32()? as usize;
        if n > 4096 {
            return Err(FormatError::Invalid(format!(
                "string of {n} bytes at offset {}",
                self.pos
            )));
        }
        let b = self.bytes(n)?;
        let end = b.iter().position(|&c| c == 0).unwrap_or(n);
        Ok(String::from_utf8_lossy(&b[..end]).into_owned())
    }
    fn utf16(&mut self) -> Result<String> {
        let n = self.u32()? as usize;
        if n > 256 || n % 2 != 0 {
            return Err(FormatError::Invalid(format!("bad UTF-16 string length {n}")));
        }
        let b = self.bytes(n)?;
        let units: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        Ok(String::from_utf16_lossy(&units))
    }
    fn crlf(&mut self) -> Result<()> {
        if self.bytes(2)? != CRLF {
            return Err(FormatError::Invalid(format!(
                "missing line break at offset {}",
                self.pos - 2
            )));
        }
        Ok(())
    }
}

/// Print parameters of the slicer info block.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CxdlpParams {
    pub display_width: String,
    pub display_height: String,
    pub layer_height: String,
    /// 0.1 s
    pub exposure: u16,
    pub wait_before_cure: u16,
    pub bottom_exposure: u16,
    pub bottom_layers: u16,
    pub bottom_lift_height: u16,
    /// mm/s
    pub bottom_lift_speed: u16,
    pub lift_height: u16,
    pub lift_speed: u16,
    pub retract_speed: u16,
    pub bottom_light_pwm: u16,
    pub light_pwm: u16,
}

pub struct CxdlpFile {
    pub version: u16,
    pub model: String,
    pub params: CxdlpParams,
    pub software: String,
    pub material: String,
    /// (offset of the line data, line count) per layer.
    layers: Vec<(u64, u32)>,
    preview: Option<(u32, u32, u64)>,
    info: SlicedInfo,
    file: Mutex<BufReader<File>>,
}

impl CxdlpFile {
    pub fn open(path: &Path) -> Result<Self> {
        let mut f = BufReader::new(File::open(path)?);
        let len = f.get_ref().metadata()?.len();
        let mut r = SeqReader { f: &mut f, pos: 0, len };
        let magic_len = r.u32()? as usize;
        if !(6..=64).contains(&magic_len) {
            return Err(FormatError::Invalid("not a CXDLP file".into()));
        }
        let magic = r.bytes(magic_len)?;
        if !magic.starts_with(MAGIC_PREFIX) {
            return Err(FormatError::Invalid("not a CXDLP file".into()));
        }
        let version = r.u16()?;
        if version > 3 {
            return Err(FormatError::Unsupported(format!(
                "CXDLP version {version} (use the .cxdlpv4 reader)"
            )));
        }
        let model = r.cstr()?;
        let layer_count = r.u16()? as u32;
        let (res_x, res_y) = (r.u16()? as u32, r.u16()? as u32);
        // lines cannot address more; also bounds the image buffer of `layer`
        if res_x > MAX_RESOLUTION.0 || res_y > MAX_RESOLUTION.1 {
            return Err(FormatError::Invalid(format!(
                "CXDLP resolution {res_x}x{res_y} exceeds {}x{}",
                MAX_RESOLUTION.0, MAX_RESOLUTION.1
            )));
        }
        r.skip(64)?;
        let mut preview = None;
        for (k, (pw, ph)) in PREVIEWS.into_iter().enumerate() {
            if k == 1 {
                preview = Some((pw, ph, r.pos));
            }
            r.skip((pw * ph * 2) as u64)?;
            r.crlf()?;
        }
        let mut params = CxdlpParams {
            display_width: r.utf16()?,
            display_height: r.utf16()?,
            layer_height: r.utf16()?,
            ..Default::default()
        };
        for v in [
            &mut params.exposure,
            &mut params.wait_before_cure,
            &mut params.bottom_exposure,
            &mut params.bottom_layers,
            &mut params.bottom_lift_height,
            &mut params.bottom_lift_speed,
            &mut params.lift_height,
            &mut params.lift_speed,
            &mut params.retract_speed,
            &mut params.bottom_light_pwm,
            &mut params.light_pwm,
        ] {
            *v = r.u16()?;
        }
        r.skip(layer_count as u64 * 4)?;
        r.crlf()?;
        let (mut software, mut material) = (String::new(), String::new());
        if version >= 3 {
            software = r.cstr()?;
            material = r.cstr()?;
            // compensation settings and the anti-aliasing flag; the level itself is not stored
            r.skip(20)?;
            r.crlf()?;
        }
        let mut layers = Vec::with_capacity(layer_count as usize);
        for i in 0..layer_count {
            let _area = r.u32()?;
            let count = r.u32()?;
            if r.pos + count as u64 * 6 + 2 > len {
                return Err(FormatError::Invalid(format!("layer {i} runs past the end of the file")));
            }
            layers.push((r.pos, count));
            r.skip(count as u64 * 6)?;
            r.crlf()?;
        }
        let footer_len = r.u32()? as usize;
        if !r.bytes(footer_len.min(64))?.starts_with(MAGIC_PREFIX) {
            return Err(FormatError::Invalid("bad CXDLP footer".into()));
        }
        if r.pos + 4 != len {
            return Err(FormatError::Invalid("unexpected data after the CXDLP footer".into()));
        }
        let stored = r.u32()?;
        let expected = file_crc32(&mut f, len - 4, version < 3)?;
        if stored != expected {
            return Err(FormatError::Invalid(format!(
                "CXDLP checksum mismatch (stored {stored:#x}, computed {expected:#x})"
            )));
        }
        let num = |s: &str| s.trim().parse::<f32>().unwrap_or(0.0);
        let info = SlicedInfo {
            format: "cxdlp".into(),
            machine_name: model.clone(),
            resolution_x: res_x,
            resolution_y: res_y,
            size_x: num(&params.display_width),
            size_y: num(&params.display_height),
            layer_count,
            layer_height: num(&params.layer_height),
            exposure: params.exposure as f32 / 10.0,
            bottom_exposure: params.bottom_exposure as f32,
            bottom_layers: params.bottom_layers as u32,
            print_time_s: 0,
            volume_ml: 0.0,
            mirror_x: false,
            mirror_y: false,
            anti_aliasing: 1,
        };
        Ok(Self {
            version,
            model,
            params,
            software,
            material,
            layers,
            preview,
            info,
            file: Mutex::new(f),
        })
    }
}

impl SlicedFile for CxdlpFile {
    fn info(&self) -> &SlicedInfo {
        &self.info
    }

    fn layer(&self, index: u32) -> Result<LayerRaster> {
        let (pos, count) = *self
            .layers
            .get(index as usize)
            .ok_or_else(|| FormatError::Invalid(format!("layer {index} out of range")))?;
        let mut data = vec![0u8; count as usize * 6];
        {
            let mut f = self.file.lock().unwrap();
            f.seek(SeekFrom::Start(pos))?;
            f.read_exact(&mut data)?;
        }
        let (w, h) = (self.info.resolution_x, self.info.resolution_y);
        Ok(LayerRaster::from_gray(w, h, &decode_lines(&data, w, h)))
    }

    fn layer_z(&self, index: u32) -> f32 {
        if index >= self.info.layer_count {
            return 0.0;
        }
        (index as f32 + 1.0) * self.info.layer_height
    }

    fn thumbnail(&self) -> Option<Thumbnail> {
        let (w, h, pos) = self.preview?;
        let mut data = vec![0u8; (w * h * 2) as usize];
        let mut f = self.file.lock().ok()?;
        f.seek(SeekFrom::Start(pos)).ok()?;
        f.read_exact(&mut data).ok()?;
        let px: Vec<u16> = data.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        Some(Thumbnail::from_rgb565(w, h, &px))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openresin_core::slicer::Run;

    #[test]
    fn crc_check_value_and_combine() {
        // reflected CRC-32 with zero initial value and no final XOR
        assert_eq!(crc32_update(0, b"123456789"), 0x2DFD_2D88);
        let data: Vec<u8> = (0..5000u32).map(|i| (i * 31 % 251) as u8).collect();
        let whole = crc32_update(0, &data);
        for split in [0usize, 1, 7, 2500, 4999, 5000] {
            let (a, b) = data.split_at(split);
            assert_eq!(
                crc32_combine(crc32_update(0, a), crc32_update(0, b), b.len() as u64),
                whole
            );
        }
    }

    #[test]
    fn line_packing() {
        let l = Line {
            x: 3,
            y0: 5,
            y1: 9,
            value: 200,
        };
        assert_eq!(l.to_bytes(), [0, 40, 2, 64, 3, 200]);
        assert_eq!(Line::from_bytes(&l.to_bytes()), l);
        let max = Line {
            x: 0x3FFF,
            y0: 0x1FFF,
            y1: 0x1FFF,
            value: 255,
        };
        assert_eq!(max.to_bytes(), [0xFF; 6]);
        assert_eq!(Line::from_bytes(&max.to_bytes()), max);
        let mixed = Line {
            x: 0x2A5A,
            y0: 0x1234,
            y1: 0x0ABC,
            value: 7,
        };
        assert_eq!(Line::from_bytes(&mixed.to_bytes()), mixed);
    }

    #[test]
    fn lines_are_columns_of_equal_grey() {
        #[rustfmt::skip]
        let px: Vec<u8> = vec![
            0,   0,   0, 0,
            0, 255, 128, 0,
            0, 255, 128, 0,
            0, 255,  64, 0,
            0,   0,   0, 9,
        ];
        let r = LayerRaster::from_gray(4, 5, &px);
        let lines = encode_lines(&r);
        let l = |x, y0, y1, value| Line { x, y0, y1, value };
        assert_eq!(
            lines,
            vec![l(1, 1, 3, 255), l(2, 1, 2, 128), l(2, 3, 3, 64), l(3, 4, 4, 9)]
        );
        let bytes: Vec<u8> = lines.iter().flat_map(|l| l.to_bytes()).collect();
        assert_eq!(decode_lines(&bytes, 4, 5), px);
        assert!(encode_lines(&LayerRaster::empty(4, 5)).is_empty());
        // lines reaching the last row and long columns
        let r = LayerRaster::from_runs(
            3,
            4000,
            vec![
                Run { value: 0, len: 1 },
                Run {
                    value: 255,
                    len: 11_999,
                },
            ],
        );
        let lines = encode_lines(&r);
        assert_eq!(lines, vec![l(0, 1, 3999, 255), l(1, 0, 3999, 255), l(2, 0, 3999, 255)]);
        let bytes: Vec<u8> = lines.iter().flat_map(|l| l.to_bytes()).collect();
        assert_eq!(
            LayerRaster::from_gray(3, 4000, &decode_lines(&bytes, 3, 4000)).runs,
            r.runs
        );
    }

    /// A resolution the line coordinates cannot address is rejected before
    /// anything is allocated for it.
    #[test]
    fn oversized_resolution_is_an_error() {
        let mut w = BeWriter::default();
        w.u32(MAGIC.len() as u32).bytes(MAGIC).u16(VERSION);
        put_cstr(&mut w, "CL-60");
        w.u16(1).u16(65535).u16(65535).bytes(&[0u8; 64]);
        let path = std::env::temp_dir().join(format!("openresin-cxdlp-res-{}.cxdlp", std::process::id()));
        std::fs::write(&path, &w.buf).unwrap();
        let e = CxdlpFile::open(&path).err().unwrap();
        assert!(e.to_string().contains("65535x65535 exceeds"), "{e}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn model_codes() {
        let mut p = PrinterProfile::generic();
        p.name = "HALOT-ONE".into();
        (p.resolution_x, p.resolution_y) = (1620, 2560);
        assert_eq!(model_code(&p), "CL-60");
        p.name = "HALOT-LITE".into();
        (p.resolution_x, p.resolution_y) = (3840, 2400);
        assert_eq!(model_code(&p), "CL-89L");
        p.name = "HALOT-RAY".into();
        (p.resolution_x, p.resolution_y) = (5760, 3600);
        assert_eq!(model_code(&p), "CL925");
        p.machine_type = Some("CL-999".into());
        assert_eq!(model_code(&p), "CL-999");
        p.machine_type = None;
        p.name = "Unknown".into();
        (p.resolution_x, p.resolution_y) = (100, 100);
        assert_eq!(model_code(&p), "Generic Unknown");
        assert_eq!(num_string(192.0, 2), "192");
        assert_eq!(num_string(130.56, 2), "130.56");
        assert_eq!(num_string(0.05, 4), "0.05");
    }
}
