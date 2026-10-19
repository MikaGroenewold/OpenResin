//! Printer (machine) definitions and print/resin settings.

use serde::{Deserialize, Serialize};

use crate::slicer::RasterOptions;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MirrorMode {
    /// Image is written exactly as seen from above.
    None,
    /// Standard mirrored LCD: the image is the plate as seen from below
    /// (rows flipped relative to the top view). Matches the reference slicer's
    /// "LCD mirror" output pixel for pixel.
    #[default]
    Lcd,
    /// Top view mirrored horizontally (custom setups).
    X,
    /// Top view mirrored vertically (custom setups).
    Y,
}

impl MirrorMode {
    /// Whether file headers should flag the image as mirrored.
    pub fn is_mirrored(self) -> bool {
        self != MirrorMode::None
    }
}

/// Network protocol spoken by a printer's mainboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NetworkProtocol {
    /// CBD/Chitu "Smart Device Control Protocol" v3 (WebSocket + HTTP upload).
    Sdcp,
    /// Legacy Chitu WiFi protocol (G-code over UDP, port 3000).
    ChituUdp,
    /// Anycubic Photon LAN protocol.
    Anycubic,
    /// Creality HALOT LAN protocol.
    Halot,
}

/// Static description of a printer model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrinterProfile {
    /// Stable identifier (slug), e.g. `elegoo-saturn-4-ultra`.
    pub id: String,
    pub vendor: String,
    pub name: String,
    pub resolution_x: u32,
    pub resolution_y: u32,
    /// Exposure area width in mm.
    pub size_x: f32,
    /// Exposure area depth in mm.
    pub size_y: f32,
    /// Maximum build height in mm.
    pub size_z: f32,
    #[serde(default)]
    pub mirror: MirrorMode,
    /// Output formats (file extensions) supported by the printer, preferred first.
    pub formats: Vec<String>,
    /// All formats the printer firmware understands (informational).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub native_formats: Vec<String>,
    /// LAN protocol, if the printer can receive files over the network.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<NetworkProtocol>,
    /// Machine type string written into files when a format needs it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_type: Option<String>,
}

impl PrinterProfile {
    pub fn pixel_size_um(&self) -> (f32, f32) {
        (
            self.size_x / self.resolution_x as f32 * 1000.0,
            self.size_y / self.resolution_y as f32 * 1000.0,
        )
    }

    pub fn raster_options(&self, anti_aliasing: u8) -> RasterOptions {
        RasterOptions {
            width: self.resolution_x,
            height: self.resolution_y,
            size_x: self.size_x,
            size_y: self.size_y,
            mirror_x: self.mirror == MirrorMode::X,
            mirror_y: matches!(self.mirror, MirrorMode::Y | MirrorMode::Lcd),
            anti_aliasing,
        }
    }

    pub fn display_name(&self) -> String {
        if self.name.to_lowercase().starts_with(&self.vendor.to_lowercase()) {
            self.name.clone()
        } else {
            format!("{} {}", self.vendor, self.name)
        }
    }

    pub fn machine_type(&self) -> String {
        self.machine_type.clone().unwrap_or_else(|| self.display_name())
    }

    /// A generic printer used when nothing else is selected.
    pub fn generic() -> Self {
        PrinterProfile {
            id: "generic-4k".into(),
            vendor: "Generic".into(),
            name: "Generic 4K".into(),
            resolution_x: 3840,
            resolution_y: 2400,
            size_x: 192.0,
            size_y: 120.0,
            size_z: 200.0,
            mirror: MirrorMode::Lcd,
            formats: vec!["ctb".into(), "sl1".into(), "zip".into()],
            native_formats: Vec::new(),
            network: None,
            machine_type: None,
        }
    }
}

static BUILTIN_PRINTERS: &str = include_str!("../../../profiles/printers.json");

/// The printer database shipped with OpenResin.
pub fn builtin_printers() -> Vec<PrinterProfile> {
    let mut list: Vec<PrinterProfile> =
        serde_json::from_str(BUILTIN_PRINTERS).expect("valid built-in printer database");
    list.insert(0, PrinterProfile::generic());
    list
}

/// Finds a printer by id or (case-insensitive) name fragment.
pub fn find_printer<'a>(list: &'a [PrinterProfile], query: &str) -> Option<&'a PrinterProfile> {
    let q = query.to_lowercase();
    list.iter()
        .find(|p| p.id == q || p.name.to_lowercase() == q)
        .or_else(|| list.iter().find(|p| p.display_name().to_lowercase().contains(&q)))
}

/// Two-stage motion (first stage `distance`/`speed`, optional second stage).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Motion {
    /// mm
    pub distance: f32,
    /// mm/min
    pub speed: f32,
    /// mm (0 disables the second stage)
    #[serde(default)]
    pub distance2: f32,
    /// mm/min
    #[serde(default)]
    pub speed2: f32,
}

impl Motion {
    pub const fn new(distance: f32, speed: f32) -> Self {
        Self {
            distance,
            speed,
            distance2: 0.0,
            speed2: 0.0,
        }
    }

    pub fn total_distance(&self) -> f32 {
        self.distance + self.distance2
    }

    /// Time in seconds needed to perform the move.
    pub fn duration_s(&self) -> f32 {
        let t = |d: f32, s: f32| if d > 0.0 && s > 0.0 { d / s * 60.0 } else { 0.0 };
        t(self.distance, self.speed) + t(self.distance2, self.speed2)
    }
}

/// Timing/motion settings for one class of layers (bottom or normal).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LayerParams {
    /// Exposure time in seconds.
    pub exposure: f32,
    /// Light-off delay before exposure (used when `wait_mode` is false), seconds.
    pub light_off_delay: f32,
    /// Rest time before lift, seconds.
    pub wait_before_lift: f32,
    /// Rest time after lift, seconds.
    pub wait_after_lift: f32,
    /// Rest time after retract (before exposure), seconds.
    pub wait_after_retract: f32,
    pub lift: Motion,
    pub retract: Motion,
    /// UV LED power 0..=255.
    pub light_pwm: u8,
}

/// Slicing and printing settings, usually tied to a resin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrintSettings {
    pub name: String,
    /// Layer height in mm.
    pub layer_height: f32,
    pub bottom_layers: u32,
    /// Layers whose exposure is linearly interpolated from bottom to normal exposure.
    pub transition_layers: u32,
    pub bottom: LayerParams,
    pub normal: LayerParams,
    /// Use the explicit rest times ("wait mode") instead of the light-off delay.
    pub wait_mode: bool,
    /// Anti-aliasing level: 1 (off), 2, 4, 8 or 16.
    pub anti_aliasing: u8,
    /// Resin density in g/ml.
    pub resin_density: f32,
    /// Resin price per litre.
    pub resin_price_per_liter: f32,
    pub currency: String,
}

impl Default for PrintSettings {
    fn default() -> Self {
        let normal = LayerParams {
            exposure: 2.5,
            light_off_delay: 0.5,
            wait_before_lift: 0.0,
            wait_after_lift: 0.0,
            wait_after_retract: 0.5,
            lift: Motion::new(6.0, 65.0),
            retract: Motion::new(6.0, 150.0),
            light_pwm: 255,
        };
        let bottom = LayerParams {
            exposure: 30.0,
            light_off_delay: 0.5,
            lift: Motion::new(6.0, 60.0),
            retract: Motion::new(6.0, 150.0),
            ..normal
        };
        PrintSettings {
            name: "Standard resin".into(),
            layer_height: 0.05,
            bottom_layers: 4,
            transition_layers: 0,
            bottom,
            normal,
            wait_mode: true,
            anti_aliasing: 4,
            resin_density: 1.1,
            resin_price_per_liter: 30.0,
            currency: "€".into(),
        }
    }
}

impl PrintSettings {
    /// Exposure time of layer `i`, including transition layers.
    pub fn exposure_for_layer(&self, i: u32) -> f32 {
        if i < self.bottom_layers {
            return self.bottom.exposure;
        }
        let t = i - self.bottom_layers;
        if t < self.transition_layers {
            let step = (self.bottom.exposure - self.normal.exposure) / (self.transition_layers + 1) as f32;
            return self.bottom.exposure - step * (t + 1) as f32;
        }
        self.normal.exposure
    }

    pub fn params_for_layer(&self, i: u32) -> &LayerParams {
        if i < self.bottom_layers {
            &self.bottom
        } else {
            &self.normal
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if !(0.005..=0.5).contains(&self.layer_height) {
            return Err(format!("layer height {} mm is out of range", self.layer_height));
        }
        if ![1u8, 2, 4, 8, 16].contains(&self.anti_aliasing) {
            return Err(format!("anti-aliasing level {} is not supported", self.anti_aliasing));
        }
        if self.normal.exposure <= 0.0 || self.bottom.exposure <= 0.0 {
            return Err("exposure times must be positive".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_database_loads() {
        let list = builtin_printers();
        assert!(list.len() > 100);
        let p = find_printer(&list, "saturn 4 ultra").unwrap();
        assert_eq!((p.resolution_x, p.resolution_y), (11520, 5120));
        assert_eq!(p.network, Some(NetworkProtocol::Sdcp));
        let ids: std::collections::HashSet<_> = list.iter().map(|p| &p.id).collect();
        assert_eq!(ids.len(), list.len(), "duplicate printer ids");
    }

    #[test]
    fn transition_matches_reference_slicer() {
        // Reference file: bottom 32s, normal 2.5s, 2 bottom layers, 5 transition layers
        // -> layer index 2 is exposed for 27.083s.
        let mut s = PrintSettings::default();
        s.bottom.exposure = 32.0;
        s.normal.exposure = 2.5;
        s.bottom_layers = 2;
        s.transition_layers = 5;
        assert!((s.exposure_for_layer(2) - 27.0833).abs() < 1e-3);
        assert_eq!(s.exposure_for_layer(7), 2.5);
        assert_eq!(s.exposure_for_layer(0), 32.0);
    }
}
