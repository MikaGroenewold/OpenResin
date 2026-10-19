//! Zip archives with one PNG per layer.
//!
//! Writing produces the widely supported "zip + run.gcode" layout (layers named
//! `1.png`, `2.png`, … plus a G-code program with `;key:value` header comments).
//! Reading accepts that layout as well as Prusa SL1 archives.

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Mutex;

use image::{GrayImage, ImageEncoder};
use openresin_core::estimate::PrintEstimate;
use openresin_core::profile::PrintSettings;
use openresin_core::slicer::LayerRaster;

use crate::preview::Thumbnail;
use crate::{FormatError, FormatWriter, JobInfo, LayerMeta, Result, SlicedFile, SlicedInfo, WriteSeek};

pub(crate) fn encode_png(raster: &LayerRaster) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let enc = image::codecs::png::PngEncoder::new_with_quality(
        &mut out,
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::Sub,
    );
    enc.write_image(
        &raster.to_gray(),
        raster.width,
        raster.height,
        image::ExtendedColorType::L8,
    )?;
    Ok(out)
}

pub(crate) fn decode_png(data: &[u8], width: u32, height: u32) -> Result<LayerRaster> {
    let img = image::load_from_memory(data)?.to_luma8();
    if img.width() != width || img.height() != height {
        return Err(FormatError::Invalid(format!(
            "layer image is {}x{}, expected {width}x{height}",
            img.width(),
            img.height()
        )));
    }
    Ok(LayerRaster::from_gray(width, height, img.as_raw()))
}

pub struct PngZipWriter {
    zip: Option<zip::ZipWriter<Box<dyn WriteSeek>>>,
    gcode: String,
    settings: Option<PrintSettings>,
}

impl PngZipWriter {
    pub fn new(out: Box<dyn WriteSeek>) -> Self {
        Self {
            zip: Some(zip::ZipWriter::new(out)),
            gcode: String::new(),
            settings: None,
        }
    }

    fn zip(&mut self) -> &mut zip::ZipWriter<Box<dyn WriteSeek>> {
        self.zip.as_mut().expect("writer already finished")
    }
}

fn stored() -> zip::write::SimpleFileOptions {
    zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)
}

impl FormatWriter for PngZipWriter {
    fn begin(&mut self, info: &JobInfo) -> Result<()> {
        self.settings = Some(info.settings.clone());
        if let Some(t) = info.thumbnail {
            let png = t.fit(400, 300).to_png()?;
            self.zip().start_file("preview.png", stored())?;
            self.zip().write_all(&png)?;
            let png = t.fit(200, 125).to_png()?;
            self.zip().start_file("preview_cropping.png", stored())?;
            self.zip().write_all(&png)?;
        }
        Ok(())
    }

    fn encode_layer(&self, _index: u32, raster: &LayerRaster) -> Result<Vec<u8>> {
        encode_png(raster)
    }

    fn write_layer(&mut self, meta: &LayerMeta, encoded: Vec<u8>) -> Result<()> {
        let name = format!("{}.png", meta.index + 1);
        self.zip().start_file(&name, stored())?;
        self.zip().write_all(&encoded)?;
        // the per-layer motion is written directly (expanding placeholders in
        // `finish` rescanned the whole program once per layer)
        let s = self.settings.as_ref().expect("begin() called");
        let lp = s.params_for_layer(meta.index);
        let z = (meta.index + 1) as f32 * s.layer_height;
        let rest = if s.wait_mode {
            lp.wait_after_retract
        } else {
            lp.light_off_delay
        };
        let block = format!(
            ";LAYER_START:{}\n;currPos:{:.4}\nM6054 \"{name}\";show Image\nG0 Z{:.4} F{};\nG0 Z{:.4} F{};\nG4 P{};\nM106 S{};light on\nG4 P{};\nM106 S0;light off\n;LAYER_END\n",
            meta.index,
            meta.position_z,
            z + lp.lift.total_distance(),
            lp.lift.speed,
            z,
            lp.retract.speed,
            (rest * 1000.0).round() as u32,
            lp.light_pwm,
            (meta.exposure * 1000.0).round() as u32
        );
        self.gcode.push_str(&block);
        Ok(())
    }

    fn finish(&mut self, info: &JobInfo, est: &PrintEstimate) -> Result<()> {
        let p = info.printer;
        let s = info.settings;
        let mut g = String::new();
        let kv = |g: &mut String, k: &str, v: String| g.push_str(&format!(";{k}:{v}\n"));
        kv(&mut g, "fileName", info.job_name.to_string());
        kv(&mut g, "machineType", p.machine_type());
        kv(&mut g, "estimatedPrintTime", format!("{:.0}", est.print_time_s));
        kv(&mut g, "volume", format!("{:.3}", est.volume_ml()));
        kv(&mut g, "resin", s.name.clone());
        kv(&mut g, "weight", format!("{:.3}", est.weight_g));
        kv(&mut g, "price", format!("{:.3}", est.cost));
        kv(&mut g, "layerHeight", format!("{}", s.layer_height));
        kv(&mut g, "normalExposureTime", format!("{}", s.normal.exposure));
        kv(&mut g, "bottomLayExposureTime", format!("{}", s.bottom.exposure));
        kv(&mut g, "bottomLayerExposureTime", format!("{}", s.bottom.exposure));
        kv(&mut g, "normalDropSpeed", format!("{}", s.normal.retract.speed));
        kv(
            &mut g,
            "normalLayerLiftHeight",
            format!("{}", s.normal.lift.total_distance()),
        );
        kv(&mut g, "normalLayerLiftSpeed", format!("{}", s.normal.lift.speed));
        kv(&mut g, "bottomLayCount", format!("{}", s.bottom_layers));
        kv(&mut g, "bottomLayerCount", format!("{}", s.bottom_layers));
        kv(
            &mut g,
            "bottomLayerLiftHeight",
            format!("{}", s.bottom.lift.total_distance()),
        );
        kv(&mut g, "bottomLayerLiftSpeed", format!("{}", s.bottom.lift.speed));
        kv(&mut g, "bottomLightOffTime", format!("{}", s.bottom.light_off_delay));
        kv(&mut g, "lightOffTime", format!("{}", s.normal.light_off_delay));
        kv(&mut g, "bottomPWMLight", format!("{}", s.bottom.light_pwm));
        kv(&mut g, "normalPWMLight", format!("{}", s.normal.light_pwm));
        kv(&mut g, "antiAliasLevel", format!("{}", s.anti_aliasing));
        kv(&mut g, "mirror", format!("{}", p.mirror.is_mirrored() as u8));
        kv(&mut g, "totalLayer", format!("{}", info.layer_count));
        kv(&mut g, "resolutionX", format!("{}", p.resolution_x));
        kv(&mut g, "resolutionY", format!("{}", p.resolution_y));
        kv(&mut g, "machineX", format!("{}", p.size_x));
        kv(&mut g, "machineY", format!("{}", p.size_y));
        kv(&mut g, "machineZ", format!("{}", p.size_z));
        kv(
            &mut g,
            "projectType",
            if p.mirror.is_mirrored() {
                "mirror_LCD".into()
            } else {
                "normal".into()
            },
        );
        g.push_str(";START_GCODE_BEGIN\nG21;\nG90;\nM106 S0;\nG28 Z0;\n;START_GCODE_END\n");
        g.push_str(&std::mem::take(&mut self.gcode));
        g.push_str(&format!(
            ";END_GCODE_BEGIN\nM106 S0;\nG1 Z{} F25;\nM18;\n;END_GCODE_END\n",
            p.size_z
        ));
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        self.zip().start_file("run.gcode", opts)?;
        self.zip().write_all(g.as_bytes())?;
        let zip = self.zip.take().expect("writer already finished");
        let mut out = zip.finish()?;
        out.flush()?;
        Ok(())
    }
}

/// Reader for SL1 and PNG zip archives.
pub struct ZipLayers {
    info: SlicedInfo,
    names: Vec<String>,
    z: Vec<f32>,
    thumbnail: Option<String>,
    zip: Mutex<zip::ZipArchive<File>>,
}

fn parse_kv(text: &str, sep: char) -> std::collections::HashMap<String, String> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim().trim_start_matches(';');
            let (k, v) = l.split_once(sep)?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

impl ZipLayers {
    pub fn open(path: &Path) -> Result<Self> {
        let mut zip = zip::ZipArchive::new(File::open(path)?)?;
        let names: Vec<String> = zip.file_names().map(str::to_string).collect();
        let read = |zip: &mut zip::ZipArchive<File>, n: &str| -> Option<String> {
            let mut s = String::new();
            zip.by_name(n).ok()?.read_to_string(&mut s).ok()?;
            Some(s)
        };
        let is_layer = |n: &str| {
            n.to_ascii_lowercase().ends_with(".png")
                && !n.contains('/')
                && !n.to_ascii_lowercase().starts_with("preview")
        };
        let mut layers: Vec<String> = names.iter().filter(|n| is_layer(n)).cloned().collect();
        let (kv, format, thumb) = if let Some(cfg) = read(&mut zip, "config.ini") {
            let mut kv = parse_kv(&cfg, '=');
            if let Some(ps) = read(&mut zip, "prusaslicer.ini") {
                kv.extend(parse_kv(&ps, '='));
            }
            layers.sort();
            let thumb = names.iter().filter(|n| n.starts_with("thumbnail/")).max().cloned();
            (kv, "sl1", thumb)
        } else if let Some(g) = read(&mut zip, "run.gcode") {
            layers.sort_by_key(|n| n.trim_end_matches(".png").parse::<u32>().unwrap_or(u32::MAX));
            (
                parse_kv(&g, ':'),
                "zip",
                names.iter().find(|n| *n == "preview.png").cloned(),
            )
        } else {
            return Err(FormatError::Invalid(
                "zip contains neither config.ini nor run.gcode".into(),
            ));
        };
        let num = |keys: &[&str]| -> f32 { keys.iter().find_map(|k| kv.get(*k)?.parse().ok()).unwrap_or(0.0) };
        let mut info = SlicedInfo {
            format: format.into(),
            machine_name: kv
                .get("machineType")
                .or_else(|| kv.get("printerModel"))
                .cloned()
                .unwrap_or_default(),
            resolution_x: num(&["resolutionX", "display_pixels_x"]) as u32,
            resolution_y: num(&["resolutionY", "display_pixels_y"]) as u32,
            size_x: num(&["machineX", "display_width"]),
            size_y: num(&["machineY", "display_height"]),
            layer_count: layers.len() as u32,
            layer_height: num(&["layerHeight"]),
            exposure: num(&["normalExposureTime", "expTime"]),
            bottom_exposure: num(&["bottomLayerExposureTime", "expTimeFirst"]),
            bottom_layers: num(&["bottomLayerCount", "numFade"]) as u32,
            print_time_s: num(&["estimatedPrintTime", "printTime"]) as u32,
            volume_ml: num(&["volume", "usedMaterial"]),
            mirror_x: num(&["mirror", "display_mirror_x"]) != 0.0,
            mirror_y: num(&["display_mirror_y"]) != 0.0,
            anti_aliasing: num(&["antiAliasLevel"]).max(1.0) as u32,
        };
        if (info.resolution_x == 0 || info.resolution_y == 0) && !layers.is_empty() {
            let mut data = Vec::new();
            zip.by_name(&layers[0])?.read_to_end(&mut data)?;
            let img: GrayImage = image::load_from_memory(&data)?.to_luma8();
            info.resolution_x = img.width();
            info.resolution_y = img.height();
        }
        let z = (0..layers.len()).map(|i| (i + 1) as f32 * info.layer_height).collect();
        Ok(Self {
            info,
            names: layers,
            z,
            thumbnail: thumb,
            zip: Mutex::new(zip),
        })
    }
}

impl SlicedFile for ZipLayers {
    fn info(&self) -> &SlicedInfo {
        &self.info
    }

    fn layer(&self, index: u32) -> Result<LayerRaster> {
        let name = self
            .names
            .get(index as usize)
            .ok_or_else(|| FormatError::Invalid(format!("layer {index} out of range")))?;
        let mut data = Vec::new();
        self.zip.lock().unwrap().by_name(name)?.read_to_end(&mut data)?;
        decode_png(&data, self.info.resolution_x, self.info.resolution_y)
    }

    fn layer_z(&self, index: u32) -> f32 {
        self.z.get(index as usize).copied().unwrap_or(0.0)
    }

    fn thumbnail(&self) -> Option<Thumbnail> {
        let name = self.thumbnail.as_ref()?;
        let mut data = Vec::new();
        self.zip.lock().ok()?.by_name(name).ok()?.read_to_end(&mut data).ok()?;
        Thumbnail::from_png(&data).ok()
    }
}
