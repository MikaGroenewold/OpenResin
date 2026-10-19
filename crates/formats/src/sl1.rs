//! Prusa SL1 / SL1S archive writer.

use std::io::Write;

use openresin_core::estimate::PrintEstimate;
use openresin_core::slicer::LayerRaster;

use crate::pngzip::encode_png;
use crate::{FormatWriter, JobInfo, LayerMeta, Result, WriteSeek};

pub struct Sl1Writer {
    zip: Option<zip::ZipWriter<Box<dyn WriteSeek>>>,
    job_dir: String,
}

impl Sl1Writer {
    pub fn new(out: Box<dyn WriteSeek>) -> Self {
        Self {
            zip: Some(zip::ZipWriter::new(out)),
            job_dir: "job".into(),
        }
    }

    fn zip(&mut self) -> &mut zip::ZipWriter<Box<dyn WriteSeek>> {
        self.zip.as_mut().expect("writer already finished")
    }
}

fn stored() -> zip::write::SimpleFileOptions {
    zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)
}

fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(40)
        .collect();
    if s.is_empty() {
        "job".into()
    } else {
        s
    }
}

impl FormatWriter for Sl1Writer {
    fn begin(&mut self, info: &JobInfo) -> Result<()> {
        self.job_dir = sanitize(info.job_name);
        if let Some(t) = info.thumbnail {
            for (w, h) in [(400, 400), (800, 480)] {
                let png = t.fit(w, h).to_png()?;
                self.zip()
                    .start_file(format!("thumbnail/thumbnail{w}x{h}.png"), stored())?;
                self.zip().write_all(&png)?;
            }
        }
        Ok(())
    }

    fn encode_layer(&self, _index: u32, raster: &LayerRaster) -> Result<Vec<u8>> {
        encode_png(raster)
    }

    fn write_layer(&mut self, meta: &LayerMeta, encoded: Vec<u8>) -> Result<()> {
        let name = format!("{}{:05}.png", self.job_dir, meta.index);
        self.zip().start_file(name, stored())?;
        self.zip().write_all(&encoded)?;
        Ok(())
    }

    fn finish(&mut self, info: &JobInfo, est: &PrintEstimate) -> Result<()> {
        let p = info.printer;
        let s = info.settings;
        let fast = info.layer_count;
        let config = format!(
            "action = print\njobDir = {job}\nexpTime = {exp}\nexpTimeFirst = {bexp}\nfileCreationTimestamp = {ts}\nlayerHeight = {lh}\nmaterialName = {mat}\nnumFade = {fade}\nnumFast = {fast}\nnumSlow = 0\nprintProfile = {mat}\nprintTime = {time:.0}\nprinterModel = {model}\nprinterProfile = {model}\nprinterVariant = default\nprusaSlicerVersion = OpenResin {ver}\nusedMaterial = {ml:.3}\n",
            job = self.job_dir,
            exp = s.normal.exposure,
            bexp = s.bottom.exposure,
            ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            lh = s.layer_height,
            mat = s.name,
            fade = s.bottom_layers,
            fast = fast,
            time = est.print_time_s,
            model = p.machine_type(),
            ver = env!("CARGO_PKG_VERSION"),
            ml = est.volume_ml(),
        );
        let printer_ini = format!(
            "display_width = {}\ndisplay_height = {}\ndisplay_pixels_x = {}\ndisplay_pixels_y = {}\ndisplay_orientation = landscape\ndisplay_mirror_x = {}\ndisplay_mirror_y = {}\nmax_print_height = {}\nexposure_time = {}\ninitial_exposure_time = {}\nlayer_height = {}\nfaded_layers = {}\n",
            p.size_x,
            p.size_y,
            p.resolution_x,
            p.resolution_y,
            (p.mirror == openresin_core::profile::MirrorMode::X) as u8,
            matches!(p.mirror, openresin_core::profile::MirrorMode::Y | openresin_core::profile::MirrorMode::Lcd) as u8,
            p.size_z,
            s.normal.exposure,
            s.bottom.exposure,
            s.layer_height,
            s.bottom_layers
        );
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        self.zip().start_file("config.ini", opts)?;
        self.zip().write_all(config.as_bytes())?;
        self.zip().start_file("prusaslicer.ini", opts)?;
        self.zip().write_all(printer_ini.as_bytes())?;
        let zip = self.zip.take().expect("writer already finished");
        let mut out = zip.finish()?;
        out.flush()?;
        Ok(())
    }
}
