//! Protocol independent printer state.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MachineState {
    #[default]
    Unknown,
    Offline,
    Idle,
    Printing,
    Pausing,
    Paused,
    Stopping,
    Stopped,
    Completed,
    FileTransfer,
    Homing,
    FileChecking,
    ExposureTest,
    SelfTest,
    Error,
}

impl MachineState {
    pub fn is_busy(self) -> bool {
        matches!(
            self,
            MachineState::Printing
                | MachineState::Pausing
                | MachineState::Paused
                | MachineState::Stopping
                | MachineState::FileTransfer
                | MachineState::Homing
                | MachineState::FileChecking
                | MachineState::ExposureTest
                | MachineState::SelfTest
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PrinterStatus {
    pub state: MachineState,
    /// Human readable detail (sub-state, error text).
    pub detail: Option<String>,
    pub filename: Option<String>,
    pub current_layer: Option<u32>,
    pub total_layers: Option<u32>,
    /// Elapsed print time in seconds.
    pub elapsed_s: Option<u64>,
    /// Estimated total print time in seconds.
    pub total_s: Option<u64>,
    pub uv_led_temp: Option<f32>,
    pub box_temp: Option<f32>,
    pub error_code: Option<i64>,
    /// Remaining storage in bytes, when reported.
    pub free_storage: Option<u64>,
    pub camera_connected: Option<bool>,
    /// Whether time-lapse recording is enabled.
    #[serde(default)]
    pub time_lapse: Option<bool>,
}

impl PrinterStatus {
    pub fn progress(&self) -> Option<f32> {
        match (self.current_layer, self.total_layers) {
            (Some(c), Some(t)) if t > 0 => Some((c as f32 / t as f32).clamp(0.0, 1.0)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub is_dir: bool,
    pub size: Option<u64>,
}
