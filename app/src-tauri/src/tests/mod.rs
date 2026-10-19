//! Functional tests of the backend commands.
//!
//! Every test builds an app on Tauri's mock runtime with the real invoke
//! handler and a real [`AppState`] whose configuration and work directory live
//! in a fresh temp directory, then calls commands through the IPC layer exactly
//! like the frontend does (same command names, camelCase argument keys, JSON
//! payloads). Nothing touches the user's configuration, and the device tests
//! only use the loopback interface.

mod config;
mod devices;
mod edit;
mod geometry;
mod lifecycle;
mod scene;
mod slicing;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use openresin_core::geometry::Aabb;
use openresin_core::io::write_binary_stl;
use openresin_core::primitives::cuboid;
use openresin_core::scene::Transform;
use openresin_core::{Mesh, Vec3};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::ipc::{CallbackFn, InvokeBody, InvokeResponseBody};
use tauri::test::{get_ipc_response, mock_builder, mock_context, noop_assets, MockRuntime, INVOKE_KEY};
use tauri::webview::InvokeRequest;
use tauri::WebviewWindow;

use crate::scene_cmds::ObjectInfo;
use crate::state::AppState;

// ------------------------------------------------------------------ temp files

/// A unique directory under the system temp dir, removed on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new() -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "openresin-app-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    pub fn path_exists(&self) -> bool {
        self.0.is_dir()
    }

    pub fn join(&self, p: &str) -> PathBuf {
        self.0.join(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn s(p: &Path) -> String {
    p.to_str().unwrap().to_string()
}

// ------------------------------------------------------------------ app harness

pub struct TestApp {
    /// Shared with apps re-opened on the same configuration.
    pub dir: Arc<TempDir>,
    pub work_dir: PathBuf,
    webview: WebviewWindow<MockRuntime>,
    _app: tauri::App<MockRuntime>,
}

impl TestApp {
    /// A new app with an empty configuration.
    pub fn new() -> Self {
        Self::open(Arc::new(TempDir::new()))
    }

    /// Another app instance (fresh `AppState`) that loads the configuration
    /// saved by this one, like restarting the application.
    pub fn reopen(&self) -> Self {
        Self::open(self.dir.clone())
    }

    fn open(dir: Arc<TempDir>) -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let config_dir = dir.join("config");
        let work_dir = dir.join(&format!("work-{}", N.fetch_add(1, Ordering::Relaxed)));
        let state = AppState::with_dirs(&config_dir, work_dir.clone());
        let app = mock_builder()
            .manage(state)
            .invoke_handler(crate::invoke_handler())
            .build(mock_context(noop_assets()))
            .expect("failed to build the mock app");
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .expect("failed to create the mock webview");
        TestApp {
            dir,
            work_dir,
            webview,
            _app: app,
        }
    }

    /// The managed state, as the commands see it.
    pub fn state(&self) -> tauri::State<'_, AppState> {
        use tauri::Manager;
        self._app.state::<AppState>()
    }

    /// The mock app itself (e.g. to run its event loop) and the temp directory
    /// that must outlive it.
    pub fn into_app(self) -> (tauri::App<MockRuntime>, Arc<TempDir>) {
        (self._app, self.dir)
    }

    pub fn config_file(&self) -> PathBuf {
        self.dir.join("config").join("config.json")
    }

    fn request(cmd: &str, args: Value) -> InvokeRequest {
        InvokeRequest {
            cmd: cmd.into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: if cfg!(any(windows, target_os = "android")) {
                "http://tauri.localhost"
            } else {
                "tauri://localhost"
            }
            .parse()
            .unwrap(),
            body: InvokeBody::Json(args),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        }
    }

    /// Invokes a command; `Err` carries the error message the frontend would see.
    pub fn try_invoke(&self, cmd: &str, args: Value) -> Result<InvokeResponseBody, String> {
        invoke_on(&self.webview, cmd, args)
    }

    /// The mock webview, to invoke commands from another thread.
    pub fn webview(&self) -> WebviewWindow<MockRuntime> {
        self.webview.clone()
    }

    pub fn try_call<T: DeserializeOwned>(&self, cmd: &str, args: Value) -> Result<T, String> {
        self.try_invoke(cmd, args).map(|body| {
            let text = match &body {
                InvokeResponseBody::Json(s) => s.clone(),
                InvokeResponseBody::Raw(b) => format!("<{} raw bytes>", b.len()),
            };
            body.deserialize::<T>()
                .unwrap_or_else(|e| panic!("{cmd}: cannot decode response ({e}): {text}"))
        })
    }

    #[track_caller]
    pub fn call<T: DeserializeOwned>(&self, cmd: &str, args: Value) -> T {
        self.try_call(cmd, args).unwrap_or_else(|e| panic!("{cmd} failed: {e}"))
    }

    /// Calls a command whose result is ignored (unit or uninteresting).
    #[track_caller]
    pub fn run(&self, cmd: &str, args: Value) {
        if let Err(e) = self.try_invoke(cmd, args) {
            panic!("{cmd} failed: {e}");
        }
    }

    /// Calls a command that must fail and returns its error message.
    #[track_caller]
    pub fn fail(&self, cmd: &str, args: Value) -> String {
        match self.try_invoke(cmd, args) {
            Ok(body) => panic!("{cmd} unexpectedly succeeded: {body:?}"),
            Err(e) => e,
        }
    }

    /// Calls a command returning `tauri::ipc::Response` (raw bytes).
    #[track_caller]
    pub fn call_raw(&self, cmd: &str, args: Value) -> Vec<u8> {
        match self.try_invoke(cmd, args) {
            Ok(InvokeResponseBody::Raw(b)) => b,
            Ok(InvokeResponseBody::Json(j)) => panic!("{cmd}: expected raw bytes, got JSON {j}"),
            Err(e) => panic!("{cmd} failed: {e}"),
        }
    }

    // ---- shortcuts for frequent commands

    #[track_caller]
    pub fn import(&self, paths: &[&Path]) -> Vec<ObjectInfo> {
        let paths: Vec<String> = paths.iter().map(|p| s(p)).collect();
        self.call("import_models", json!({ "paths": paths }))
    }

    #[track_caller]
    pub fn import_one(&self, path: &Path) -> ObjectInfo {
        let mut v = self.import(&[path]);
        assert_eq!(v.len(), 1, "{} contains {} models", path.display(), v.len());
        v.remove(0)
    }

    /// Writes an axis aligned box (STL) and imports it.
    #[track_caller]
    pub fn add_box(&self, name: &str, size: Vec3) -> ObjectInfo {
        let path = self.dir.join(&format!("{name}.stl"));
        write_stl(&path, &cuboid(Vec3::ZERO, size));
        self.import_one(&path)
    }

    pub fn objects(&self) -> Vec<ObjectInfo> {
        self.call("scene_objects", json!({}))
    }

    #[track_caller]
    pub fn object(&self, id: u64) -> ObjectInfo {
        self.objects()
            .into_iter()
            .find(|o| o.id == id)
            .unwrap_or_else(|| panic!("object {id} not in the scene"))
    }

    #[track_caller]
    pub fn set_transform(&self, id: u64, t: Transform, keep_on_plate: bool) -> ObjectInfo {
        self.call(
            "set_transform",
            json!({ "id": id, "transform": t, "keepOnPlate": keep_on_plate }),
        )
    }

    #[track_caller]
    pub fn move_by(&self, id: u64, d: Vec3, keep_on_plate: bool) -> ObjectInfo {
        let mut t = self.object(id).transform;
        t.translation += d;
        self.set_transform(id, t, keep_on_plate)
    }

    #[track_caller]
    pub fn mesh(&self, cmd: &str, id: u64) -> Mesh {
        decode_mesh(&self.call_raw(cmd, json!({ "id": id })))
    }

    pub fn history(&self) -> (bool, bool) {
        #[derive(Deserialize)]
        struct H {
            can_undo: bool,
            can_redo: bool,
        }
        let h: H = self.call("history_state", json!({}));
        (h.can_undo, h.can_redo)
    }

    /// Selects a built-in printer and print settings for fast, exact slicing.
    #[track_caller]
    pub fn use_printer(&self, id: &str, layer_height: f32, aa: u8) {
        self.run("set_printer", json!({ "id": id }));
        let mut st = openresin_core::profile::PrintSettings {
            layer_height,
            anti_aliasing: aa,
            ..Default::default()
        };
        st.name = "test".into();
        self.run("set_settings", json!({ "settings": st }));
    }
}

/// Mirror of `scene_cmds::SliceSummary` for decoding.
#[derive(Debug, Clone, Deserialize)]
pub struct Summary {
    pub format: openresin_formats::Format,
    pub extension: String,
    pub layer_count: u32,
    pub print_time_s: f64,
    pub volume_ml: f64,
    pub weight_g: f64,
    pub cost: f64,
    pub currency: String,
    pub height_mm: f64,
    pub file_size: u64,
    pub duration_ms: u64,
    pub info: openresin_formats::SlicedInfo,
    pub layer_areas: Vec<f64>,
    pub suggested_name: String,
    pub warnings: Vec<String>,
}

impl TestApp {
    #[track_caller]
    pub fn slice(&self, format: Option<&str>) -> Summary {
        self.call("slice", json!({ "format": format, "thumbnail": null }))
    }

    /// Lit pixels of a layer of the current slice result (full resolution).
    #[track_caller]
    pub fn layer_lit(&self, index: u32) -> usize {
        png_lit(&self.call_raw("get_layer_image", json!({ "index": index, "maxSize": 0 }))).2
    }
}

/// Invokes a command on `webview`; `Err` carries the error message.
pub fn invoke_on(webview: &WebviewWindow<MockRuntime>, cmd: &str, args: Value) -> Result<InvokeResponseBody, String> {
    get_ipc_response(webview, TestApp::request(cmd, args)).map_err(|v| match v {
        Value::String(s) => s,
        other => other.to_string(),
    })
}

// ------------------------------------------------------------------ fixtures

pub fn write_stl(path: &Path, mesh: &Mesh) {
    let f = std::fs::File::create(path).unwrap();
    write_binary_stl(mesh, std::io::BufWriter::new(f)).unwrap();
}

/// 10 mm cube as a Wavefront OBJ with quad faces.
pub fn cube_obj(path: &Path) {
    let obj = "v 0 0 0\nv 10 0 0\nv 10 10 0\nv 0 10 0\n\
        v 0 0 10\nv 10 0 10\nv 10 10 10\nv 0 10 10\n\
        f 1 4 3 2\nf 5 6 7 8\nf 1 2 6 5\nf 2 3 7 6\nf 3 4 8 7\nf 4 1 5 8\n";
    std::fs::write(path, obj).unwrap();
}

/// 3MF with two named objects: a 10 mm cube and a 20 x 10 x 5 mm block.
pub fn two_objects_3mf(path: &Path) {
    fn box_xml(id: u32, name: &str, sx: f32, sy: f32, sz: f32) -> String {
        let v = [
            (0.0, 0.0, 0.0),
            (sx, 0.0, 0.0),
            (sx, sy, 0.0),
            (0.0, sy, 0.0),
            (0.0, 0.0, sz),
            (sx, 0.0, sz),
            (sx, sy, sz),
            (0.0, sy, sz),
        ];
        let t = [
            [0, 3, 2],
            [0, 2, 1],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [1, 2, 6],
            [1, 6, 5],
            [2, 3, 7],
            [2, 7, 6],
            [3, 0, 4],
            [3, 4, 7],
        ];
        let mut x = format!("<object id=\"{id}\" name=\"{name}\" type=\"model\"><mesh><vertices>");
        for (a, b, c) in v {
            x += &format!("<vertex x=\"{a}\" y=\"{b}\" z=\"{c}\"/>");
        }
        x += "</vertices><triangles>";
        for [a, b, c] in t {
            x += &format!("<triangle v1=\"{a}\" v2=\"{b}\" v3=\"{c}\"/>");
        }
        x + "</triangles></mesh></object>"
    }
    let model = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<model unit="millimeter" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">
<resources>{}{}</resources>
<build><item objectid="1"/><item objectid="2" transform="1 0 0 0 1 0 0 0 1 40 0 0"/></build>
</model>"#,
        box_xml(1, "Cube", 10.0, 10.0, 10.0),
        box_xml(2, "Block", 20.0, 10.0, 5.0)
    );
    let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("3D/3dmodel.model", opts).unwrap();
    zip.write_all(model.as_bytes()).unwrap();
    zip.finish().unwrap();
}

/// Decodes the `get_mesh` wire format.
pub fn decode_mesh(b: &[u8]) -> Mesh {
    let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
    let nv = u32_at(0) as usize;
    let nt = u32_at(4) as usize;
    assert_eq!(b.len(), 8 + nv * 12 + nt * 12, "mesh buffer size");
    let f32_at = |i: usize| f32::from_le_bytes(b[i..i + 4].try_into().unwrap());
    let positions = (0..nv)
        .map(|i| Vec3::new(f32_at(8 + i * 12), f32_at(12 + i * 12), f32_at(16 + i * 12)))
        .collect();
    let base = 8 + nv * 12;
    let triangles = (0..nt)
        .map(|i| {
            [
                u32_at(base + i * 12),
                u32_at(base + i * 12 + 4),
                u32_at(base + i * 12 + 8),
            ]
        })
        .collect();
    let m = Mesh::new(positions, triangles);
    assert!(
        m.triangles.iter().flatten().all(|&i| (i as usize) < nv),
        "index out of range"
    );
    m
}

// ------------------------------------------------------------------ assertions

pub fn size(b: &Aabb) -> Vec3 {
    b.max - b.min
}

#[track_caller]
pub fn assert_near(a: f32, b: f32, eps: f32, what: &str) {
    assert!((a - b).abs() <= eps, "{what}: {a} != {b} (± {eps})");
}

#[track_caller]
pub fn assert_vec_near(a: Vec3, b: Vec3, eps: f32, what: &str) {
    assert!((a - b).abs().max_element() <= eps, "{what}: {a} != {b} (± {eps})");
}

#[track_caller]
pub fn assert_rel(a: f64, b: f64, rel: f64, what: &str) {
    assert!(
        (a - b).abs() <= b.abs() * rel,
        "{what}: {a} != {b} (± {:.1}%)",
        rel * 100.0
    );
}

/// Solid objects do not overlap in XY (at least `gap` mm apart) and lie inside
/// a plate of `w` x `d` mm centred on the origin.
#[track_caller]
pub fn assert_laid_out(objs: &[ObjectInfo], w: f32, d: f32, gap: f32) {
    for (i, a) in objs.iter().enumerate() {
        let b = a.bounds;
        assert!(
            b.min.x >= -w / 2.0 - 1e-3
                && b.max.x <= w / 2.0 + 1e-3
                && b.min.y >= -d / 2.0 - 1e-3
                && b.max.y <= d / 2.0 + 1e-3,
            "{} is off the plate: {b:?}",
            a.name
        );
        assert!(
            b.min.z.abs() < 1e-3,
            "{} is not on the plate (min z {})",
            a.name,
            b.min.z
        );
        for o in &objs[i + 1..] {
            let c = o.bounds;
            let apart = b.max.x + gap - 1e-3 <= c.min.x
                || c.max.x + gap - 1e-3 <= b.min.x
                || b.max.y + gap - 1e-3 <= c.min.y
                || c.max.y + gap - 1e-3 <= b.min.y;
            assert!(apart, "{} {b:?} and {} {c:?} overlap", a.name, o.name);
        }
    }
}

/// Decodes a PNG and counts pixels brighter than mid grey.
pub fn png_lit(png: &[u8]) -> (u32, u32, usize) {
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .expect("valid PNG")
        .to_luma8();
    let lit = img.pixels().filter(|p| p.0[0] > 127).count();
    (img.width(), img.height(), lit)
}

/// Pixel size (mm) of a built-in printer.
pub fn pixel_mm(printer: &str) -> (f32, f32) {
    let p = openresin_core::profile::builtin_printers()
        .into_iter()
        .find(|p| p.id == printer)
        .unwrap();
    (p.size_x / p.resolution_x as f32, p.size_y / p.resolution_y as f32)
}

/// Pixels covered by `area_mm2` on a built-in printer.
pub fn area_px(printer: &str, area_mm2: f64) -> f64 {
    let (x, y) = pixel_mm(printer);
    area_mm2 / (x as f64 * y as f64)
}
