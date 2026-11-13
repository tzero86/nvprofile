//! Public, serde-serializable domain types. Nothing here depends on a backend.

use serde::{Deserialize, Serialize};

/// Static description of the GPU being managed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpuInfo {
    /// LACT device id, e.g. `10DE:2208-1569:2208-0000:0a:00.0`.
    pub id: String,
    pub name: String,
    pub subsystem: Option<String>,
    pub driver: Option<String>,
    pub vbios: Option<String>,
    pub family: Option<String>,
    pub vram_total_mb: Option<u64>,
    /// Capability flags reported by the daemon (e.g. `ConfigurableFanControl`).
    pub capabilities: Vec<String>,
}

/// One point of the GPU voltage/frequency curve as the driver reports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VfPointInfo {
    pub index: u32,
    pub voltage_mv: u32,
    pub base_freq_mhz: u32,
}

/// Hardware limits used to validate presets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpuLimits {
    pub power_min_w: f64,
    pub power_max_w: f64,
    pub power_default_w: f64,
    /// Lowest manual fan speed as a 0..1 fraction (driver PWM floor, ~0.30).
    pub fan_min: f64,
    pub clock_min_mhz: u32,
    pub clock_max_mhz: u32,
    pub offset_min_mhz: i32,
    pub offset_max_mhz: i32,
    /// The driver's V/F curve; empty when unknown. Needed to expand [`VfSpec::Flat`].
    pub vf_points: Vec<VfPointInfo>,
}

/// One live reading. Every field is optional: backends fill what they can.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Stats {
    pub temp_c: Option<f64>,
    pub hotspot_c: Option<f64>,
    pub vram_temp_c: Option<f64>,
    pub fan_pct: Option<f64>,
    pub fan_rpm: Option<u32>,
    pub power_draw_w: Option<f64>,
    pub power_cap_w: Option<f64>,
    pub gpu_clock_mhz: Option<u32>,
    pub mem_clock_mhz: Option<u32>,
    pub voltage_mv: Option<u32>,
    pub util_pct: Option<u32>,
    pub vram_used_mb: Option<u64>,
    pub vram_total_mb: Option<u64>,
    /// Active throttle reasons (e.g. `SW_POWER_CAP`); empty when unthrottled.
    pub throttle: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CurvePoint {
    pub temp_c: u32,
    /// Fan speed as a 0..1 fraction.
    pub speed: f64,
}

/// Fan behaviour. Speeds are 0..1 fractions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FanSpec {
    /// Driver-managed fan (LACT fan control disabled).
    Auto,
    Static { speed: f64 },
    Curve {
        points: Vec<CurvePoint>,
        /// Below this temperature the fan is handed back to the driver's auto
        /// mode (LACT `auto_threshold`), which can stop the fans entirely on
        /// cards whose manual PWM floor is ~30%.
        #[serde(default)]
        zero_rpm_below_c: Option<u32>,
        /// Minimum temperature change before the speed is re-evaluated
        /// (LACT `change_threshold`).
        #[serde(default)]
        hysteresis_c: u32,
        /// Delay before slowing the fan down (LACT `spindown_delay_ms`).
        #[serde(default)]
        spindown_delay_ms: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VfOffset {
    pub index: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voltage_mv: Option<u32>,
    pub offset_mhz: i32,
}

/// Voltage/frequency tuning.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VfSpec {
    /// No offsets, no clock cap.
    Stock,
    /// Explicit per-point offsets (Afterburner-style curve).
    Offsets { points: Vec<VfOffset> },
    /// The same offset on every point of the driver's V/F curve.
    Flat { offset_mhz: i32 },
    /// Cap the maximum graphics clock.
    MaxClock { mhz: u32 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    /// Slug: `[a-z0-9][a-z0-9_-]{0,31}`; also the user preset file stem.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// `None` leaves the driver default (no LACT override).
    #[serde(default)]
    pub power_cap_w: Option<f64>,
    pub fan: FanSpec,
    pub vf: VfSpec,
    /// True for presets compiled into the engine (never read from YAML).
    #[serde(default, skip_serializing_if = "is_false")]
    pub builtin: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// The settings the daemon currently has applied, in preset vocabulary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppliedSettings {
    pub power_cap_w: Option<f64>,
    pub fan: FanSpec,
    pub vf: VfSpec,
}

/// Which preset (if any) matches what is applied right now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActiveState {
    /// `None` means "custom": the applied config matches no known preset.
    pub preset_id: Option<String>,
    pub preset_name: Option<String>,
    pub applied: AppliedSettings,
}

/// One differing setting between the applied config and a preset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Change {
    pub field: String,
    pub current: String,
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diff {
    pub preset_id: String,
    /// True when applying the preset would change nothing.
    pub matches: bool,
    pub changes: Vec<Change>,
}

/// How settings reach the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyPath {
    /// `SetGpuConfig` + `ConfirmPendingConfig` over `/run/lactd.sock`; the
    /// daemon applies and persists the config itself. No root needed.
    DaemonSocket,
    /// Render a merged config to a temp file; the caller must run the returned
    /// [`PrivilegedWrite`] with elevated rights.
    PrivilegedInstall,
}

/// A file install that needs root. The engine never runs it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrivilegedWrite {
    pub source: String,
    pub dest: String,
    /// argv to run, e.g. `["sudo","install","-m","644",source,dest]`.
    pub command: Vec<String>,
}

impl PrivilegedWrite {
    pub fn install(source: &str, dest: &str) -> Self {
        Self {
            source: source.to_owned(),
            dest: dest.to_owned(),
            command: ["sudo", "install", "-m", "644", source, dest]
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApplyPlan {
    pub preset_id: String,
    pub path: ApplyPath,
    pub changes: Vec<Change>,
    /// Per-GPU LACT config block that will be written (V/F points elided).
    pub config_block: String,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ApplyOutcome {
    DryRun,
    /// The target already matches the applied config; nothing was sent.
    NoChange,
    Applied {
        /// Re-read of the daemon config matched the target.
        verified: bool,
        notes: Vec<String>,
        backup: Option<String>,
    },
    PrivilegeRequired {
        write: PrivilegedWrite,
        backup: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApplyResult {
    pub plan: ApplyPlan,
    pub outcome: ApplyOutcome,
}

/// Catalog plus (when the GPU is reachable) which entry is active.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PresetList {
    pub presets: Vec<Preset>,
    /// Per-file load problems for user presets (they are skipped).
    pub problems: Vec<String>,
    pub active: Option<ActiveState>,
}

/// Everything a status screen needs in one call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub gpu: GpuInfo,
    pub limits: GpuLimits,
    pub stats: Stats,
    pub active: ActiveState,
    pub apply_path: ApplyPath,
}

impl FanSpec {
    pub fn summary(&self) -> String {
        match self {
            FanSpec::Auto => "auto (driver curve)".to_owned(),
            FanSpec::Static { speed } => format!("static {:.0}%", speed * 100.0),
            FanSpec::Curve { points, zero_rpm_below_c, .. } => {
                let lo = points.first().map_or(0.0, |p| p.speed) * 100.0;
                let hi = points.last().map_or(0.0, |p| p.speed) * 100.0;
                let t_lo = points.first().map_or(0, |p| p.temp_c);
                let t_hi = points.last().map_or(0, |p| p.temp_c);
                let zero = zero_rpm_below_c
                    .filter(|t| *t > 0)
                    .map(|t| format!(", zero-RPM < {t}C"))
                    .unwrap_or_default();
                format!("curve {lo:.0}% @ {t_lo}C -> {hi:.0}% @ {t_hi}C{zero}")
            }
        }
    }
}

impl VfSpec {
    pub fn summary(&self) -> String {
        match self {
            VfSpec::Stock => "stock".to_owned(),
            VfSpec::Flat { offset_mhz } => format!("flat {offset_mhz:+} MHz on all points"),
            VfSpec::MaxClock { mhz } => format!("max clock {mhz} MHz"),
            VfSpec::Offsets { points } => {
                let lo = points.iter().map(|p| p.offset_mhz).min().unwrap_or(0);
                let hi = points.iter().map(|p| p.offset_mhz).max().unwrap_or(0);
                format!("{}-point curve, offsets {lo:+}..{hi:+} MHz", points.len())
            }
        }
    }
}

pub fn power_summary(cap: Option<f64>) -> String {
    match cap {
        Some(w) => format!("{w:.1} W"),
        None => "driver default".to_owned(),
    }
}
