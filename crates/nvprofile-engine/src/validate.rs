//! Preset validation against live GPU limits.

use crate::error::{EngineError, Result};
use crate::types::*;

/// Tolerance so `0.30` passes against a 77/255 = 0.302 PWM floor.
const FAN_EPS: f64 = 0.005;

/// Valid preset id: `[a-z0-9][a-z0-9_-]{0,31}`.
pub fn is_valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    id.len() <= 32 && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// Turn a display name into a valid id (`"Quiet Render!"` -> `"quiet-render"`).
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_end_matches('-');
    trimmed.chars().take(32).collect::<String>().trim_end_matches('-').to_owned()
}

/// All problems with `preset` given `limits`; empty when valid.
pub fn issues(preset: &Preset, limits: &GpuLimits) -> Vec<String> {
    let mut out = Vec::new();
    if !is_valid_id(&preset.id) {
        out.push(format!("id '{}' must match [a-z0-9][a-z0-9_-]{{0,31}}", preset.id));
    }
    if preset.name.trim().is_empty() {
        out.push("name must not be empty".into());
    }

    if let Some(w) = preset.power_cap_w {
        if !(limits.power_min_w..=limits.power_max_w).contains(&w) {
            out.push(format!(
                "power cap {w} W outside the GPU range {}-{} W",
                limits.power_min_w, limits.power_max_w
            ));
        }
    }

    match &preset.fan {
        FanSpec::Auto => {}
        FanSpec::Static { speed } => {
            if !(0.0..=1.0).contains(speed) {
                out.push(format!("static fan speed {speed} outside 0..1"));
            } else if *speed + FAN_EPS < limits.fan_min {
                out.push(format!(
                    "static fan {:.0}% is below the {:.0}% manual minimum; use the auto fan or a curve with zero_rpm_below_c",
                    speed * 100.0,
                    limits.fan_min * 100.0
                ));
            }
        }
        FanSpec::Curve { points, zero_rpm_below_c, .. } => {
            if points.len() < 2 {
                out.push("fan curve needs at least 2 points".into());
            }
            if let Some(t) = zero_rpm_below_c {
                if !(20..=90).contains(t) {
                    out.push(format!("zero_rpm_below_c {t} outside 20..90 C"));
                }
            }
            for w in points.windows(2) {
                if w[1].temp_c <= w[0].temp_c {
                    out.push(format!("fan curve temperatures must strictly increase ({} then {})", w[0].temp_c, w[1].temp_c));
                }
                if w[1].speed + 1e-9 < w[0].speed {
                    out.push(format!("fan curve speed must not decrease ({}C -> {}C)", w[0].temp_c, w[1].temp_c));
                }
            }
            for p in points {
                if p.temp_c > 120 {
                    out.push(format!("fan curve temperature {}C above 120C", p.temp_c));
                }
                if !(0.0..=1.0).contains(&p.speed) {
                    out.push(format!("fan speed {} at {}C outside 0..1", p.speed, p.temp_c));
                    continue;
                }
                // Below the zero-RPM threshold the driver owns the fan, so low
                // points there are meaningless but harmless.
                let driver_owned = zero_rpm_below_c.is_some_and(|z| z > 0 && p.temp_c < z);
                if p.speed + FAN_EPS < limits.fan_min && !driver_owned {
                    out.push(format!(
                        "fan {:.0}% at {}C is below the {:.0}% manual minimum; set zero_rpm_below_c above {}C or raise the speed",
                        p.speed * 100.0,
                        p.temp_c,
                        limits.fan_min * 100.0,
                        p.temp_c
                    ));
                }
            }
        }
    }

    match &preset.vf {
        VfSpec::Stock => {}
        VfSpec::Offsets { points } => {
            if points.is_empty() {
                out.push("offset list is empty (use vf type 'stock')".into());
            }
            let mut seen = std::collections::BTreeSet::new();
            for p in points {
                if !seen.insert(p.index) {
                    out.push(format!("duplicate V/F index {}", p.index));
                }
                if !(limits.offset_min_mhz..=limits.offset_max_mhz).contains(&p.offset_mhz) {
                    out.push(format!(
                        "V/F point {} offset {:+} MHz outside {:+}..{:+}",
                        p.index, p.offset_mhz, limits.offset_min_mhz, limits.offset_max_mhz
                    ));
                }
                if !limits.vf_points.is_empty() && !limits.vf_points.iter().any(|v| v.index == p.index) {
                    out.push(format!("V/F point {} does not exist on this GPU", p.index));
                }
            }
        }
        VfSpec::Flat { offset_mhz } => {
            if !(limits.offset_min_mhz..=limits.offset_max_mhz).contains(offset_mhz) {
                out.push(format!(
                    "flat offset {offset_mhz:+} MHz outside {:+}..{:+}",
                    limits.offset_min_mhz, limits.offset_max_mhz
                ));
            }
        }
        VfSpec::MaxClock { mhz } => {
            if !(limits.clock_min_mhz..=limits.clock_max_mhz).contains(mhz) {
                out.push(format!("max clock {mhz} MHz outside {}-{} MHz", limits.clock_min_mhz, limits.clock_max_mhz));
            }
        }
    }
    out
}

/// `Ok(())` when valid, otherwise [`EngineError::Validation`].
pub fn validate(preset: &Preset, limits: &GpuLimits) -> Result<()> {
    let issues = issues(preset, limits);
    if issues.is_empty() {
        Ok(())
    } else {
        Err(EngineError::Validation { issues })
    }
}
