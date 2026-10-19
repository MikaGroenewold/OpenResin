//! Anycubic Photon Workshop files: `.pwmx`, `.pwma`, `.pwmb`, `.pm3`, `.pm5s`, …
//! and the older `.pws`, `.pw0` and `.pwx`.
//!
//! All of these share one little-endian container. A file mark (`"ANYCUBIC"`,
//! version, number of tables and the address of every section) is followed by
//! sections that mostly start with a 12 byte NUL padded name and a `u32` length:
//!
//! | section     | versions | content                                        |
//! |-------------|----------|------------------------------------------------|
//! | `HEADER`    | all      | global print settings                          |
//! | `PREVIEW`   | all      | 224×168 RGB565 thumbnail                       |
//! | grey table  | ≥ 515    | unnamed, grey value per anti-aliasing level    |
//! | `LAYERDEF`  | all      | one 32 byte record per layer                   |
//! | `EXTRA`     | ≥ 516    | two-stage ("TSMC") lift and retract settings   |
//! | `MACHINE`   | ≥ 516    | machine name, layer codec name, display size   |
//! | software    | ≥ 517    | slicer name and version (no section name)      |
//! | `MODEL`     | ≥ 517    | bounding box of the model                      |
//! | `SUBIMGS`   | ≥ 518    | second table of layer image addresses          |
//! | `PREVIEW2`  | ≥ 518    | second RGB565 thumbnail                        |
//!
//! The layer images follow the tables. Speeds are stored in mm/s, every layer
//! record stores its own thickness (positions are the running sum).
//!
//! Layer images use one of two run-length codecs, see [`encode_pw0`] (every
//! format except `.pws`) and [`encode_planes`] (`.pws`, Photon / Photon S).
//!
//! # Verification status
//!
//! The layout follows the field order and sizes documented by the UVtools
//! project (an independent implementation). Files of every family were read
//! back with UVtools 4.4.3: header fields, two-stage motion, transition layers
//! and the layer images (PW0 and PWS, anti-aliasing 4 and 8) match OpenResin's
//! reader pixel for pixel, and re-encoding them with UVtools gives the same
//! tables apart from rounding. The currency symbol is one UTF-16 code unit, as
//! UVtools reads it. No file produced by Photon Workshop was available and
//! nothing written here has been printed yet. In particular these points could
//! not be checked against a printer-produced file:
//! * the `PREVIEW`/`PREVIEW2` length field includes the 16 byte section head and
//!   `EXTRA` declares a length of 24 although it holds 56 bytes (both as in the
//!   reference);
//! * the meaning of the grey table (written as the reference does:
//!   `min((i + 1) * 255 / aa, 255)`), the `MACHINE` background constant and the
//!   `PREVIEW2` size of 320×190 (the reference's table default; its encoder
//!   writes 330×190, readers take the size from the section);
//! * the PW0 encoder rounds to the nearest of the 16 levels (`n * 17`), the
//!   reference truncates (`value >> 4`); both decode identically;
//! * the version written for `.pm4n` (517) is a guess, the reference does not
//!   pin one (and UVtools 4.4.3 does not know the extension; the file was
//!   checked as `.pm3n`). The other versions match the reference printer
//!   profiles.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Mutex;

use openresin_core::estimate::PrintEstimate;
use openresin_core::profile::{LayerParams, PrintSettings, PrinterProfile};
use openresin_core::slicer::{LayerRaster, Run};
use serde::{Deserialize, Serialize};

use crate::bin::{LeWriter, Reader};
use crate::preview::Thumbnail;
use crate::{FormatError, FormatWriter, JobInfo, LayerMeta, Result, SlicedFile, SlicedInfo, WriteSeek};

pub const MARK: &str = "ANYCUBIC";
pub const VERSION_1: u32 = 1;
pub const VERSION_515: u32 = 515;
pub const VERSION_516: u32 = 516;
pub const VERSION_517: u32 = 517;
pub const VERSION_518: u32 = 518;
pub const PREVIEW_SIZE: (u32, u32) = (224, 168);
pub const PREVIEW2_SIZE: (u32, u32) = (320, 190);

const NAME_LEN: usize = 12;
/// Name plus length field that starts most sections.
const SECTION_HEAD: u32 = NAME_LEN as u32 + 4;
const LAYER_DEF_SIZE: u32 = 32;
const SUB_LAYER_DEF_SIZE: u32 = 44;
const SOFTWARE_SIZE: u32 = 164;
const MODEL_SIZE: u32 = 48;
/// Value of the `EXTRA` length field (the section itself is 16 + 56 bytes).
const EXTRA_DECLARED_LEN: u32 = 24;
const MACHINE_BACKGROUND: u32 = 6_506_241;
const PREVIEW2_BACKGROUND: u16 = 4293;
const PW0_MAX_BW_RUN: u32 = 0xFFF;
const PW0_MAX_GREY_RUN: u32 = 0xF;
const PLANE_MAX_RUN: u32 = 125;
/// Bytes of writer-private metadata prepended by `encode_layer`.
const PREFIX_LEN: usize = 20;

// ---------------------------------------------------------------- models

/// Image codec used for the layers of a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerCodec {
    /// 4-bit greyscale run-length encoding.
    Pw0,
    /// One 1-bit run-length plane per anti-aliasing level.
    Pws,
}

/// Printer family (file extension) of the Photon Workshop container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnycubicModel {
    Pws,
    Pw0,
    Pwx,
    Dlp,
    Dl2p,
    Pwmx,
    Pmx2,
    Pwmb,
    Px6s,
    Pwmo,
    Pm3n,
    Pm4n,
    Pwms,
    Pwma,
    Pmsq,
    Pm3,
    Pm3m,
    Pm3r,
    Pm5,
    Pm5s,
    M5sp,
}

impl AnycubicModel {
    pub const COUNT: usize = 21;
    pub const ALL: [AnycubicModel; Self::COUNT] = {
        use AnycubicModel::*;
        [
            Pwmx, Pwma, Pwmb, Pwmo, Pwms, Pmsq, Pmx2, Px6s, Pm3, Pm3m, Pm3r, Pm3n, Pm4n, Pm5, Pm5s, M5sp, Dlp, Dl2p,
            Pwx, Pw0, Pws,
        ]
    };

    /// (extension, machine name, file version, description)
    const fn info(self) -> (&'static str, &'static str, u32, &'static str) {
        use AnycubicModel::*;
        match self {
            Pws => ("pws", "Photon S", VERSION_1, "Anycubic Photon / Photon S (PWS)"),
            Pw0 => ("pw0", "Photon Zero", VERSION_1, "Anycubic Photon Zero (PW0)"),
            Pwx => ("pwx", "Photon X", VERSION_1, "Anycubic Photon X (PWX)"),
            Dlp => ("dlp", "Photon Ultra", VERSION_516, "Anycubic Photon Ultra (DLP)"),
            Dl2p => ("dl2p", "Photon D2", VERSION_517, "Anycubic Photon D2 (DL2P)"),
            Pwmx => ("pwmx", "Photon Mono X", VERSION_516, "Anycubic Photon Mono X (PWMX)"),
            Pmx2 => ("pmx2", "Photon Mono X2", VERSION_517, "Anycubic Photon Mono X2 (PMX2)"),
            Pwmb => (
                "pwmb",
                "Photon M3 Plus",
                VERSION_517,
                "Anycubic Photon Mono X 6K / M3 Plus (PWMB)",
            ),
            Px6s => (
                "px6s",
                "Photon Mono X 6Ks",
                VERSION_517,
                "Anycubic Photon Mono X 6Ks (PX6S)",
            ),
            Pwmo => ("pwmo", "Photon Mono", VERSION_516, "Anycubic Photon Mono (PWMO)"),
            Pm3n => ("pm3n", "Photon Mono 2", VERSION_517, "Anycubic Photon Mono 2 (PM3N)"),
            Pm4n => ("pm4n", "Photon Mono 4", VERSION_517, "Anycubic Photon Mono 4 (PM4N)"),
            Pwms => ("pwms", "Photon Mono SE", VERSION_516, "Anycubic Photon Mono SE (PWMS)"),
            Pwma => ("pwma", "Photon Mono 4K", VERSION_516, "Anycubic Photon Mono 4K (PWMA)"),
            Pmsq => ("pmsq", "Photon Mono SQ", VERSION_516, "Anycubic Photon Mono SQ (PMSQ)"),
            Pm3 => ("pm3", "Photon M3", VERSION_516, "Anycubic Photon M3 (PM3)"),
            Pm3m => ("pm3m", "Photon M3 Max", VERSION_516, "Anycubic Photon M3 Max (PM3M)"),
            Pm3r => (
                "pm3r",
                "Photon M3 Premium",
                VERSION_517,
                "Anycubic Photon M3 Premium (PM3R)",
            ),
            Pm5 => ("pm5", "Photon Mono M5", VERSION_517, "Anycubic Photon Mono M5 (PM5)"),
            Pm5s => (
                "pm5s",
                "Photon Mono M5s",
                VERSION_518,
                "Anycubic Photon Mono M5s (PM5S)",
            ),
            M5sp => (
                "m5sp",
                "Photon Mono M5s Pro",
                VERSION_518,
                "Anycubic Photon Mono M5s Pro (M5SP)",
            ),
        }
    }

    pub const fn extension(self) -> &'static str {
        self.info().0
    }

    /// Machine name used when the printer profile does not provide one.
    pub const fn default_machine_name(self) -> &'static str {
        self.info().1
    }

    /// File version written for this family.
    pub const fn version(self) -> u32 {
        self.info().2
    }

    pub const fn description(self) -> &'static str {
        self.info().3
    }

    pub const fn codec(self) -> LayerCodec {
        match self {
            AnycubicModel::Pws => LayerCodec::Pws,
            _ => LayerCodec::Pw0,
        }
    }

    /// File version for a particular printer of this family.
    pub fn version_for(self, printer: &PrinterProfile) -> u32 {
        match self {
            // `.pwmb` is shared by the Photon Mono X 6K (516) and the M3 Plus (517)
            AnycubicModel::Pwmb if printer.name.to_ascii_lowercase().contains("6k") => VERSION_516,
            m => m.version(),
        }
    }

    pub fn from_extension(ext: &str) -> Option<Self> {
        let ext = ext.trim_start_matches('.');
        Self::ALL.into_iter().find(|m| m.extension().eq_ignore_ascii_case(ext))
    }
}

// ---------------------------------------------------------------- PW0 codec

/// 4-bit level (0..=15) of a greyscale value, rounded to the nearest level.
#[inline]
fn pw0_level(v: u8) -> u8 {
    ((v as u32 * 15 + 127) / 255) as u8
}

/// The value a pixel has after a PW0 round trip.
pub fn quantize_pw0(v: u8) -> u8 {
    pw0_level(v) * 17
}

fn push_pw0(out: &mut Vec<u8>, level: u8, mut len: u64) {
    if level == 0 || level == 15 {
        while len > 0 {
            let n = len.min(PW0_MAX_BW_RUN as u64) as u32;
            out.push((level << 4) | (n >> 8) as u8);
            out.push(n as u8);
            len -= n as u64;
        }
    } else {
        while len > 0 {
            let n = len.min(PW0_MAX_GREY_RUN as u64) as u8;
            out.push((level << 4) | n);
            len -= n as u64;
        }
    }
}

/// Encodes runs with the PW0 codec.
///
/// Every code byte holds the 4-bit level in the high nibble. Black (0) and
/// white (15) runs take a second byte and store a 12-bit length (max 4095);
/// grey runs store a 4-bit length (max 15) in the low nibble. Pixels are
/// quantised to the nearest of the levels `n * 17` and runs that become equal
/// are merged.
pub fn encode_pw0(runs: &[Run]) -> Vec<u8> {
    let mut out = Vec::with_capacity(runs.len() * 2 + 4);
    encode_pw0_into(runs, &mut out);
    out
}

fn encode_pw0_into(runs: &[Run], out: &mut Vec<u8>) {
    let mut cur: Option<(u8, u64)> = None;
    for r in runs {
        let level = pw0_level(r.value);
        match &mut cur {
            Some((l, n)) if *l == level => *n += r.len as u64,
            _ => {
                if let Some((l, n)) = cur.take() {
                    push_pw0(out, l, n);
                }
                cur = Some((level, r.len as u64));
            }
        }
    }
    if let Some((l, n)) = cur {
        push_pw0(out, l, n);
    }
}

/// Decodes PW0 data into runs covering exactly `pixels` pixels.
///
/// Trailing bytes after the last pixel are ignored and a final black/white
/// code that lacks its length byte fills the rest of the image (both behaviours
/// are tolerated by other readers). Empty data decodes to a blank image.
pub fn decode_pw0(data: &[u8], pixels: u64) -> Result<Vec<Run>> {
    let mut runs: Vec<Run> = Vec::new();
    let mut total = 0u64;
    let mut i = 0usize;
    while i < data.len() && total < pixels {
        let b = data[i];
        i += 1;
        let level = b >> 4;
        let mut len = (b & 0x0F) as u64;
        if level == 0 || level == 15 {
            match data.get(i) {
                Some(&lo) => {
                    len = (len << 8) | lo as u64;
                    i += 1;
                }
                None => len = pixels - total,
            }
        }
        if len == 0 {
            continue;
        }
        if total + len > pixels {
            return Err(FormatError::Invalid(format!(
                "PW0 layer runs past the image end ({} > {pixels} pixels)",
                total + len
            )));
        }
        let value = level * 17;
        total += len;
        push_run(&mut runs, value, len);
    }
    if total == 0 && data.is_empty() {
        return Ok(vec![Run {
            value: 0,
            len: pixels as u32,
        }]);
    }
    if total != pixels {
        return Err(FormatError::Invalid(format!(
            "PW0 layer decodes to {total} pixels, expected {pixels}"
        )));
    }
    Ok(runs)
}

fn push_run(runs: &mut Vec<Run>, value: u8, mut len: u64) {
    if let Some(last) = runs.last_mut() {
        if last.value == value {
            let add = len.min((u32::MAX - last.len) as u64);
            last.len += add as u32;
            len -= add;
        }
    }
    while len > 0 {
        let n = len.min(u32::MAX as u64) as u32;
        runs.push(Run { value, len: n });
        len -= n as u64;
    }
}

// ---------------------------------------------------------------- PWS bit planes

/// Inclusive threshold of plane `level` (1-based) when `aa` planes are used.
#[inline]
fn plane_threshold(level: u32, aa: u32) -> u8 {
    (255 * level / (aa + 1) + 1) as u8
}

/// Grey value of a pixel that is lit in `count` of `aa` planes.
///
/// `count * 256 / aa - 1` as in the reference, but never below the threshold
/// of plane `count`, so that a decoded image encodes to the same planes again
/// (at 13, 14 and 16 levels the reference value of one plane falls below it).
#[inline]
fn planes_to_gray(count: u32, aa: u32) -> u8 {
    if count == 0 {
        0
    } else {
        let v = (count * (256 / aa)).saturating_sub(1);
        v.max(plane_threshold(count.min(aa), aa) as u32).min(255) as u8
    }
}

/// The value a pixel has after a round trip through `aa` bit planes.
pub fn quantize_planes(v: u8, aa: u8) -> u8 {
    let aa = aa.clamp(1, 16) as u32;
    let count = (1..=aa).filter(|&k| v >= plane_threshold(k, aa)).count() as u32;
    planes_to_gray(count, aa)
}

/// Encodes runs as `aa` 1-bit planes (the `.pws` codec).
///
/// Plane `k` (1-based) has a pixel on when its value is at least
/// `255 * k / (aa + 1) + 1`. Each byte of a plane is a run: bit 7 is the pixel
/// state, bits 0-6 the length (written up to 125). Planes are concatenated.
pub fn encode_planes(runs: &[Run], aa: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(runs.len() * aa as usize + 16);
    encode_planes_into(runs, aa, &mut out);
    out
}

fn encode_planes_into(runs: &[Run], aa: u8, out: &mut Vec<u8>) {
    let aa = aa.clamp(1, 16) as u32;
    let flush = |out: &mut Vec<u8>, on: bool, mut len: u64| {
        let flag = if on { 0x80 } else { 0 };
        while len > 0 {
            let n = len.min(PLANE_MAX_RUN as u64);
            out.push(n as u8 | flag);
            len -= n;
        }
    };
    for level in 1..=aa {
        let t = plane_threshold(level, aa);
        let mut cur: Option<(bool, u64)> = None;
        for r in runs {
            let on = r.value >= t;
            match &mut cur {
                Some((s, n)) if *s == on => *n += r.len as u64,
                _ => {
                    if let Some((s, n)) = cur.take() {
                        flush(out, s, n);
                    }
                    cur = Some((on, r.len as u64));
                }
            }
        }
        if let Some((s, n)) = cur {
            flush(out, s, n);
        }
    }
}

/// Decodes `aa` concatenated bit planes of `pixels` pixels.
pub fn decode_planes(data: &[u8], aa: u8, pixels: u64) -> Result<Vec<Run>> {
    if aa == 0 || aa > 16 {
        return Err(FormatError::Invalid(format!(
            "unsupported PWS anti-aliasing level {aa}"
        )));
    }
    // every plane needs at least one byte per 127 pixels; checked before the
    // image buffer is allocated from the (untrusted) resolution
    let min_len = aa as u64 * pixels.div_ceil(0x7F);
    if (data.len() as u64) < min_len {
        return Err(FormatError::Invalid(format!(
            "PWS layer of {} bytes is too short for {aa} planes of {pixels} pixels",
            data.len()
        )));
    }
    let mut counts = vec![0u8; pixels as usize];
    let mut i = 0usize;
    for plane in 0..aa {
        let mut px = 0usize;
        while px < counts.len() {
            let b = *data
                .get(i)
                .ok_or_else(|| FormatError::Invalid(format!("PWS layer plane {plane} is truncated")))?;
            i += 1;
            let n = (b & 0x7F) as usize;
            if n > counts.len() - px {
                return Err(FormatError::Invalid("PWS layer plane runs past the image end".into()));
            }
            if b & 0x80 != 0 {
                counts[px..px + n].iter_mut().for_each(|c| *c += 1);
            }
            px += n;
        }
    }
    let mut runs: Vec<Run> = Vec::new();
    for &c in &counts {
        push_run(&mut runs, planes_to_gray(c as u32, aa as u32), 1);
    }
    Ok(runs)
}

// ---------------------------------------------------------------- tables

fn put_str(w: &mut LeWriter, s: &str, len: usize) {
    let b = s.as_bytes();
    let n = b.len().min(len.saturating_sub(1));
    w.bytes(&b[..n]);
    w.bytes(&vec![0u8; len - n]);
}

fn put_section(w: &mut LeWriter, name: &str, len: u32) {
    put_str(w, name, NAME_LEN);
    w.u32(len);
}

fn expect_section(r: &mut Reader, name: &str) -> Result<u32> {
    let got = r.str_fixed(NAME_LEN)?;
    if got != name {
        return Err(FormatError::Invalid(format!("expected section {name}, found {got:?}")));
    }
    r.u32()
}

/// Section addresses and counts stored at the start of the file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileMark {
    pub version: u32,
    pub tables: u32,
    pub header: u32,
    pub software: u32,
    pub preview: u32,
    pub grey_table: u32,
    pub layer_defs: u32,
    pub extra: u32,
    pub machine: u32,
    pub layer_images: u32,
    pub model: u32,
    pub sub_layer_defs: u32,
    pub preview2: u32,
}

impl FileMark {
    pub fn size(version: u32) -> u32 {
        let mut n = NAME_LEN as u32 + 9 * 4;
        if version >= VERSION_516 {
            n += 4;
        }
        if version >= VERSION_517 {
            n += 4;
        }
        if version >= VERSION_518 {
            n += 8;
        }
        n
    }

    fn table_count(version: u32) -> u32 {
        match version {
            VERSION_1 => 4,
            VERSION_515 => 5,
            VERSION_516 => 8,
            VERSION_517 => 9,
            _ => 11,
        }
    }

    fn encode(&self, w: &mut LeWriter) {
        let v = self.version;
        put_str(w, MARK, NAME_LEN);
        w.u32(v)
            .u32(self.tables)
            .u32(self.header)
            .u32(self.software)
            .u32(self.preview);
        w.u32(self.grey_table).u32(self.layer_defs).u32(self.extra);
        if v >= VERSION_516 {
            w.u32(self.machine);
        }
        w.u32(self.layer_images);
        if v >= VERSION_517 {
            w.u32(self.model);
        }
        if v >= VERSION_518 {
            w.u32(self.sub_layer_defs).u32(self.preview2);
        }
    }

    fn decode(r: &mut Reader) -> Result<Self> {
        if r.str_fixed(NAME_LEN)? != MARK {
            return Err(FormatError::Invalid("not an Anycubic Photon Workshop file".into()));
        }
        let version = r.u32()?;
        if ![VERSION_1, VERSION_515, VERSION_516, VERSION_517, VERSION_518].contains(&version) {
            return Err(FormatError::Unsupported(format!(
                "Photon Workshop file version {version}"
            )));
        }
        let mut m = FileMark {
            version,
            tables: r.u32()?,
            header: r.u32()?,
            software: r.u32()?,
            preview: r.u32()?,
            grey_table: r.u32()?,
            layer_defs: r.u32()?,
            extra: r.u32()?,
            ..Default::default()
        };
        if version >= VERSION_516 {
            m.machine = r.u32()?;
        }
        m.layer_images = r.u32()?;
        if version >= VERSION_517 {
            m.model = r.u32()?;
        }
        if version >= VERSION_518 {
            m.sub_layer_defs = r.u32()?;
            m.preview2 = r.u32()?;
        }
        Ok(m)
    }
}

/// The currency field: one UTF-16 code unit in a `u32` (`'$'` is `24 00 00 00`,
/// `'€'` is `AC 20 00 00`). Symbols outside the BMP fall back to `'$'`.
fn currency_code(currency: &str) -> u32 {
    currency.chars().next().filter(|c| (*c as u32) <= 0xFFFF).unwrap_or('$') as u32
}

fn currency_symbol(code: u32) -> String {
    char::from_u32(code)
        .filter(|&c| c != '\0' && code <= 0xFFFF)
        .map(String::from)
        .unwrap_or_default()
}

/// The `HEADER` section.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PwsHeader {
    pub pixel_size_um: f32,
    pub layer_height: f32,
    pub exposure: f32,
    /// Light-off time before each exposure, seconds.
    pub wait_before_cure: f32,
    pub bottom_exposure: f32,
    pub bottom_layers: f32,
    pub lift_height: f32,
    /// mm/s
    pub lift_speed: f32,
    /// mm/s
    pub retract_speed: f32,
    pub volume_ml: f32,
    pub anti_aliasing: u32,
    pub resolution_x: u32,
    pub resolution_y: u32,
    pub weight_g: f32,
    pub price: f32,
    pub currency: String,
    pub per_layer_settings: bool,
    pub print_time_s: u32,
    pub transition_layers: u32,
    pub transition_type: u32,
    /// v516+: 1 when the `EXTRA` two-stage motion settings apply.
    pub advanced_mode: u32,
    /// v517+
    pub grey: u16,
    pub blur_level: u16,
    pub resin_type: u32,
    /// v518+
    pub intelligent_mode: u32,
}

impl PwsHeader {
    /// Length declared for the section (excluding its 16 byte head).
    pub fn declared_len(version: u32) -> u32 {
        match version {
            v if v >= VERSION_518 => 96,
            v if v >= VERSION_517 => 92,
            v if v >= VERSION_516 => 84,
            _ => 80,
        }
    }

    fn encode(&self, version: u32, w: &mut LeWriter) {
        let len = Self::declared_len(version);
        put_section(w, "HEADER", len);
        w.f32(self.pixel_size_um)
            .f32(self.layer_height)
            .f32(self.exposure)
            .f32(self.wait_before_cure)
            .f32(self.bottom_exposure)
            .f32(self.bottom_layers)
            .f32(self.lift_height)
            .f32(self.lift_speed)
            .f32(self.retract_speed)
            .f32(self.volume_ml)
            .u32(self.anti_aliasing)
            .u32(self.resolution_x)
            .u32(self.resolution_y)
            .f32(self.weight_g)
            .f32(self.price)
            .u32(currency_code(&self.currency));
        w.u32(self.per_layer_settings as u32)
            .u32(self.print_time_s)
            .u32(self.transition_layers)
            .u32(self.transition_type);
        if len >= 84 {
            w.u32(self.advanced_mode);
        }
        if len >= 92 {
            w.u16(self.grey).u16(self.blur_level).u32(self.resin_type);
        }
        if len >= 96 {
            w.u32(self.intelligent_mode);
        }
    }

    fn decode(r: &mut Reader) -> Result<Self> {
        let len = expect_section(r, "HEADER")?;
        if len < 80 {
            return Err(FormatError::Invalid(format!(
                "HEADER section is too short ({len} bytes)"
            )));
        }
        let mut h = PwsHeader {
            pixel_size_um: r.f32()?,
            layer_height: r.f32()?,
            exposure: r.f32()?,
            wait_before_cure: r.f32()?,
            bottom_exposure: r.f32()?,
            bottom_layers: r.f32()?,
            lift_height: r.f32()?,
            lift_speed: r.f32()?,
            retract_speed: r.f32()?,
            volume_ml: r.f32()?,
            anti_aliasing: r.u32()?,
            resolution_x: r.u32()?,
            resolution_y: r.u32()?,
            weight_g: r.f32()?,
            price: r.f32()?,
            ..Default::default()
        };
        h.currency = currency_symbol(r.u32()?);
        h.per_layer_settings = r.u32()? != 0;
        h.print_time_s = r.u32()?;
        h.transition_layers = r.u32()?;
        h.transition_type = r.u32()?;
        if len >= 84 {
            h.advanced_mode = r.u32()?;
        }
        if len >= 92 {
            h.grey = r.u16()?;
            h.blur_level = r.u16()?;
            h.resin_type = r.u32()?;
        }
        if len >= 96 {
            h.intelligent_mode = r.u32()?;
        }
        Ok(h)
    }
}

/// One record of the `LAYERDEF` table.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PwsLayerDef {
    pub data_address: u32,
    pub data_length: u32,
    /// Total lift distance, mm.
    pub lift_height: f32,
    /// mm/s
    pub lift_speed: f32,
    pub exposure: f32,
    /// Thickness of this layer (difference to the previous position), mm.
    pub layer_height: f32,
    pub non_zero_pixels: u32,
}

impl PwsLayerDef {
    fn encode(&self, w: &mut LeWriter) {
        w.u32(self.data_address)
            .u32(self.data_length)
            .f32(self.lift_height)
            .f32(self.lift_speed)
            .f32(self.exposure)
            .f32(self.layer_height)
            .u32(self.non_zero_pixels)
            .u32(0);
    }

    fn decode(r: &mut Reader) -> Result<Self> {
        let d = PwsLayerDef {
            data_address: r.u32()?,
            data_length: r.u32()?,
            lift_height: r.f32()?,
            lift_speed: r.f32()?,
            exposure: r.f32()?,
            layer_height: r.f32()?,
            non_zero_pixels: r.u32()?,
        };
        r.skip(4)?;
        Ok(d)
    }
}

fn wait_before_cure(p: &LayerParams, wait_mode: bool) -> f32 {
    if wait_mode {
        p.wait_after_retract
    } else {
        p.light_off_delay
    }
}

fn uses_two_stage_motion(s: &PrintSettings) -> bool {
    [&s.bottom, &s.normal]
        .iter()
        .any(|p| p.lift.distance2 > 0.0 || p.retract.distance2 > 0.0)
}

fn round_to(v: f32, decimals: i32) -> f32 {
    let f = 10f32.powi(decimals);
    (v * f).round() / f
}

fn mm_s(mm_min: f32) -> f32 {
    mm_min / 60.0
}

/// `EXTRA`: for bottom and normal layers two motion segments of
/// `(height, lift speed, retract speed)`. Segment 1 is the part next to the vat,
/// so its retract speed is the slow second retract stage.
fn encode_extra(s: &PrintSettings, w: &mut LeWriter) {
    put_section(w, "EXTRA", EXTRA_DECLARED_LEN);
    for p in [&s.bottom, &s.normal] {
        let lift2_speed = if p.lift.distance2 > 0.0 && p.lift.speed2 > 0.0 {
            p.lift.speed2
        } else {
            p.lift.speed
        };
        let near_retract = if p.retract.distance2 > 0.0 && p.retract.speed2 > 0.0 {
            p.retract.speed2
        } else {
            p.retract.speed
        };
        w.u32(2);
        w.f32(p.lift.distance).f32(mm_s(p.lift.speed)).f32(mm_s(near_retract));
        w.f32(p.lift.distance2)
            .f32(mm_s(lift2_speed))
            .f32(mm_s(p.retract.speed));
    }
}

fn encode_preview_section(w: &mut LeWriter, second: bool, size: (u32, u32), thumb: Option<&Thumbnail>) {
    let (pw, ph) = size;
    let data_len = pw * ph * 2;
    put_section(
        w,
        if second { "PREVIEW2" } else { "PREVIEW" },
        SECTION_HEAD + 12 + data_len,
    );
    w.u32(pw);
    if second {
        w.u16(PREVIEW2_BACKGROUND).u16(0);
    } else {
        put_str(w, "x", 4);
    }
    w.u32(ph);
    match thumb {
        Some(t) => {
            for px in t.fit(pw, ph).to_rgb565() {
                w.u16(px);
            }
        }
        None => {
            w.bytes(&vec![0u8; data_len as usize]);
        }
    }
}

// ---------------------------------------------------------------- writer

pub struct AnycubicWriter {
    out: Box<dyn WriteSeek>,
    model: AnycubicModel,
    version: u32,
    aa: u8,
    layer_count: u32,
    params: [LayerParams; 2],
    head_len: u64,
    next_data: u64,
    prev_z: f32,
    defs: Vec<PwsLayerDef>,
    bbox: Option<(u32, u32, u32, u32)>,
}

impl AnycubicWriter {
    pub fn new(out: Box<dyn WriteSeek>, model: AnycubicModel) -> Self {
        let p = PrintSettings::default().normal;
        Self {
            out,
            model,
            version: model.version(),
            aa: 1,
            layer_count: 0,
            params: [p, p],
            head_len: 0,
            next_data: 0,
            prev_z: 0.0,
            defs: Vec::new(),
            bbox: None,
        }
    }

    fn machine_name(&self, p: &PrinterProfile) -> String {
        let name = p.machine_type();
        let name = name.trim();
        let stripped = match name.get(..8) {
            Some(prefix) if prefix.eq_ignore_ascii_case("anycubic") => name[8..].trim(),
            _ => name,
        };
        if stripped.is_empty() {
            self.model.default_machine_name().to_string()
        } else {
            stripped.to_string()
        }
    }

    fn header(&self, info: &JobInfo, est: Option<&PrintEstimate>) -> PwsHeader {
        let p = info.printer;
        let s = info.settings;
        let (px, py) = p.pixel_size_um();
        PwsHeader {
            pixel_size_um: px.max(py),
            layer_height: s.layer_height,
            exposure: s.normal.exposure,
            wait_before_cure: wait_before_cure(&s.normal, s.wait_mode),
            bottom_exposure: s.bottom.exposure,
            bottom_layers: s.bottom_layers as f32,
            lift_height: s.normal.lift.total_distance(),
            lift_speed: mm_s(s.normal.lift.speed),
            retract_speed: mm_s(s.normal.retract.speed),
            volume_ml: est.map(|e| e.volume_ml() as f32).unwrap_or(0.0),
            anti_aliasing: self.aa as u32,
            resolution_x: p.resolution_x,
            resolution_y: p.resolution_y,
            weight_g: est.map(|e| e.weight_g as f32).unwrap_or(0.0),
            price: est.map(|e| e.cost as f32).unwrap_or(0.0),
            currency: s.currency.clone(),
            per_layer_settings: false,
            print_time_s: est.map(|e| e.print_time_s.round() as u32).unwrap_or(0),
            transition_layers: s.transition_layers,
            transition_type: 0,
            advanced_mode: uses_two_stage_motion(s) as u32,
            grey: 0,
            blur_level: 0,
            resin_type: 0,
            intelligent_mode: 0,
        }
    }

    /// Everything before the first layer image. The size only depends on the
    /// version and the layer count, so it can be rewritten in `finish`.
    fn build_head(&self, info: &JobInfo, est: Option<&PrintEstimate>) -> Vec<u8> {
        let v = self.version;
        let p = info.printer;
        let s = info.settings;
        let n = self.layer_count;
        let base = FileMark::size(v);
        let mut mark = FileMark {
            version: v,
            tables: FileMark::table_count(v),
            ..Default::default()
        };
        let mut w = LeWriter::default();
        let pos = |w: &LeWriter| base + w.buf.len() as u32;

        mark.header = pos(&w);
        self.header(info, est).encode(v, &mut w);

        mark.preview = pos(&w);
        encode_preview_section(&mut w, false, PREVIEW_SIZE, info.thumbnail);

        if v >= VERSION_515 {
            mark.grey_table = pos(&w);
            let step = 255.0 / self.aa.max(1) as f32;
            w.u32(0).u32(16);
            for i in 0..16 {
                w.u8(((i + 1) as f32 * step).min(255.0) as u8);
            }
            w.u32(0);
        }

        mark.layer_defs = pos(&w);
        put_section(&mut w, "LAYERDEF", 4 + LAYER_DEF_SIZE * n);
        w.u32(n);
        for i in 0..n as usize {
            self.defs.get(i).copied().unwrap_or_default().encode(&mut w);
        }

        if v >= VERSION_516 {
            mark.extra = pos(&w);
            encode_extra(s, &mut w);

            mark.machine = pos(&w);
            let machine_len = if v >= VERSION_518 { 224 } else { 156 };
            put_section(&mut w, "MACHINE", machine_len);
            put_str(&mut w, &self.machine_name(p), 96);
            let codec = match self.model.codec() {
                LayerCodec::Pw0 => "pw0Img",
                LayerCodec::Pws => "pwsImg",
            };
            put_str(&mut w, codec, 16);
            let property_fields = match v {
                VERSION_516 => 1,
                VERSION_517 => 7,
                _ => 15,
            };
            w.u32(16).u32(property_fields);
            w.f32(p.size_x).f32(p.size_y).f32(p.size_z);
            w.u32(v).u32(MACHINE_BACKGROUND);
            if v >= VERSION_518 {
                let (px, py) = p.pixel_size_um();
                w.f32(px).f32(py);
                w.bytes(&[0u8; 32]);
                w.u32(1).u32(0);
                w.u16(p.resolution_x.min(u16::MAX as u32) as u16)
                    .u16(p.resolution_y.min(u16::MAX as u32) as u16);
                w.bytes(&[0u8; 16]);
            }
        }

        if v >= VERSION_517 {
            mark.software = pos(&w);
            put_str(&mut w, "OpenResin", 32);
            w.u32(SOFTWARE_SIZE);
            put_str(&mut w, env!("CARGO_PKG_VERSION"), 32);
            put_str(
                &mut w,
                &format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
                64,
            );
            put_str(&mut w, "", 32);

            mark.model = pos(&w);
            put_section(&mut w, "MODEL", MODEL_SIZE);
            let (sx, sy) = (p.size_x, p.size_y);
            let (px, py) = (sx / p.resolution_x.max(1) as f32, sy / p.resolution_y.max(1) as f32);
            let (x0, y0, x1, y1) = self.bbox.unwrap_or((0, 0, 0, 0));
            let mm = |v: u32, pitch: f32, size: f32| round_to(v as f32 * pitch - size / 2.0, 4);
            let height = if self.prev_z > 0.0 {
                self.prev_z
            } else {
                n as f32 * s.layer_height
            };
            w.f32(mm(x0, px, sx)).f32(mm(y0, py, sy)).f32(0.0);
            w.f32(mm(x1, px, sx)).f32(mm(y1, py, sy)).f32(round_to(height, 4));
            w.u32(0).f32(0.0);
        }

        if v >= VERSION_518 {
            mark.sub_layer_defs = pos(&w);
            put_section(&mut w, "SUBIMGS", SECTION_HEAD + 8 + SUB_LAYER_DEF_SIZE * n);
            w.u32(n).u32(1);
            for i in 0..n as usize {
                let d = self.defs.get(i).copied().unwrap_or_default();
                w.u32(d.data_address).u32(d.data_length).u32(d.non_zero_pixels);
                w.bytes(&[0u8; 32]);
            }

            mark.preview2 = pos(&w);
            encode_preview_section(&mut w, true, PREVIEW2_SIZE, info.thumbnail);
        }

        mark.layer_images = pos(&w);
        let mut out = LeWriter::default();
        mark.encode(&mut out);
        debug_assert_eq!(out.buf.len() as u32, base);
        out.bytes(&w.buf);
        out.buf
    }
}

impl FormatWriter for AnycubicWriter {
    fn begin(&mut self, info: &JobInfo) -> Result<()> {
        let s = info.settings;
        self.version = self.model.version_for(info.printer);
        self.aa = s.anti_aliasing.clamp(1, 16);
        self.layer_count = info.layer_count;
        self.params = [s.bottom, s.normal];
        self.defs = Vec::with_capacity(info.layer_count as usize);
        self.prev_z = 0.0;
        self.bbox = None;
        let head = self.build_head(info, None);
        self.head_len = head.len() as u64;
        self.next_data = self.head_len;
        self.out.seek(SeekFrom::Start(0))?;
        self.out.write_all(&head)?;
        Ok(())
    }

    fn encode_layer(&self, _index: u32, raster: &LayerRaster) -> Result<Vec<u8>> {
        let (non_zero, mut out) = match self.model.codec() {
            LayerCodec::Pw0 => {
                let nz = raster
                    .runs
                    .iter()
                    .filter(|r| pw0_level(r.value) != 0)
                    .map(|r| r.len)
                    .sum::<u32>();
                (nz, Vec::with_capacity(PREFIX_LEN + raster.runs.len() * 2 + 4))
            }
            LayerCodec::Pws => {
                let t = plane_threshold(1, self.aa as u32);
                let nz = raster.runs.iter().filter(|r| r.value >= t).map(|r| r.len).sum::<u32>();
                (
                    nz,
                    Vec::with_capacity(PREFIX_LEN + raster.runs.len() * self.aa as usize + 16),
                )
            }
        };
        let (x0, y0, x1, y1) = raster.bbox.unwrap_or((u32::MAX, u32::MAX, 0, 0));
        for v in [non_zero, x0, y0, x1, y1] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        match self.model.codec() {
            LayerCodec::Pw0 => encode_pw0_into(&raster.runs, &mut out),
            LayerCodec::Pws => encode_planes_into(&raster.runs, self.aa, &mut out),
        }
        Ok(out)
    }

    fn write_layer(&mut self, meta: &LayerMeta, encoded: Vec<u8>) -> Result<()> {
        if encoded.len() < PREFIX_LEN {
            return Err(FormatError::Invalid("layer was not encoded by this writer".into()));
        }
        let word = |k: usize| u32::from_le_bytes(encoded[k * 4..k * 4 + 4].try_into().unwrap());
        let (non_zero, x0, y0, x1, y1) = (word(0), word(1), word(2), word(3), word(4));
        let data = &encoded[PREFIX_LEN..];
        let addr = self.next_data;
        if addr + data.len() as u64 > u32::MAX as u64 {
            return Err(FormatError::Unsupported(
                "Photon Workshop files are limited to 4 GiB".into(),
            ));
        }
        self.out.write_all(data)?;
        let p = &self.params[if meta.is_bottom { 0 } else { 1 }];
        self.defs.push(PwsLayerDef {
            data_address: addr as u32,
            data_length: data.len() as u32,
            lift_height: p.lift.total_distance(),
            lift_speed: mm_s(p.lift.speed),
            exposure: meta.exposure,
            layer_height: round_to(meta.position_z - self.prev_z, 5),
            non_zero_pixels: non_zero,
        });
        self.prev_z = meta.position_z;
        if x1 > x0 {
            self.bbox = Some(match self.bbox {
                None => (x0, y0, x1, y1),
                Some((a, b, c, d)) => (a.min(x0), b.min(y0), c.max(x1), d.max(y1)),
            });
        }
        self.next_data = addr + data.len() as u64;
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
            return Err(FormatError::Invalid("Photon Workshop header changed size".into()));
        }
        self.out.seek(SeekFrom::Start(0))?;
        self.out.write_all(&head)?;
        self.out.seek(SeekFrom::End(0))?;
        self.out.flush()?;
        Ok(())
    }
}

// ---------------------------------------------------------------- reader

pub struct AnycubicFile {
    pub mark: FileMark,
    pub header: PwsHeader,
    pub layers: Vec<PwsLayerDef>,
    pub codec: LayerCodec,
    z: Vec<f32>,
    /// (width, height, data offset) of the first preview found.
    preview: Option<(u32, u32, u64)>,
    info: SlicedInfo,
    file: Mutex<BufReader<File>>,
}

fn read_at(f: &mut BufReader<File>, pos: u64, len: usize) -> Result<Vec<u8>> {
    f.seek(SeekFrom::Start(pos))?;
    let mut buf = vec![0u8; len];
    f.read_exact(&mut buf)?;
    Ok(buf)
}

impl AnycubicFile {
    /// Opens a file of the given family (the codec is taken from the `MACHINE`
    /// section when present, otherwise from the family).
    pub fn open(path: &Path, model: AnycubicModel) -> Result<Self> {
        let mut f = BufReader::new(File::open(path)?);
        let file_len = f.get_ref().metadata()?.len();
        let head = read_at(&mut f, 0, (FileMark::size(VERSION_518) as u64).min(file_len) as usize)?;
        let mark = FileMark::decode(&mut Reader::le(&head, 0))?;

        let buf = read_at(&mut f, mark.header as u64, (SECTION_HEAD + 96) as usize)
            .or_else(|_| read_at(&mut f, mark.header as u64, (SECTION_HEAD + 80) as usize))?;
        let header = PwsHeader::decode(&mut Reader::le(&buf, 0))?;

        let mut preview = None;
        for (addr, second) in [(mark.preview, false), (mark.preview2, true)] {
            if addr == 0 || preview.is_some() {
                continue;
            }
            let b = read_at(&mut f, addr as u64, (SECTION_HEAD + 12) as usize)?;
            let mut r = Reader::le(&b, 0);
            expect_section(&mut r, if second { "PREVIEW2" } else { "PREVIEW" })?;
            let w = r.u32()?;
            r.skip(4)?;
            let h = r.u32()?;
            if w > 0 && h > 0 && w <= 4096 && h <= 4096 {
                preview = Some((w, h, addr as u64 + (SECTION_HEAD + 12) as u64));
            }
        }

        let mut codec = model.codec();
        let mut machine_name = model.default_machine_name().to_string();
        let mut size = None;
        if mark.machine != 0 {
            let b = read_at(&mut f, mark.machine as u64, (SECTION_HEAD + 96 + 16 + 20) as usize)?;
            let mut r = Reader::le(&b, 0);
            expect_section(&mut r, "MACHINE")?;
            machine_name = r.str_fixed(96)?;
            match r.str_fixed(16)?.to_ascii_lowercase().as_str() {
                "pwsimg" => codec = LayerCodec::Pws,
                "pw0img" => codec = LayerCodec::Pw0,
                _ => {}
            }
            r.skip(8)?;
            size = Some((r.f32()?, r.f32()?));
        }

        let b = read_at(&mut f, mark.layer_defs as u64, (SECTION_HEAD + 4) as usize)?;
        let mut r = Reader::le(&b, 0);
        let len = expect_section(&mut r, "LAYERDEF")?;
        let count = r.u32()?;
        // in u64: a damaged count must not overflow
        let table_len = LAYER_DEF_SIZE as u64 * count as u64;
        if len as u64 != 4 + table_len {
            return Err(FormatError::Invalid(format!(
                "LAYERDEF section length {len} does not match {count} layers"
            )));
        }
        let table_at = mark.layer_defs as u64 + (SECTION_HEAD + 4) as u64;
        if table_at + table_len > file_len {
            return Err(FormatError::Invalid(format!(
                "the layer table of {count} layers runs past the end of the file"
            )));
        }
        let table = read_at(&mut f, table_at, table_len as usize)?;
        let mut r = Reader::le(&table, 0);
        let layers = (0..count)
            .map(|_| PwsLayerDef::decode(&mut r))
            .collect::<Result<Vec<_>>>()?;
        for (i, d) in layers.iter().enumerate() {
            if d.data_address as u64 + d.data_length as u64 > file_len {
                return Err(FormatError::Invalid(format!("layer {i} runs past the end of the file")));
            }
        }
        let mut acc = 0f64;
        let z = layers
            .iter()
            .map(|d| {
                acc += d.layer_height as f64;
                acc as f32
            })
            .collect();

        let (size_x, size_y) = size.unwrap_or((
            header.resolution_x as f32 * header.pixel_size_um / 1000.0,
            header.resolution_y as f32 * header.pixel_size_um / 1000.0,
        ));
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or(model.extension());
        let info = SlicedInfo {
            format: ext.to_ascii_lowercase(),
            machine_name,
            resolution_x: header.resolution_x,
            resolution_y: header.resolution_y,
            size_x,
            size_y,
            layer_count: count,
            layer_height: header.layer_height,
            exposure: header.exposure,
            bottom_exposure: header.bottom_exposure,
            bottom_layers: header.bottom_layers.max(0.0) as u32,
            print_time_s: header.print_time_s,
            volume_ml: header.volume_ml,
            mirror_x: false,
            mirror_y: false,
            anti_aliasing: header.anti_aliasing,
        };
        Ok(Self {
            mark,
            header,
            layers,
            codec,
            z,
            preview,
            info,
            file: Mutex::new(f),
        })
    }

    /// The encoded image of a layer (its range was checked against the file
    /// length in [`AnycubicFile::open`]).
    pub fn layer_bytes(&self, index: u32) -> Result<Vec<u8>> {
        let d = self
            .layers
            .get(index as usize)
            .ok_or_else(|| FormatError::Invalid(format!("layer {index} out of range")))?;
        let mut f = self.file.lock().unwrap();
        read_at(&mut f, d.data_address as u64, d.data_length as usize)
    }
}

impl SlicedFile for AnycubicFile {
    fn info(&self) -> &SlicedInfo {
        &self.info
    }

    fn layer(&self, index: u32) -> Result<LayerRaster> {
        let data = self.layer_bytes(index)?;
        let (w, h) = (self.info.resolution_x, self.info.resolution_y);
        let pixels = w as u64 * h as u64;
        let runs = match self.codec {
            LayerCodec::Pw0 => decode_pw0(&data, pixels)?,
            LayerCodec::Pws => decode_planes(&data, self.header.anti_aliasing.min(255) as u8, pixels)?,
        };
        Ok(LayerRaster::from_runs(w, h, runs))
    }

    fn layer_z(&self, index: u32) -> f32 {
        self.z.get(index as usize).copied().unwrap_or(0.0)
    }

    fn thumbnail(&self) -> Option<Thumbnail> {
        let (w, h, off) = self.preview?;
        let mut f = self.file.lock().ok()?;
        let data = read_at(&mut f, off, (w * h * 2) as usize).ok()?;
        let px: Vec<u16> = data.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        Some(Thumbnail::from_rgb565(w, h, &px))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Format, JobInfo};
    use openresin_core::profile::MirrorMode;

    fn total(runs: &[Run]) -> u64 {
        runs.iter().map(|r| r.len as u64).sum()
    }

    #[test]
    fn pw0_known_bytes() {
        // black and white runs carry a 12-bit length in two bytes
        assert_eq!(encode_pw0(&[Run { value: 0, len: 5 }]), vec![0x00, 0x05]);
        assert_eq!(encode_pw0(&[Run { value: 255, len: 0x123 }]), vec![0xF1, 0x23]);
        // grey runs use the low nibble, 128 -> level 8
        assert_eq!(encode_pw0(&[Run { value: 128, len: 3 }]), vec![0x83]);
        let dec = decode_pw0(&[0x00, 0x05, 0x83, 0xF0, 0x02], 10).unwrap();
        assert_eq!(
            dec,
            vec![
                Run { value: 0, len: 5 },
                Run { value: 136, len: 3 },
                Run { value: 255, len: 2 }
            ]
        );
    }

    #[test]
    fn pw0_long_runs_split() {
        // 10000 = 4095 + 4095 + 1810 (0x712)
        assert_eq!(
            encode_pw0(&[Run { value: 0, len: 10_000 }]),
            vec![0x0F, 0xFF, 0x0F, 0xFF, 0x07, 0x12]
        );
        // grey runs are split into chunks of 15
        assert_eq!(encode_pw0(&[Run { value: 128, len: 40 }]), vec![0x8F, 0x8F, 0x8A]);
        let runs = vec![
            Run {
                value: 255,
                len: 0x0FFF_FFFF,
            },
            Run { value: 17, len: 1000 },
            Run { value: 0, len: 4096 },
        ];
        let enc = encode_pw0(&runs);
        assert_eq!(decode_pw0(&enc, total(&runs)).unwrap(), runs);
    }

    #[test]
    fn pw0_quantisation() {
        assert_eq!(quantize_pw0(0), 0);
        assert_eq!(quantize_pw0(8), 0);
        assert_eq!(quantize_pw0(9), 17);
        assert_eq!(quantize_pw0(128), 136);
        assert_eq!(quantize_pw0(246), 238);
        assert_eq!(quantize_pw0(247), 255);
        assert_eq!(quantize_pw0(255), 255);
        for v in 0..=255u8 {
            assert!((quantize_pw0(v) as i32 - v as i32).abs() <= 8, "{v}");
        }
        // runs that land on the same level merge into one code
        let enc = encode_pw0(&[Run { value: 200, len: 4 }, Run { value: 201, len: 4 }]);
        assert_eq!(enc, vec![0xC8]);
        // every level survives a round trip
        let runs: Vec<Run> = (0..16u8).map(|l| Run { value: l * 17, len: 3 }).collect();
        assert_eq!(decode_pw0(&encode_pw0(&runs), 48).unwrap(), runs);
    }

    #[test]
    fn pw0_decoder_edge_cases() {
        // empty data is a blank layer
        assert_eq!(decode_pw0(&[], 7).unwrap(), vec![Run { value: 0, len: 7 }]);
        // a final black/white code without its length byte fills the image
        assert_eq!(
            decode_pw0(&[0x83, 0xF0], 10).unwrap(),
            vec![Run { value: 136, len: 3 }, Run { value: 255, len: 7 }]
        );
        // trailing data after the last pixel (e.g. a checksum) is ignored
        assert_eq!(
            decode_pw0(&[0x00, 0x04, 0xAB, 0xCD], 4).unwrap(),
            vec![Run { value: 0, len: 4 }]
        );
        assert!(decode_pw0(&[0x00, 0x09], 4).is_err());
        assert!(decode_pw0(&[0x00, 0x02], 4).is_err());
    }

    #[test]
    fn planes_roundtrip_and_quantisation() {
        // thresholds for 4 planes: 52, 103, 154, 205 -> 63, 127, 191, 255
        assert_eq!(quantize_planes(51, 4), 0);
        assert_eq!(quantize_planes(52, 4), 63);
        assert_eq!(quantize_planes(103, 4), 127);
        assert_eq!(quantize_planes(204, 4), 191);
        assert_eq!(quantize_planes(205, 4), 255);
        assert_eq!(quantize_planes(127, 1), 0);
        assert_eq!(quantize_planes(128, 1), 255);
        for aa in [1u8, 2, 4, 8, 16] {
            let runs: Vec<Run> = (0..=255u8)
                .map(|v| Run {
                    value: v,
                    len: 1 + v as u32 % 7,
                })
                .collect();
            let n = total(&runs);
            let dec = decode_planes(&encode_planes(&runs, aa), aa, n).unwrap();
            let got = LayerRaster::from_runs(n as u32, 1, dec).to_gray();
            let want: Vec<u8> = LayerRaster::from_runs(n as u32, 1, runs.clone())
                .to_gray()
                .into_iter()
                .map(|v| quantize_planes(v, aa))
                .collect();
            assert_eq!(got, want, "aa {aa}");
        }
    }

    /// A decoded grey encodes to the same planes again, for every level count
    /// (the reference's value for one plane fell below its threshold at 13, 14
    /// and 16 levels).
    #[test]
    fn planes_quantisation_is_idempotent() {
        for aa in 1..=16u8 {
            for v in 0..=255u8 {
                let q = quantize_planes(v, aa);
                assert_eq!(quantize_planes(q, aa), q, "aa {aa}: {v} -> {q}");
            }
        }
        assert_eq!(quantize_planes(16, 16), 16);
        assert_eq!(quantize_planes(15, 16), 0);
        // the other levels keep the reference values
        assert_eq!(quantize_planes(255, 16), 255);
        assert_eq!(quantize_planes(31, 16), 31);
    }

    /// The image buffer is only allocated when the data can cover it.
    #[test]
    fn planes_reject_data_too_short_for_the_resolution() {
        let e = decode_planes(&[0x80 | 125; 8], 1, 1 << 40).unwrap_err();
        assert!(e.to_string().contains("too short"), "{e}");
        assert!(decode_planes(&encode_planes(&[Run { value: 255, len: 300 }], 2)[..5], 2, 300).is_err());
    }

    #[test]
    fn planes_long_runs() {
        // 300 pixels: runs of 125 + 125 + 50 per plane
        let enc = encode_planes(&[Run { value: 255, len: 300 }], 1);
        assert_eq!(enc, vec![0x80 | 125, 0x80 | 125, 0x80 | 50]);
        let enc = encode_planes(&[Run { value: 0, len: 100 }, Run { value: 255, len: 100 }], 2);
        assert_eq!(enc, vec![100, 0x80 | 100, 100, 0x80 | 100]);
        assert!(decode_planes(&enc[..3], 2, 200).is_err());
        assert!(decode_planes(&[0x80 | 120], 1, 100).is_err());
    }

    /// UVtools reads the currency as a UTF-16 code unit; UTF-8 bytes showed
    /// `€` as `苢` (U+82E2).
    #[test]
    fn currency_is_a_utf16_code_unit() {
        for (symbol, bytes, back) in [
            ("$", [0x24, 0, 0, 0], "$"),
            ("€", [0xAC, 0x20, 0, 0], "€"),
            ("¥ (yen)", [0xA5, 0, 0, 0], "¥"),
            ("", [0x24, 0, 0, 0], "$"),
            ("𝄞", [0x24, 0, 0, 0], "$"),
        ] {
            let h = PwsHeader {
                currency: symbol.into(),
                ..Default::default()
            };
            let mut w = LeWriter::default();
            h.encode(VERSION_516, &mut w);
            // section head, then 15 fields before the currency
            let at = SECTION_HEAD as usize + 15 * 4;
            assert_eq!(w.buf[at..at + 4], bytes, "{symbol}");
            assert_eq!(PwsHeader::decode(&mut Reader::le(&w.buf, 0)).unwrap().currency, back);
        }
        assert_eq!(currency_symbol(0), "");
        assert_eq!(currency_symbol(0x0001_0024), "");
    }

    fn job(version_model: AnycubicModel, layers: u32, path: &Path) -> Vec<u8> {
        let p = PrinterProfile {
            id: "t".into(),
            vendor: "Anycubic".into(),
            name: "Anycubic Photon Test".into(),
            resolution_x: 40,
            resolution_y: 30,
            size_x: 20.0,
            size_y: 15.0,
            size_z: 100.0,
            mirror: MirrorMode::Lcd,
            formats: vec![version_model.extension().into()],
            native_formats: Vec::new(),
            network: None,
            machine_type: None,
        };
        let s = PrintSettings::default();
        let info = JobInfo {
            printer: &p,
            settings: &s,
            layer_count: layers,
            thumbnail: None,
            job_name: "t",
        };
        let mut w = AnycubicWriter::new(Box::new(File::create(path).unwrap()), version_model);
        w.begin(&info).unwrap();
        for i in 0..layers {
            let mut px = vec![0u8; 40 * 30];
            let k = (i as usize * 41) % px.len();
            px[k] = 255;
            let r = LayerRaster::from_gray(40, 30, &px);
            let enc = w.encode_layer(i, &r).unwrap();
            let meta = LayerMeta {
                index: i,
                position_z: (i + 1) as f32 * 0.05,
                exposure: s.exposure_for_layer(i),
                is_bottom: i < s.bottom_layers,
                area_mm2: 0.25,
            };
            w.write_layer(&meta, enc).unwrap();
        }
        w.finish(&info, &PrintEstimate::default()).unwrap();
        drop(w);
        std::fs::read(path).unwrap()
    }

    /// Checks the section layout of every version against the documented sizes.
    #[test]
    fn section_layout() {
        let dir = std::env::temp_dir().join(format!("openresin-pws-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (model, version) in [
            (AnycubicModel::Pw0, 1),
            (AnycubicModel::Pwmx, 516),
            (AnycubicModel::Pm5, 517),
            (AnycubicModel::Pm5s, 518),
        ] {
            let n = 6u32;
            let path = dir.join(format!("t.{}", model.extension()));
            let data = job(model, n, &path);
            let mut r = Reader::le(&data, 0);
            let mark = FileMark::decode(&mut r).unwrap();
            assert_eq!(mark.version, version);
            assert_eq!(r.pos as u32, FileMark::size(version));
            assert_eq!(mark.header, FileMark::size(version));
            let at = |addr: u32| Reader::le(&data, addr as usize);
            let section = |addr: u32, name: &str| expect_section(&mut at(addr), name).unwrap();
            assert_eq!(section(mark.header, "HEADER"), PwsHeader::declared_len(version));
            assert_eq!(
                mark.preview,
                mark.header + SECTION_HEAD + PwsHeader::declared_len(version)
            );
            assert_eq!(section(mark.preview, "PREVIEW"), 28 + 224 * 168 * 2);
            let table_at = mark.preview + 28 + 224 * 168 * 2;
            if version >= 515 {
                assert_eq!(mark.grey_table, table_at);
                assert_eq!(mark.layer_defs, table_at + 28);
            } else {
                assert_eq!(mark.grey_table, 0);
                assert_eq!(mark.layer_defs, table_at);
            }
            assert_eq!(section(mark.layer_defs, "LAYERDEF"), 4 + 32 * n);
            let after_defs = mark.layer_defs + SECTION_HEAD + 4 + 32 * n;
            if version >= 516 {
                assert_eq!(mark.extra, after_defs);
                assert_eq!(section(mark.extra, "EXTRA"), 24);
                assert_eq!(mark.machine, mark.extra + 72);
                let mlen = section(mark.machine, "MACHINE");
                assert_eq!(mlen, if version >= 518 { 224 } else { 156 });
                let mut m = at(mark.machine + 16);
                assert_eq!(m.str_fixed(96).unwrap(), "Photon Test");
                assert_eq!(m.str_fixed(16).unwrap(), "pw0Img");
                if version >= 517 {
                    assert_eq!(mark.software, mark.machine + mlen);
                    let mut sw = at(mark.software);
                    assert_eq!(sw.str_fixed(32).unwrap(), "OpenResin");
                    assert_eq!(sw.u32().unwrap(), 164);
                    assert_eq!(mark.model, mark.software + 164);
                    assert_eq!(section(mark.model, "MODEL"), 48);
                }
                if version >= 518 {
                    assert_eq!(mark.sub_layer_defs, mark.model + 48);
                    assert_eq!(section(mark.sub_layer_defs, "SUBIMGS"), 24 + 44 * n);
                    assert_eq!(mark.preview2, mark.sub_layer_defs + 24 + 44 * n);
                    assert_eq!(section(mark.preview2, "PREVIEW2"), 28 + 320 * 190 * 2);
                    assert_eq!(mark.layer_images, mark.preview2 + 28 + 320 * 190 * 2);
                    // the second table points at the same images
                    let mut sub = at(mark.sub_layer_defs + 24);
                    let mut defs = at(mark.layer_defs + 20);
                    for _ in 0..n {
                        let d = PwsLayerDef::decode(&mut defs).unwrap();
                        assert_eq!(sub.u32().unwrap(), d.data_address);
                        assert_eq!(sub.u32().unwrap(), d.data_length);
                        assert_eq!(sub.u32().unwrap(), d.non_zero_pixels);
                        sub.skip(32).unwrap();
                    }
                } else if version == 517 {
                    assert_eq!(mark.layer_images, mark.model + 48);
                } else {
                    assert_eq!(mark.layer_images, mark.machine + 156);
                }
            } else {
                assert_eq!((mark.extra, mark.machine, mark.software), (0, 0, 0));
                assert_eq!(mark.layer_images, after_defs);
            }
            // layers are stored back to back after the tables and decode
            let f = AnycubicFile::open(&path, model).unwrap();
            assert_eq!(f.layers[0].data_address, mark.layer_images);
            for w in f.layers.windows(2) {
                assert_eq!(w[0].data_address + w[0].data_length, w[1].data_address);
            }
            assert_eq!(f.layers[0].non_zero_pixels, 1);
            assert!((f.layer_z(n - 1) - 0.3).abs() < 1e-4);
            assert!((f.layers[1].layer_height - 0.05).abs() < 1e-6);
            for i in 0..n {
                assert_eq!(f.layer(i).unwrap().lit_area_px(), 1.0);
            }
            assert_eq!(f.info().format, model.extension());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Layer data past the end of the file is reported when opening.
    #[test]
    fn truncated_layer_data_is_an_error() {
        let path = std::env::temp_dir().join(format!("openresin-pws-trunc-{}.pwmx", std::process::id()));
        let data = job(AnycubicModel::Pwmx, 4, &path);
        assert!(AnycubicFile::open(&path, AnycubicModel::Pwmx).is_ok());
        std::fs::write(&path, &data[..data.len() - 1]).unwrap();
        let e = AnycubicFile::open(&path, AnycubicModel::Pwmx).err().unwrap();
        assert!(e.to_string().contains("layer 3 runs past the end"), "{e}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn model_table_is_consistent() {
        for m in AnycubicModel::ALL {
            assert_eq!(AnycubicModel::from_extension(m.extension()), Some(m));
            assert_eq!(Format::from_extension(m.extension()), Some(Format::Anycubic(m)));
            let json = serde_json::to_string(&m).unwrap();
            assert_eq!(json, format!("\"{}\"", m.extension()));
            assert!([1, 516, 517, 518].contains(&m.version()));
        }
        let mut exts: Vec<_> = AnycubicModel::ALL.iter().map(|m| m.extension()).collect();
        exts.sort();
        exts.dedup();
        assert_eq!(exts.len(), AnycubicModel::COUNT);
    }
}
