//! Printer file formats.
//!
//! Writers implement [`FormatWriter`]; layers are encoded in parallel
//! ([`FormatWriter::encode_layer`] takes `&self`) and written in order.
//! Readers implement [`SlicedFile`] for previews and verification.

pub mod anycubic;
mod bin;
pub mod ctb;
pub mod cxdlp;
pub mod cxdlpv4;
pub mod export;
pub mod goo;
pub mod pngzip;
pub mod preview;
pub mod sl1;

use std::io::{Seek, Write};
use std::path::Path;

use openresin_core::estimate::PrintEstimate;
use openresin_core::profile::{PrintSettings, PrinterProfile};
use openresin_core::slicer::LayerRaster;
use serde::{Deserialize, Serialize};

pub use anycubic::AnycubicModel;
pub use export::{slice_to_file, ExportJob, ExportProgress, ExportResult};
pub use preview::Thumbnail;

#[derive(Debug, thiserror::Error)]
pub enum FormatError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("image error: {0}")]
    Image(#[from] image::ImageError),
    #[error("invalid file: {0}")]
    Invalid(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("cancelled")]
    Cancelled,
}

pub type Result<T, E = FormatError> = std::result::Result<T, E>;

/// Output formats supported for writing.
///
/// Serialised as the lowercase file extension (`"goo"`, `"cxdlpv4"`, `"pwmx"`, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// Elegoo GOO (open specification).
    Goo,
    /// Chitubox CTB, classic (unencrypted header) v3 layout.
    Ctb,
    /// Prusa SL1/SL1S zip.
    Sl1,
    /// Generic zip of PNG layer images plus a JSON manifest.
    Zip,
    /// Creality HALOT CXDLP (header version 3).
    Cxdlp,
    /// Creality HALOT CXDLPv4.
    Cxdlpv4,
    /// Anycubic Photon Workshop container; one variant per printer family.
    #[serde(untagged)]
    Anycubic(AnycubicModel),
}

impl Format {
    /// Every format, native printer formats before the generic ones.
    pub const ALL: [Format; 6 + AnycubicModel::COUNT] = {
        let mut all = [Format::Zip; 6 + AnycubicModel::COUNT];
        all[0] = Format::Goo;
        all[1] = Format::Ctb;
        all[2] = Format::Cxdlp;
        all[3] = Format::Cxdlpv4;
        let mut i = 0;
        while i < AnycubicModel::COUNT {
            all[4 + i] = Format::Anycubic(AnycubicModel::ALL[i]);
            i += 1;
        }
        all[4 + AnycubicModel::COUNT] = Format::Sl1;
        all[5 + AnycubicModel::COUNT] = Format::Zip;
        all
    };

    pub fn extension(self) -> &'static str {
        match self {
            Format::Goo => "goo",
            Format::Ctb => "ctb",
            Format::Sl1 => "sl1",
            Format::Zip => "zip",
            Format::Cxdlp => "cxdlp",
            Format::Cxdlpv4 => "cxdlpv4",
            Format::Anycubic(m) => m.extension(),
        }
    }

    pub fn from_extension(ext: &str) -> Option<Format> {
        let ext = ext.trim_start_matches('.').to_ascii_lowercase();
        if ext == "sl1s" {
            return Some(Format::Sl1);
        }
        Format::ALL.into_iter().find(|f| f.extension() == ext)
    }

    pub fn description(self) -> &'static str {
        match self {
            Format::Goo => "Elegoo GOO",
            Format::Ctb => "Chitubox CTB (classic v3)",
            Format::Sl1 => "Prusa SL1",
            Format::Zip => "PNG layer zip",
            Format::Cxdlp => "Creality HALOT CXDLP",
            Format::Cxdlpv4 => "Creality HALOT CXDLPv4",
            Format::Anycubic(m) => m.description(),
        }
    }
}

/// Information about a layer that writers may store.
#[derive(Debug, Clone, Copy)]
pub struct LayerMeta {
    pub index: u32,
    /// Z position of the layer top in mm.
    pub position_z: f32,
    pub exposure: f32,
    pub is_bottom: bool,
    /// Lit area in mm².
    pub area_mm2: f64,
}

/// Global information available to writers.
pub struct JobInfo<'a> {
    pub printer: &'a PrinterProfile,
    pub settings: &'a PrintSettings,
    pub layer_count: u32,
    pub thumbnail: Option<&'a Thumbnail>,
    /// Name of the job (used inside some formats).
    pub job_name: &'a str,
}

pub trait FormatWriter: Send + Sync {
    /// Called once before any layer is written.
    fn begin(&mut self, info: &JobInfo) -> Result<()>;
    /// Encodes one layer. Must be pure; called concurrently.
    fn encode_layer(&self, index: u32, raster: &LayerRaster) -> Result<Vec<u8>>;
    /// Writes an encoded layer. Called sequentially in layer order.
    fn write_layer(&mut self, meta: &LayerMeta, encoded: Vec<u8>) -> Result<()>;
    /// Finalises the file (headers, footers).
    fn finish(&mut self, info: &JobInfo, estimate: &PrintEstimate) -> Result<()>;
}

pub trait WriteSeek: Write + Seek + Send + Sync {}
impl<T: Write + Seek + Send + Sync> WriteSeek for T {}

pub fn create_writer(format: Format, out: Box<dyn WriteSeek>) -> Box<dyn FormatWriter> {
    match format {
        Format::Goo => Box::new(goo::GooWriter::new(out)),
        Format::Ctb => Box::new(ctb::CtbWriter::new(out)),
        Format::Sl1 => Box::new(sl1::Sl1Writer::new(out)),
        Format::Zip => Box::new(pngzip::PngZipWriter::new(out)),
        Format::Cxdlp => Box::new(cxdlp::CxdlpWriter::new(out)),
        Format::Cxdlpv4 => Box::new(cxdlpv4::CxdlpV4Writer::new(out)),
        Format::Anycubic(m) => Box::new(anycubic::AnycubicWriter::new(out, m)),
    }
}

/// Summary of a sliced file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlicedInfo {
    pub format: String,
    pub machine_name: String,
    pub resolution_x: u32,
    pub resolution_y: u32,
    pub size_x: f32,
    pub size_y: f32,
    pub layer_count: u32,
    pub layer_height: f32,
    pub exposure: f32,
    pub bottom_exposure: f32,
    pub bottom_layers: u32,
    pub print_time_s: u32,
    pub volume_ml: f32,
    pub mirror_x: bool,
    pub mirror_y: bool,
    pub anti_aliasing: u32,
}

/// Read access to a sliced file.
pub trait SlicedFile: Send + Sync {
    fn info(&self) -> &SlicedInfo;
    fn layer(&self, index: u32) -> Result<LayerRaster>;
    fn layer_z(&self, index: u32) -> f32;
    fn thumbnail(&self) -> Option<Thumbnail>;
}

/// Opens a sliced file for reading, detecting the format from its extension.
pub fn open(path: &Path) -> Result<Box<dyn SlicedFile>> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default();
    match Format::from_extension(ext) {
        Some(Format::Goo) => Ok(Box::new(goo::GooFile::open(path)?)),
        Some(Format::Ctb) => Ok(Box::new(ctb::CtbFile::open(path)?)),
        Some(Format::Sl1) | Some(Format::Zip) => Ok(Box::new(pngzip::ZipLayers::open(path)?)),
        Some(Format::Cxdlp) => Ok(Box::new(cxdlp::CxdlpFile::open(path)?)),
        Some(Format::Cxdlpv4) => Ok(Box::new(cxdlpv4::CxdlpV4File::open(path)?)),
        Some(Format::Anycubic(m)) => Ok(Box::new(anycubic::AnycubicFile::open(path, m)?)),
        None => Err(FormatError::Unsupported(format!("unknown extension .{ext}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_strings_are_extensions() {
        let mut seen = std::collections::HashSet::new();
        for f in Format::ALL {
            assert!(seen.insert(f), "{f:?} listed twice");
            assert_eq!(Format::from_extension(f.extension()), Some(f));
            assert_eq!(Format::from_extension(&f.extension().to_ascii_uppercase()), Some(f));
            let json = serde_json::to_string(&f).unwrap();
            assert_eq!(json, format!("\"{}\"", f.extension()));
            assert_eq!(serde_json::from_str::<Format>(&json).unwrap(), f);
            assert!(!f.description().is_empty());
        }
        assert_eq!(Format::from_extension(".SL1S"), Some(Format::Sl1));
        assert_eq!(Format::from_extension("pwsz"), None);
        assert!(serde_json::from_str::<Format>("\"nope\"").is_err());
    }
}
