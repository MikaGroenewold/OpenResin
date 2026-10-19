//! Slicing pipeline: rasterize layers in parallel and stream them into a file.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use openresin_core::estimate::{estimate, PrintEstimate};
use openresin_core::profile::{PrintSettings, PrinterProfile};
use openresin_core::slicer::Slicer;
use openresin_core::Mesh;
use rayon::prelude::*;
use serde::Serialize;

use crate::preview::Thumbnail;
use crate::{create_writer, Format, FormatError, JobInfo, LayerMeta, Result};

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ExportProgress {
    pub done: u32,
    pub total: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExportResult {
    pub path: PathBuf,
    pub format: Format,
    pub estimate: PrintEstimate,
    /// Lit area of every layer in mm².
    pub layer_areas: Vec<f64>,
    pub file_size: u64,
    pub duration: Duration,
}

pub struct ExportJob<'a> {
    pub meshes: &'a [&'a Mesh],
    /// Volumes subtracted from `meshes` (hollowing, drain holes, boolean cuts).
    pub negatives: &'a [&'a Mesh],
    pub printer: &'a PrinterProfile,
    pub settings: &'a PrintSettings,
    pub format: Format,
    pub thumbnail: Option<&'a Thumbnail>,
    pub job_name: &'a str,
}

/// Slices `job.meshes` and writes the result to `path`.
///
/// The file is written to `<path>.part` and renamed on success, so a failed or
/// cancelled export never leaves a truncated file behind.
pub fn slice_to_file(
    job: &ExportJob,
    path: &Path,
    progress: &(dyn Fn(ExportProgress) + Sync),
    cancel: &AtomicBool,
) -> Result<ExportResult> {
    let started = Instant::now();
    job.settings.validate().map_err(FormatError::Invalid)?;
    let s = job.settings;
    let p = job.printer;
    let raster = p.raster_options(s.anti_aliasing);
    let slicer = if job.negatives.is_empty() {
        Slicer::new(job.meshes, s.layer_height, raster)
    } else {
        Slicer::with_negatives(job.meshes, job.negatives, s.layer_height, raster)
    };
    let n = slicer.layer_count() as u32;
    if n == 0 {
        return Err(FormatError::Invalid("nothing to slice: the plate is empty".into()));
    }
    let (px, py) = slicer.options().pixel_size();
    let pixel_area = (px * py) as f64;

    let tmp = path.with_extension(format!(
        "{}.part",
        path.extension().and_then(|e| e.to_str()).unwrap_or("out")
    ));
    let file = File::create(&tmp)?;
    let mut writer = create_writer(job.format, Box::new(BufWriter::with_capacity(1 << 20, file)));
    let info = JobInfo {
        printer: p,
        settings: s,
        layer_count: n,
        thumbnail: job.thumbnail,
        job_name: job.job_name,
    };

    let result = (|| -> Result<Vec<f64>> {
        writer.begin(&info)?;
        let mut areas = Vec::with_capacity(n as usize);
        let batch = (rayon::current_num_threads() * 2).max(4) as u32;
        let mut start = 0u32;
        while start < n {
            if cancel.load(Ordering::Relaxed) {
                return Err(FormatError::Cancelled);
            }
            let end = (start + batch).min(n);
            let w = &writer;
            let encoded: Vec<Result<(Vec<u8>, f64)>> = (start..end)
                .into_par_iter()
                .map(|i| {
                    let r = slicer.rasterize(i as usize);
                    let area = r.lit_area_px() * pixel_area;
                    Ok((w.encode_layer(i, &r)?, area))
                })
                .collect();
            for (k, e) in encoded.into_iter().enumerate() {
                let (data, area) = e?;
                let i = start + k as u32;
                let meta = LayerMeta {
                    index: i,
                    position_z: slicer.layer_position_z(i as usize),
                    exposure: s.exposure_for_layer(i),
                    is_bottom: i < s.bottom_layers,
                    area_mm2: area,
                };
                writer.write_layer(&meta, data)?;
                areas.push(area);
            }
            start = end;
            progress(ExportProgress { done: end, total: n });
        }
        Ok(areas)
    })();

    let areas = match result {
        Ok(a) => a,
        Err(e) => {
            drop(writer);
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    };
    let est = estimate(s, &areas);
    if let Err(e) = writer.finish(&info, &est) {
        drop(writer);
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    drop(writer);
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    std::fs::rename(&tmp, path)?;
    let file_size = std::fs::metadata(path)?.len();
    Ok(ExportResult {
        path: path.to_path_buf(),
        format: job.format,
        estimate: est,
        layer_areas: areas,
        file_size,
        duration: started.elapsed(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anycubic::{self, LayerCodec};
    use crate::open;
    use openresin_core::primitives::{cuboid, frustum};
    use openresin_core::profile::MirrorMode;
    use openresin_core::Vec3;

    fn printer() -> PrinterProfile {
        PrinterProfile {
            id: "test".into(),
            vendor: "Test".into(),
            name: "Test 1K".into(),
            resolution_x: 400,
            resolution_y: 250,
            size_x: 80.0,
            size_y: 50.0,
            size_z: 100.0,
            mirror: MirrorMode::Lcd,
            formats: vec!["goo".into()],
            native_formats: Vec::new(),
            network: None,
            machine_type: Some("Test Printer".into()),
        }
    }

    #[test]
    fn all_formats_roundtrip_through_reader() {
        let dir = std::env::temp_dir().join(format!("openresin-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = cuboid(Vec3::new(-10.0, 5.0, 2.5), Vec3::new(10.0, 6.0, 5.0));
        let b = frustum(Vec3::new(15.0, -5.0, 0.0), Vec3::new(15.0, -5.0, 4.0), 5.0, 2.0, 32);
        let meshes = [&a, &b];
        let p = printer();
        let s = PrintSettings {
            layer_height: 0.1,
            ..Default::default()
        };
        let thumb = crate::preview::render_meshes(&meshes, 64, 64);
        for format in Format::ALL {
            let path = dir.join(format!("test.{}", format.extension()));
            let job = ExportJob {
                meshes: &meshes,
                negatives: &[],
                printer: &p,
                settings: &s,
                format,
                thumbnail: Some(&thumb),
                job_name: "test job",
            };
            let res = slice_to_file(&job, &path, &|_| {}, &AtomicBool::new(false)).unwrap();
            assert_eq!(res.layer_areas.len(), 50, "{format:?}");
            let f = open(&path).unwrap_or_else(|e| panic!("{format:?}: {e}"));
            assert_eq!(f.info().layer_count, 50, "{format:?}");
            assert_eq!((f.info().resolution_x, f.info().resolution_y), (400, 250), "{format:?}");
            // compare a layer with a fresh rasterization, quantised like the format stores it
            let quantize = |v: u8| match format {
                Format::Anycubic(m) => match m.codec() {
                    LayerCodec::Pw0 => anycubic::quantize_pw0(v),
                    LayerCodec::Pws => anycubic::quantize_planes(v, s.anti_aliasing),
                },
                _ => v,
            };
            let slicer = Slicer::new(&meshes, 0.1, p.raster_options(s.anti_aliasing));
            for i in [0u32, 10, 49] {
                let expect = slicer.rasterize(i as usize);
                let got = f.layer(i).unwrap();
                assert_eq!((got.width, got.height), (400, 250), "{format:?}");
                let diff: i64 = expect
                    .to_gray()
                    .iter()
                    .zip(got.to_gray())
                    .map(|(&a, b)| (quantize(a) as i64 - b as i64).abs())
                    .max()
                    .unwrap();
                // CTB and CXDLPv4 store 7 bits per pixel
                assert!(diff <= 1, "{format:?} layer {i}: max diff {diff}");
                assert!(!got.is_blank(), "{format:?} layer {i} is blank");
                assert!(
                    (f.layer_z(i) - (i + 1) as f32 * 0.1).abs() < 1e-3,
                    "{format:?} layer {i} z"
                );
            }
            assert!(f.thumbnail().is_some(), "{format:?} thumbnail");
            assert!(!path.with_extension(format!("{}.part", format.extension())).exists());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancel_removes_partial_file() {
        let dir = std::env::temp_dir().join(format!("openresin-cancel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = cuboid(Vec3::new(0.0, 0.0, 5.0), Vec3::splat(10.0));
        let p = printer();
        let s = PrintSettings::default();
        let job = ExportJob {
            meshes: &[&a],
            negatives: &[],
            printer: &p,
            settings: &s,
            format: Format::Goo,
            thumbnail: None,
            job_name: "x",
        };
        let path = dir.join("c.goo");
        let err = slice_to_file(&job, &path, &|_| {}, &AtomicBool::new(true)).unwrap_err();
        assert!(matches!(err, FormatError::Cancelled));
        assert!(!path.exists());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
