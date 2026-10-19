//! Print time, resin usage and cost estimation.

use serde::{Deserialize, Serialize};

use crate::profile::PrintSettings;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrintEstimate {
    pub layer_count: u32,
    /// Seconds.
    pub print_time_s: f64,
    /// Cured resin volume in mm³.
    pub volume_mm3: f64,
    /// Grams.
    pub weight_g: f64,
    pub cost: f64,
    pub currency: String,
    pub height_mm: f64,
}

impl PrintEstimate {
    pub fn volume_ml(&self) -> f64 {
        self.volume_mm3 / 1000.0
    }
}

/// Time in seconds that layer `i` takes (exposure, waits and motion).
pub fn layer_time_s(settings: &PrintSettings, i: u32) -> f32 {
    let p = settings.params_for_layer(i);
    let exposure = settings.exposure_for_layer(i);
    let waits = if settings.wait_mode {
        p.wait_before_lift + p.wait_after_lift + p.wait_after_retract
    } else {
        p.light_off_delay
    };
    exposure + waits + p.lift.duration_s() + p.retract.duration_s()
}

/// Estimates totals from the per-layer lit areas (in mm²).
pub fn estimate(settings: &PrintSettings, layer_areas_mm2: &[f64]) -> PrintEstimate {
    let n = layer_areas_mm2.len() as u32;
    let print_time_s = (0..n).map(|i| layer_time_s(settings, i) as f64).sum();
    let volume_mm3: f64 = layer_areas_mm2.iter().sum::<f64>() * settings.layer_height as f64;
    let ml = volume_mm3 / 1000.0;
    PrintEstimate {
        layer_count: n,
        print_time_s,
        volume_mm3,
        weight_g: ml * settings.resin_density as f64,
        cost: ml / 1000.0 * settings.resin_price_per_liter as f64,
        currency: settings.currency.clone(),
        height_mm: n as f64 * settings.layer_height as f64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totals() {
        let s = PrintSettings::default();
        let e = estimate(&s, &[100.0; 10]);
        assert_eq!(e.layer_count, 10);
        assert!((e.volume_mm3 - 50.0).abs() < 1e-4);
        assert!(e.print_time_s > 0.0);
    }
}
