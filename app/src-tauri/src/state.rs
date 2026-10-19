//! Application state and persisted configuration.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use openresin_core::profile::{builtin_printers, PrintSettings, PrinterProfile};
use openresin_core::scene::Scene;
use openresin_core::support::SupportSettings;
use openresin_formats::SlicedFile;
use openresin_net::camera::CameraRelay;
use openresin_net::PrinterClient;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedDevice {
    pub ip: String,
    pub name: String,
    #[serde(default)]
    pub model: Option<String>,
}

/// Persisted user configuration (stored as JSON in the OS config directory).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub printer_id: String,
    /// Print settings per printer id.
    pub settings: HashMap<String, PrintSettings>,
    pub support_settings: SupportSettings,
    pub devices: Vec<SavedDevice>,
    pub custom_printers: Vec<PrinterProfile>,
    /// User presets (resin profiles) per printer id.
    pub presets: HashMap<String, Vec<PrintSettings>>,
    pub theme: String,
    pub last_dir: Option<String>,
    /// File this configuration is loaded from and saved to (not serialised).
    #[serde(skip)]
    pub file: Option<PathBuf>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            printer_id: "generic-4k".into(),
            settings: HashMap::new(),
            support_settings: SupportSettings::default(),
            devices: Vec::new(),
            custom_printers: Vec::new(),
            presets: HashMap::new(),
            theme: "dark".into(),
            last_dir: None,
            file: None,
        }
    }
}

impl AppConfig {
    pub fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("OpenResin").join("config.json"))
    }

    /// Loads the configuration from the OS config directory.
    pub fn load() -> Self {
        Self::load_from(Self::path())
    }

    /// Loads the configuration from `file` (defaults if it is missing or
    /// invalid); [`save`](Self::save) writes back to the same file.
    pub fn load_from(file: Option<PathBuf>) -> Self {
        let mut cfg: Self = file
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        cfg.file = file;
        cfg
    }

    pub fn save(&self) {
        if let Some(p) = &self.file {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(s) = serde_json::to_string_pretty(self) {
                let _ = std::fs::write(p, s);
            }
        }
    }

    pub fn settings_for(&self, printer_id: &str) -> PrintSettings {
        self.settings.get(printer_id).cloned().unwrap_or_default()
    }
}

pub struct SliceOutput {
    pub path: PathBuf,
    pub file: Box<dyn SlicedFile>,
    pub job_name: String,
}

#[derive(Default)]
pub struct History {
    pub undo: Vec<Scene>,
    pub redo: Vec<Scene>,
}

pub struct AppState {
    pub scene: Mutex<Scene>,
    pub history: Mutex<History>,
    pub config: Mutex<AppConfig>,
    pub slice: Mutex<Option<SliceOutput>>,
    pub cancel_slice: Arc<AtomicBool>,
    pub devices: tokio::sync::Mutex<HashMap<String, Arc<PrinterClient>>>,
    pub cameras: tokio::sync::Mutex<HashMap<String, CameraRelay>>,
    pub uploads: Mutex<HashMap<String, Arc<AtomicBool>>>,
    pub downloads: Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// Thumbnails of files stored on printers, keyed by "ip|path".
    pub remote_thumbs: Mutex<HashMap<String, Option<Vec<u8>>>>,
    pub work_dir: PathBuf,
}

impl AppState {
    pub fn new() -> Self {
        let work_dir = std::env::temp_dir().join(format!("openresin-{}", std::process::id()));
        Self::with_config(AppConfig::load(), work_dir)
    }

    /// State whose configuration lives in `config_dir/config.json` and whose
    /// sliced files go to `work_dir` (tests must never touch the user's files).
    #[cfg(test)]
    pub fn with_dirs(config_dir: &std::path::Path, work_dir: PathBuf) -> Self {
        Self::with_config(AppConfig::load_from(Some(config_dir.join("config.json"))), work_dir)
    }

    fn with_config(config: AppConfig, work_dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&work_dir);
        Self {
            scene: Mutex::new(Scene::default()),
            history: Mutex::new(History::default()),
            config: Mutex::new(config),
            slice: Mutex::new(None),
            cancel_slice: Arc::new(AtomicBool::new(false)),
            devices: tokio::sync::Mutex::new(HashMap::new()),
            cameras: tokio::sync::Mutex::new(HashMap::new()),
            uploads: Mutex::new(HashMap::new()),
            downloads: Mutex::new(HashMap::new()),
            remote_thumbs: Mutex::new(HashMap::new()),
            work_dir,
        }
    }

    pub fn printers(&self) -> Vec<PrinterProfile> {
        let mut list = builtin_printers();
        list.extend(self.config.lock().unwrap().custom_printers.iter().cloned());
        list
    }

    pub fn current_printer(&self) -> PrinterProfile {
        let id = self.config.lock().unwrap().printer_id.clone();
        self.printers()
            .into_iter()
            .find(|p| p.id == id)
            .unwrap_or_else(PrinterProfile::generic)
    }

    pub fn current_settings(&self) -> PrintSettings {
        let cfg = self.config.lock().unwrap();
        cfg.settings_for(&cfg.printer_id)
    }
}

impl AppState {
    /// Deletes the work directory with the sliced files (when the app exits).
    /// A running slice is cancelled and the last result closed first, so that
    /// Windows lets go of the files. Errors (e.g. a file still open) are
    /// ignored; calling it again is harmless.
    pub fn cleanup_work_dir(&self) {
        self.cancel_slice.store(true, Ordering::Relaxed);
        if let Ok(mut slice) = self.slice.lock() {
            *slice = None;
        }
        let _ = std::fs::remove_dir_all(&self.work_dir);
    }
}

impl Drop for AppState {
    fn drop(&mut self) {
        // not reached when the app exits (managed state is never dropped);
        // see `cleanup_work_dir`
        let _ = std::fs::remove_dir_all(&self.work_dir);
    }
}
