//! End-to-end tests of the `openresin` binary: every test runs the real
//! executable on generated model files and checks the produced printer files by
//! reading them back with `openresin-formats`.
//!
//! Network commands are only exercised against the loopback interface (closed
//! ports or a tiny MJPEG server started by the test); nothing here sends a
//! packet to the LAN.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use openresin_core::io::write_binary_stl;
use openresin_core::primitives::cuboid;
use openresin_core::profile::{builtin_printers, PrintSettings};
use openresin_core::slicer::LayerRaster;
use openresin_core::{Mesh, Vec3};
use openresin_formats::{SlicedFile, SlicedInfo};

// ------------------------------------------------------------------ helpers

/// A unique scratch directory under the system temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "openresin-cli-test-{}-{}-{name}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    fn join(&self, p: &str) -> PathBuf {
        self.0.join(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_openresin"));
    // the camera test talks to a loopback HTTP server; never route that through a proxy
    c.env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost");
    c.env_remove("HTTP_PROXY").env_remove("http_proxy");
    c.env_remove("HTTPS_PROXY").env_remove("https_proxy");
    c.env_remove("ALL_PROXY").env_remove("all_proxy");
    c.stdin(Stdio::null());
    c
}

fn run<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    bin().args(args).output().expect("failed to run openresin")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[track_caller]
fn assert_ok(o: &Output) {
    assert!(
        o.status.success(),
        "openresin failed ({:?})\nstdout:\n{}\nstderr:\n{}",
        o.status,
        stdout(o),
        stderr(o)
    );
}

/// Asserts a failed run whose stderr contains every `needle` (case-insensitive).
#[track_caller]
fn assert_fails_with(o: &Output, needles: &[&str]) {
    assert!(
        !o.status.success(),
        "openresin unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        stdout(o),
        stderr(o)
    );
    let err = stderr(o).to_lowercase();
    for n in needles {
        assert!(
            err.contains(&n.to_lowercase()),
            "stderr does not mention {n:?}:\n{}",
            stderr(o)
        );
    }
    assert!(!err.contains("panicked"), "openresin panicked:\n{}", stderr(o));
}

fn p(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn write_stl(path: &Path, mesh: &Mesh) {
    let f = std::fs::File::create(path).unwrap();
    write_binary_stl(mesh, std::io::BufWriter::new(f)).unwrap();
}

/// Axis aligned box `size` standing on the plate.
fn box_stl(dir: &TempDir, name: &str, size: Vec3) -> PathBuf {
    let path = dir.join(name);
    write_stl(&path, &cuboid(Vec3::new(0.0, 0.0, size.z / 2.0), size));
    path
}

/// 10 mm cube as a hand-written Wavefront OBJ (quads, 1-based indices).
fn cube_obj(dir: &TempDir, name: &str) -> PathBuf {
    let path = dir.join(name);
    let obj = "# 10 mm cube\n\
        v 0 0 0\nv 10 0 0\nv 10 10 0\nv 0 10 0\n\
        v 0 0 10\nv 10 0 10\nv 10 10 10\nv 0 10 10\n\
        f 1 4 3 2\nf 5 6 7 8\nf 1 2 6 5\nf 2 3 7 6\nf 3 4 8 7\nf 4 1 5 8\n";
    std::fs::write(&path, obj).unwrap();
    path
}

/// 3MF with one 10 mm cube object placed twice by the build section.
fn two_cubes_3mf(dir: &TempDir, name: &str) -> PathBuf {
    let path = dir.join(name);
    let model = r#"<?xml version="1.0" encoding="UTF-8"?>
<model unit="millimeter" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">
  <resources>
    <object id="1" name="cube" type="model">
      <mesh>
        <vertices>
          <vertex x="0" y="0" z="0"/><vertex x="10" y="0" z="0"/><vertex x="10" y="10" z="0"/><vertex x="0" y="10" z="0"/>
          <vertex x="0" y="0" z="10"/><vertex x="10" y="0" z="10"/><vertex x="10" y="10" z="10"/><vertex x="0" y="10" z="10"/>
        </vertices>
        <triangles>
          <triangle v1="0" v2="3" v3="2"/><triangle v1="0" v2="2" v3="1"/>
          <triangle v1="4" v2="5" v3="6"/><triangle v1="4" v2="6" v3="7"/>
          <triangle v1="0" v2="1" v3="5"/><triangle v1="0" v2="5" v3="4"/>
          <triangle v1="1" v2="2" v3="6"/><triangle v1="1" v2="6" v3="5"/>
          <triangle v1="2" v2="3" v3="7"/><triangle v1="2" v2="7" v3="6"/>
          <triangle v1="3" v2="0" v3="4"/><triangle v1="3" v2="4" v3="7"/>
        </triangles>
      </mesh>
    </object>
  </resources>
  <build>
    <item objectid="1"/>
    <item objectid="1" transform="1 0 0 0 1 0 0 0 1 30 0 0"/>
  </build>
</model>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Target="/3D/3dmodel.model" Id="rel0" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/>
</Relationships>"#;
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("_rels/.rels", opts).unwrap();
    zip.write_all(rels.as_bytes()).unwrap();
    zip.start_file("3D/3dmodel.model", opts).unwrap();
    zip.write_all(model.as_bytes()).unwrap();
    zip.finish().unwrap();
    path
}

fn open_sliced(path: &Path) -> Box<dyn SlicedFile> {
    assert!(path.is_file(), "{} was not written", path.display());
    assert!(
        std::fs::metadata(path).unwrap().len() > 0,
        "{} is empty",
        path.display()
    );
    openresin_formats::open(path).unwrap_or_else(|e| panic!("cannot read back {}: {e}", path.display()))
}

fn layer(f: &dyn SlicedFile, i: u32) -> LayerRaster {
    f.layer(i).unwrap_or_else(|e| panic!("layer {i}: {e}"))
}

/// Pixel size (mm) of a built-in printer.
fn pixel_mm(printer_id: &str) -> (f32, f32) {
    let p = builtin_printers().into_iter().find(|p| p.id == printer_id).unwrap();
    (p.size_x / p.resolution_x as f32, p.size_y / p.resolution_y as f32)
}

/// Expected lit pixels of an axis aligned `w` x `d` mm rectangle.
fn rect_px(printer_id: &str, w: f32, d: f32) -> f64 {
    let (px, py) = pixel_mm(printer_id);
    (w / px) as f64 * (d / py) as f64
}

#[track_caller]
fn assert_close(actual: f64, expected: f64, rel: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= expected.abs() * rel,
        "{what}: got {actual}, expected {expected} ± {:.1}%",
        rel * 100.0
    );
}

#[track_caller]
fn assert_info(info: &SlicedInfo, printer_id: &str, layers: u32, layer_height: f32) {
    let pr = builtin_printers().into_iter().find(|p| p.id == printer_id).unwrap();
    assert_eq!(
        (info.resolution_x, info.resolution_y),
        (pr.resolution_x, pr.resolution_y),
        "{info:?}"
    );
    assert_eq!(info.layer_count, layers, "{info:?}");
    assert!((info.layer_height - layer_height).abs() < 1e-4, "{info:?}");
}

// ------------------------------------------------------------------ general

#[test]
fn version_and_help() {
    let o = run(["--version"]);
    assert_ok(&o);
    assert!(stdout(&o).contains(env!("CARGO_PKG_VERSION")), "{}", stdout(&o));

    let o = run(["--help"]);
    assert_ok(&o);
    let help = stdout(&o);
    for cmd in [
        "printers", "slice", "info", "layer", "discover", "status", "files", "download", "upload", "print", "pause",
        "resume", "stop", "camera",
    ] {
        assert!(help.contains(cmd), "--help does not list {cmd}:\n{help}");
    }

    let o = run(["slice", "--help"]);
    assert_ok(&o);
    for flag in [
        "--printer",
        "--output",
        "--format",
        "--layer-height",
        "--exposure",
        "--supports",
        "--auto-orient",
    ] {
        assert!(stdout(&o).contains(flag), "slice --help does not list {flag}");
    }

    // no subcommand / unknown subcommand: usage error, no panic
    let o = run::<[&str; 0], &str>([]);
    assert_fails_with(&o, &["usage"]);
    let o = run(["frobnicate"]);
    assert_fails_with(&o, &["frobnicate"]);
}

// ------------------------------------------------------------------ printers

#[test]
fn printers_lists_the_whole_database() {
    let o = run(["printers"]);
    assert_ok(&o);
    let out = stdout(&o);
    let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
    let db = builtin_printers();
    assert_eq!(lines.len(), db.len(), "one line per printer");
    let ids: Vec<&str> = lines.iter().map(|l| l.split_whitespace().next().unwrap()).collect();
    for pr in &db {
        assert!(ids.contains(&pr.id.as_str()), "{} missing", pr.id);
    }
    let saturn = lines
        .iter()
        .find(|l| l.split_whitespace().next() == Some("elegoo-saturn-4-ultra"))
        .unwrap();
    assert!(saturn.contains("11520x5120"), "{saturn}");
    assert!(saturn.contains("goo,ctb"), "{saturn}");
    assert!(saturn.contains("Sdcp"), "{saturn}");
    assert!(
        lines[0].starts_with("generic-4k"),
        "generic printer first: {}",
        lines[0]
    );
}

#[test]
fn printers_filter_is_case_insensitive() {
    let o = run(["printers", "SATURN 4"]);
    assert_ok(&o);
    let got: Vec<String> = stdout(&o)
        .lines()
        .filter_map(|l| l.split_whitespace().next().map(str::to_string))
        .collect();
    let want: Vec<String> = builtin_printers()
        .into_iter()
        .filter(|p| p.display_name().to_lowercase().contains("saturn 4"))
        .map(|p| p.id)
        .collect();
    assert!(!want.is_empty());
    assert_eq!(got, want);

    let o = run(["printers", "no-such-printer-xyz"]);
    assert_ok(&o);
    assert!(stdout(&o).trim().is_empty(), "{}", stdout(&o));
}

// ------------------------------------------------------------------ slice: formats and settings

#[test]
fn slice_goo_with_explicit_settings() {
    let d = TempDir::new("goo");
    let model = box_stl(&d, "cube.stl", Vec3::splat(10.0));
    let out = d.join("cube.goo");
    let printer = "elegoo-mars-4-dlp";
    let o = run([
        "slice",
        p(&model),
        "-p",
        printer,
        "-o",
        p(&out),
        "--layer-height",
        "0.1",
        "--exposure",
        "3.2",
        "--bottom-exposure",
        "25",
        "--bottom-layers",
        "3",
        "--aa",
        "1",
    ]);
    assert_ok(&o);
    let summary = stdout(&o);
    assert!(summary.contains("100 layers"), "{summary}");
    assert!(summary.contains("estimated print time"), "{summary}");
    assert!(
        !stderr(&o).contains("warning"),
        "goo is native for this printer: {}",
        stderr(&o)
    );

    let f = open_sliced(&out);
    let info = f.info();
    assert_info(info, printer, 100, 0.1);
    assert!((info.exposure - 3.2).abs() < 1e-4, "{info:?}");
    assert!((info.bottom_exposure - 25.0).abs() < 1e-4, "{info:?}");
    assert_eq!(info.bottom_layers, 3);
    assert!(info.print_time_s > 0, "{info:?}");
    assert!(info.volume_ml > 0.0, "{info:?}");
    // 10 mm cube = 1 ml
    assert_close(info.volume_ml as f64, 1.0, 0.05, "resin volume");
    assert!(f.thumbnail().is_some(), "goo carries a preview");

    let expected = rect_px(printer, 10.0, 10.0);
    for i in [0, 1, 50, 99] {
        let l = layer(&*f, i);
        assert_eq!((l.width, l.height), (2560, 1440));
        assert_close(l.lit_area_px(), expected, 0.03, &format!("lit area of layer {i}"));
        // anti-aliasing off: only fully lit or dark pixels
        assert!(
            l.runs.iter().all(|r| r.value == 0 || r.value == 255),
            "layer {i} has grey pixels"
        );
    }
    assert!((f.layer_z(0) - 0.1).abs() < 1e-4 && (f.layer_z(99) - 10.0).abs() < 1e-3);
    // the cube is centred on the plate
    let (x0, y0, x1, y1) = layer(&*f, 50).bbox.unwrap();
    let (cx, cy) = ((x0 + x1) as f32 / 2.0, (y0 + y1) as f32 / 2.0);
    assert!(
        (cx - 1280.0).abs() <= 2.0 && (cy - 720.0).abs() <= 2.0,
        "bbox {:?}",
        (x0, y0, x1, y1)
    );
}

#[test]
fn slice_defaults_to_the_printers_format_and_model_name() {
    let d = TempDir::new("default-out");
    let model = box_stl(&d, "part.stl", Vec3::new(8.0, 12.0, 4.0));
    // elegoo-mars lists only ctb; no -o / -f: writes ./part.ctb
    let o = bin()
        .current_dir(&d.0)
        .args(["slice", "part.stl", "-p", "elegoo-mars", "--layer-height", "0.05"])
        .output()
        .unwrap();
    assert_ok(&o);
    let out = d.join("part.ctb");
    let f = open_sliced(&out);
    assert_info(f.info(), "elegoo-mars", 80, 0.05);
    let ps = PrintSettings::default();
    assert!(
        (f.info().exposure - ps.normal.exposure).abs() < 1e-4,
        "default exposure"
    );
    assert!((f.info().bottom_exposure - ps.bottom.exposure).abs() < 1e-4);
    assert_eq!(f.info().bottom_layers, ps.bottom_layers);
    assert_eq!(f.info().anti_aliasing, ps.anti_aliasing as u32);
    let l = layer(&*f, 40);
    // default anti-aliasing smooths the edges, the area stays the same
    assert_close(l.lit_area_px(), rect_px("elegoo-mars", 8.0, 12.0), 0.03, "ctb lit area");
    drop(f);
    assert!(model.exists());
}

#[test]
fn slice_sl1_warns_about_non_native_format() {
    let d = TempDir::new("sl1");
    let model = box_stl(&d, "cube.stl", Vec3::splat(10.0));
    let out = d.join("cube.sl1");
    let printer = "sparkmaker-original"; // lists zip only
    let o = run([
        "slice",
        p(&model),
        "-p",
        printer,
        "-f",
        "sl1",
        "-o",
        p(&out),
        "--layer-height",
        "0.1",
        "--exposure",
        "6",
    ]);
    assert_ok(&o);
    assert!(
        stderr(&o).contains("warning: sl1 is not a format listed"),
        "expected a format warning: {}",
        stderr(&o)
    );
    let f = open_sliced(&out);
    assert_info(f.info(), printer, 100, 0.1);
    assert!((f.info().exposure - 6.0).abs() < 1e-4, "{:?}", f.info());
    for i in [0, 99] {
        assert_close(
            layer(&*f, i).lit_area_px(),
            rect_px(printer, 10.0, 10.0),
            0.05,
            "sl1 lit area",
        );
    }
}

#[test]
fn slice_pwmx_inferred_from_extension() {
    let d = TempDir::new("pwmx");
    let model = box_stl(&d, "cube.stl", Vec3::new(20.0, 10.0, 5.0));
    let out = d.join("job.pwmx");
    let printer = "anycubic-photon-mono-x";
    let o = run([
        "slice",
        p(&model),
        "-p",
        printer,
        "-o",
        p(&out),
        "--layer-height",
        "0.05",
        "--exposure",
        "2",
    ]);
    assert_ok(&o);
    let f = open_sliced(&out);
    assert_info(f.info(), printer, 100, 0.05);
    assert!((f.info().exposure - 2.0).abs() < 1e-4, "{:?}", f.info());
    for i in [0, 50, 99] {
        assert_close(
            layer(&*f, i).lit_area_px(),
            rect_px(printer, 20.0, 10.0),
            0.03,
            "pwmx lit area",
        );
    }
    assert!((f.layer_z(99) - 5.0).abs() < 1e-3, "z of last layer {}", f.layer_z(99));
}

#[test]
fn slice_png_zip_reads_back() {
    let d = TempDir::new("zip");
    let model = box_stl(&d, "cube.stl", Vec3::splat(10.0));
    let out = d.join("layers.zip");
    let printer = "sparkmaker-original";
    let o = run([
        "slice",
        p(&model),
        "-p",
        printer,
        "-f",
        "zip",
        "-o",
        p(&out),
        "--layer-height",
        "0.1",
    ]);
    assert_ok(&o);
    assert!(!stderr(&o).contains("warning"), "{}", stderr(&o));
    let f = open_sliced(&out);
    assert_info(f.info(), printer, 100, 0.1);
    assert_close(
        layer(&*f, 10).lit_area_px(),
        rect_px(printer, 10.0, 10.0),
        0.05,
        "zip lit area",
    );

    // the archive really holds one PNG per layer
    let mut z = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
    let pngs = (0..z.len())
        .filter(|&i| z.by_index(i).unwrap().name().to_ascii_lowercase().ends_with(".png"))
        .count();
    assert!(pngs >= 100, "only {pngs} png entries");
}

#[test]
fn slice_settings_file_and_flag_overrides() {
    let d = TempDir::new("settings");
    let model = box_stl(&d, "cube.stl", Vec3::splat(10.0));
    let mut s = PrintSettings {
        layer_height: 0.2,
        bottom_layers: 2,
        anti_aliasing: 2,
        ..Default::default()
    };
    s.normal.exposure = 4.0;
    s.bottom.exposure = 40.0;
    let settings = d.join("settings.json");
    std::fs::write(&settings, serde_json::to_string_pretty(&s).unwrap()).unwrap();
    let out = d.join("cube.goo");
    let o = run([
        "slice",
        p(&model),
        "-p",
        "elegoo-mars-4-dlp",
        "-o",
        p(&out),
        "--settings",
        p(&settings),
        "--exposure",
        "5",
    ]);
    assert_ok(&o);
    let f = open_sliced(&out);
    let info = f.info();
    assert_info(info, "elegoo-mars-4-dlp", 50, 0.2);
    assert!((info.exposure - 5.0).abs() < 1e-4, "flag overrides file: {info:?}");
    assert!((info.bottom_exposure - 40.0).abs() < 1e-4, "file value kept: {info:?}");
    assert_eq!(info.bottom_layers, 2);
    assert_eq!(info.anti_aliasing, 2);
}

// ------------------------------------------------------------------ slice: models, supports, orientation

#[test]
fn slice_with_supports_adds_structures_below_the_model() {
    let d = TempDir::new("supports");
    let model = box_stl(&d, "cube.stl", Vec3::splat(10.0));
    let printer = "elegoo-mars-4-dlp";
    let plain = d.join("plain.goo");
    let supported = d.join("supported.goo");
    let lifted = d.join("lifted.goo");
    let common = ["-p", printer, "--layer-height", "0.1", "--aa", "1"];

    let o = run(["slice", p(&model), "-o", p(&plain)].iter().chain(&common));
    assert_ok(&o);
    let o = run(["slice", p(&model), "-o", p(&supported), "--supports"]
        .iter()
        .chain(&common));
    assert_ok(&o);
    let msg = stderr(&o);
    let points: usize = msg
        .lines()
        .find_map(|l| l.strip_prefix("cube: ")?.strip_suffix(" support points")?.parse().ok())
        .unwrap_or_else(|| panic!("no support point count in {msg}"));
    assert!(points >= 4, "a 10 mm flat bottom needs several supports, got {points}");
    let o = run(["slice", p(&model), "-o", p(&lifted), "--supports", "--lift", "3"]
        .iter()
        .chain(&common));
    assert_ok(&o);

    let plain = open_sliced(&plain);
    let sup = open_sliced(&supported);
    let lift3 = open_sliced(&lifted);
    // default lift is 5 mm: 50 extra layers; --lift 3: 30 extra layers
    assert_eq!(plain.info().layer_count, 100);
    assert_eq!(sup.info().layer_count, 150);
    assert_eq!(lift3.info().layer_count, 130);

    let model_area = rect_px(printer, 10.0, 10.0);
    // plain: the first layer is the cube itself
    assert_close(layer(&*plain, 0).lit_area_px(), model_area, 0.03, "plain first layer");
    // supported: every layer below the model is lit (raft, then pillars) ...
    for i in 0..50 {
        let l = layer(&*sup, i);
        assert!(!l.is_blank(), "support layer {i} is empty");
    }
    // ... the pillars are much thinner than the model ...
    let pillars = layer(&*sup, 30).lit_area_px();
    assert!(
        pillars > 0.0 && pillars < model_area * 0.5,
        "pillar area {pillars} vs model {model_area}"
    );
    // ... and the model itself is unchanged above the lift height
    assert_close(
        layer(&*sup, 100).lit_area_px(),
        model_area,
        0.03,
        "supported model section",
    );
    // the raft is wider than the pillars
    assert!(layer(&*sup, 2).lit_area_px() > pillars);
    // the supported job uses more resin
    assert!(sup.info().volume_ml > plain.info().volume_ml);
}

#[test]
fn slice_auto_orient_lays_a_tall_part_down() {
    let d = TempDir::new("orient");
    // 20 mm tall slab: the largest faces are 6 x 20 mm
    let model = box_stl(&d, "slab.stl", Vec3::new(4.0, 6.0, 20.0));
    let upright = d.join("upright.goo");
    let flat = d.join("flat.goo");
    let args = ["-p", "elegoo-mars-4-dlp", "--layer-height", "0.1"];
    assert_ok(&run(["slice", p(&model), "-o", p(&upright)].iter().chain(&args)));
    assert_ok(&run(["slice", p(&model), "-o", p(&flat), "--auto-orient"]
        .iter()
        .chain(&args)));
    assert_eq!(open_sliced(&upright).info().layer_count, 200);
    let f = open_sliced(&flat);
    assert_eq!(f.info().layer_count, 40, "lying on a 6 x 20 face it is 4 mm tall");
    assert_close(
        layer(&*f, 20).lit_area_px(),
        rect_px("elegoo-mars-4-dlp", 20.0, 6.0),
        0.05,
        "flat section",
    );
}

#[test]
fn slice_several_models_obj_and_3mf() {
    let d = TempDir::new("multi");
    let printer = "elegoo-mars-4-dlp";
    let single = rect_px(printer, 10.0, 10.0);
    let stl = box_stl(&d, "a.stl", Vec3::splat(10.0));
    let obj = cube_obj(&d, "b.obj");
    let tmf = two_cubes_3mf(&d, "c.3mf");
    let args = ["-p", printer, "--layer-height", "0.1", "--aa", "1"];

    // OBJ alone gives the same cube as the STL
    let out = d.join("obj.goo");
    assert_ok(&run(["slice", p(&obj), "-o", p(&out)].iter().chain(&args)));
    let f = open_sliced(&out);
    assert_eq!(f.info().layer_count, 100);
    assert_close(layer(&*f, 50).lit_area_px(), single, 0.03, "obj cube");

    // 3MF with two build items: two cubes
    let out = d.join("3mf.goo");
    assert_ok(&run(["slice", p(&tmf), "-o", p(&out)].iter().chain(&args)));
    let f = open_sliced(&out);
    assert_eq!(f.info().layer_count, 100);
    assert_close(layer(&*f, 50).lit_area_px(), 2.0 * single, 0.03, "3mf cubes");

    // STL + OBJ + 3MF in one job: four cubes, arranged 3 mm apart without overlap
    let out = d.join("all.goo");
    let o = run(["slice", p(&stl), p(&obj), p(&tmf), "-o", p(&out)].iter().chain(&args));
    assert_ok(&o);
    assert!(!stderr(&o).contains("do not all fit"), "{}", stderr(&o));
    let f = open_sliced(&out);
    let l = layer(&*f, 50);
    assert_close(l.lit_area_px(), 4.0 * single, 0.03, "four cubes");
    // the output is named after the first model when -o is missing; checked elsewhere
    let (x0, _, x1, _) = l.bbox.unwrap();
    let (px, _) = pixel_mm(printer);
    let width_mm = (x1 - x0) as f32 * px;
    assert!(width_mm > 20.0, "cubes are spread out, block is {width_mm} mm wide");
}

// ------------------------------------------------------------------ slice: errors

#[test]
fn slice_errors_are_reported() {
    let d = TempDir::new("errors");
    let model = box_stl(&d, "cube.stl", Vec3::splat(10.0));
    let out = d.join("out.goo");

    let missing = d.join("missing.stl");
    let o = run(["slice", p(&missing), "-o", p(&out)]);
    assert_fails_with(&o, &["missing.stl"]);

    let o = run(["slice", p(&model), "-p", "no-such-printer", "-o", p(&out)]);
    assert_fails_with(&o, &["unknown printer 'no-such-printer'"]);

    let o = run(["slice", p(&model), "-f", "xyz", "-o", p(&out)]);
    assert_fails_with(&o, &["unknown format 'xyz'"]);

    let o = run(["slice", p(&model), "-o", p(&d.join("out.abc"))]);
    assert_fails_with(&o, &["cannot infer format", "out.abc"]);

    let txt = d.join("model.txt");
    std::fs::write(&txt, "not a model").unwrap();
    let o = run(["slice", p(&txt), "-o", p(&out)]);
    assert_fails_with(&o, &["model.txt"]);

    let empty = d.join("empty.stl");
    std::fs::write(&empty, b"").unwrap();
    let o = run(["slice", p(&empty), "-o", p(&out)]);
    assert_fails_with(&o, &["empty.stl"]);

    let huge = box_stl(&d, "huge.stl", Vec3::splat(300.0));
    let o = run(["slice", p(&huge), "-p", "elegoo-mars", "-o", p(&out)]);
    assert_fails_with(&o, &["do not fit into the build volume"]);

    let o = run(["slice", p(&model), "-o", p(&out), "--layer-height", "0"]);
    assert_fails_with(&o, &["invalid print settings", "layer height"]);
    let o = run(["slice", p(&model), "-o", p(&out), "--aa", "3"]);
    assert_fails_with(&o, &["invalid print settings", "anti-aliasing"]);
    let o = run(["slice", p(&model), "-o", p(&out), "--exposure", "abc"]);
    assert_fails_with(&o, &["--exposure"]);

    let bad = d.join("bad.json");
    std::fs::write(&bad, "{ \"layer_height\": ").unwrap();
    let o = run(["slice", p(&model), "-o", p(&out), "--settings", p(&bad)]);
    assert_fails_with(&o, &["invalid settings file", "bad.json"]);
    let o = run(["slice", p(&model), "-o", p(&out), "--settings", p(&d.join("nope.json"))]);
    assert_fails_with(&o, &["nope.json"]);

    let o = run(["slice"]);
    assert_fails_with(&o, &["<MODELS>"]);

    // nothing (not even a partial file) was written by the failed runs
    let left: Vec<_> = std::fs::read_dir(&d.0)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("out."))
        .collect();
    assert!(left.is_empty(), "failed runs left {left:?}");
}

// ------------------------------------------------------------------ info / layer

#[test]
fn info_and_layer_commands() {
    let d = TempDir::new("info");
    let model = box_stl(&d, "cube.stl", Vec3::splat(10.0));
    let out = d.join("cube.goo");
    let printer = "elegoo-mars-4-dlp";
    assert_ok(&run([
        "slice",
        p(&model),
        "-p",
        printer,
        "-o",
        p(&out),
        "--layer-height",
        "0.1",
        "--exposure",
        "2.7",
        "--aa",
        "1",
    ]));

    let o = run(["info", p(&out)]);
    assert_ok(&o);
    let info: SlicedInfo = serde_json::from_str(&stdout(&o)).unwrap_or_else(|e| panic!("{e}: {}", stdout(&o)));
    assert_info(&info, printer, 100, 0.1);
    assert!((info.exposure - 2.7).abs() < 1e-4);
    assert_eq!(info.format.to_lowercase(), "goo");

    // full resolution layer image
    let png = d.join("layer.png");
    let o = run(["layer", p(&out), "42", "-o", p(&png)]);
    assert_ok(&o);
    assert!(stdout(&o).contains("layer 42"), "{}", stdout(&o));
    let img = image::open(&png).unwrap().to_luma8();
    assert_eq!(img.dimensions(), (2560, 1440));
    let lit = img.pixels().filter(|p| p.0[0] > 127).count() as f64;
    assert_close(lit, rect_px(printer, 10.0, 10.0), 0.03, "png lit pixels");

    // downscaled preview
    let small = d.join("small.png");
    assert_ok(&run(["layer", p(&out), "0", "-o", p(&small), "--max-size", "256"]));
    let img = image::open(&small).unwrap().to_luma8();
    assert!(img.width() <= 256 && img.height() <= 256, "{:?}", img.dimensions());
    assert_eq!(img.dimensions(), (256, 144));
    assert!(img.pixels().any(|p| p.0[0] > 0));

    // errors
    let o = run(["layer", p(&out), "100", "-o", p(&d.join("x.png"))]);
    assert_fails_with(&o, &["100"]);
    assert!(!d.join("x.png").exists());
    let o = run(["info", p(&d.join("missing.goo"))]);
    assert_fails_with(&o, &["missing.goo"]);
    let o = run(["info", p(&model)]);
    assert_fails_with(&o, &["stl"]);
    let junk = d.join("junk.ctb");
    std::fs::write(&junk, b"definitely not a ctb file").unwrap();
    let o = run(["info", p(&junk)]);
    assert_fails_with(&o, &[]);
    let o = run(["layer", p(&out), "-1", "-o", p(&png)]);
    assert_fails_with(&o, &[]);
}

#[test]
fn every_writable_format_slices_and_reads_back() {
    // one run per output format the CLI accepts, all on the same small printer
    let d = TempDir::new("all-formats");
    let model = box_stl(&d, "cube.stl", Vec3::new(6.0, 4.0, 2.0));
    let printer = "sparkmaker-original";
    for f in openresin_formats::Format::ALL {
        let ext = f.extension();
        let out = d.join(&format!("cube.{ext}"));
        let o = run([
            "slice",
            p(&model),
            "-p",
            printer,
            "-o",
            p(&out),
            "--layer-height",
            "0.1",
            "--aa",
            "1",
        ]);
        assert_ok(&o);
        let file = open_sliced(&out);
        assert_eq!(file.info().layer_count, 20, "{ext}: {:?}", file.info());
        let l = layer(&*file, 10);
        assert_close(
            l.lit_area_px(),
            rect_px(printer, 6.0, 4.0),
            0.08,
            &format!("{ext} lit area"),
        );
    }
}

// ------------------------------------------------------------------ network (loopback only)

/// Network subcommands that need a printer connection.
fn connect_cmds(dir: &TempDir) -> Vec<Vec<String>> {
    let job = dir.join("job.goo");
    std::fs::write(&job, b"x").unwrap();
    let dl = dir.join("dl.goo");
    [
        vec!["status", "127.0.0.1"],
        vec!["files", "127.0.0.1"],
        vec!["print", "127.0.0.1", "job.goo"],
        vec!["pause", "127.0.0.1"],
        vec!["resume", "127.0.0.1"],
        vec!["stop", "127.0.0.1"],
        vec!["upload", "127.0.0.1", p(&job)],
        vec!["download", "127.0.0.1", "/local/job.goo", "-o", p(&dl)],
        vec!["camera", "127.0.0.1", "--snapshot", p(&dir.join("snap.jpg"))],
    ]
    .into_iter()
    .map(|v| v.into_iter().map(str::to_string).collect())
    .collect()
}

#[test]
fn network_commands_fail_cleanly_without_a_printer() {
    // 127.0.0.1 has no printer: every command must give up quickly with a clear
    // message. All commands run concurrently to keep the test short.
    let d = TempDir::new("net");
    let started = Instant::now();
    let children: Vec<_> = connect_cmds(&d)
        .into_iter()
        .map(|args| {
            let child = bin()
                .args(&args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            (args, child)
        })
        .collect();
    for (args, child) in children {
        let o = child.wait_with_output().unwrap();
        assert_fails_with(&o, &["127.0.0.1", "no printer answered"]);
        assert!(!stderr(&o).contains("panicked"), "{args:?}");
    }
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "connection attempts took {:?}",
        started.elapsed()
    );
    assert!(!d.join("dl.goo").exists(), "failed download left a file");
    assert!(!d.join("snap.jpg").exists());
}

#[test]
fn network_argument_validation() {
    for args in [
        vec!["status", "printer.local"],
        vec!["status", "10.0.0.300"],
        vec!["files"],
        vec!["upload", "127.0.0.1"],
        vec!["download", "127.0.0.1", "/local/x.goo"],
        vec!["print", "127.0.0.1"],
        vec!["discover", "--timeout", "soon"],
        vec!["camera"],
    ] {
        let started = Instant::now();
        let o = run(&args);
        assert_fails_with(&o, &[]);
        assert_eq!(
            o.status.code(),
            Some(2),
            "{args:?}: usage error expected\n{}",
            stderr(&o)
        );
        assert!(started.elapsed() < Duration::from_secs(5), "{args:?} did not fail fast");
    }
    let o = run(["discover", "--help"]);
    assert_ok(&o);
    assert!(stdout(&o).contains("--timeout") && stdout(&o).contains("--json"));
}

#[test]
fn upload_of_a_missing_file_fails_before_connecting() {
    let d = TempDir::new("upload-missing");
    let started = Instant::now();
    let o = run(["upload", "127.0.0.1", p(&d.join("nothing.goo"))]);
    assert_fails_with(&o, &["nothing.goo"]);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "should not try to connect first ({:?})",
        started.elapsed()
    );
}

/// A one-shot loopback MJPEG server: serves `jpeg` as a multipart stream.
fn mjpeg_server(jpeg: Vec<u8>) -> (u16, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let h = std::thread::spawn(move || {
        listener.set_nonblocking(false).unwrap();
        let (mut s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut req = [0u8; 2048];
        let _ = s.read(&mut req);
        let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary=frame\r\n\r\n");
        for _ in 0..20 {
            let part = format!(
                "--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
                jpeg.len()
            );
            if s.write_all(part.as_bytes())
                .and_then(|_| s.write_all(&jpeg))
                .and_then(|_| s.write_all(b"\r\n"))
                .is_err()
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    (port, h)
}

fn test_jpeg(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([(x * 4) as u8, (y * 4) as u8, 128]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Jpeg).unwrap();
    out.into_inner()
}

#[test]
fn camera_snapshot_from_a_stream_url() {
    let d = TempDir::new("camera");
    let jpeg = test_jpeg(64, 48);
    let (port, server) = mjpeg_server(jpeg.clone());
    let snap = d.join("snap.jpg");
    let url = format!("http://127.0.0.1:{port}/video");
    // with --url the printer itself is never contacted
    let o = run(["camera", "127.0.0.1", "--url", &url, "--snapshot", p(&snap)]);
    assert_ok(&o);
    assert!(stdout(&o).contains("saved 64x48 snapshot"), "{}", stdout(&o));
    assert_eq!(std::fs::read(&snap).unwrap(), jpeg, "snapshot is the served frame");
    server.join().unwrap();
}

#[test]
fn camera_snapshot_from_a_dead_stream_fails() {
    // a port that was just closed again: connection refused
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let d = TempDir::new("camera-dead");
    let snap = d.join("snap.jpg");
    let started = Instant::now();
    let o = run([
        "camera",
        "127.0.0.1",
        "--url",
        &format!("http://127.0.0.1:{port}/video"),
        "--snapshot",
        p(&snap),
    ]);
    assert_fails_with(&o, &["no frame received", "camera error"]);
    assert!(!snap.exists());
    assert!(started.elapsed() < Duration::from_secs(30), "{:?}", started.elapsed());
}
