//! Pure translation between presets and LACT's per-GPU config block.
//!
//! LACT (`/etc/lact/config.yaml`, version 7) stores per-GPU settings under
//! `gpus.<id>` (or `profiles.<current_profile>.gpus.<id>` when a LACT profile
//! is active). The engine only ever touches five keys of that block
//! ([`MANAGED_KEYS`]); everything else (daemon block, other GPUs, profiles,
//! `pmfw_options`, memory offsets, ...) is passed through untouched.

use std::collections::BTreeMap;
use std::fmt;

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_norway::{Mapping, Value as Yaml};

use crate::error::{EngineError, Result};
use crate::types::*;

/// Keys of the per-GPU block owned by nvprofile.
pub const MANAGED_KEYS: [&str; 5] = [
    "fan_control_enabled",
    "fan_control_settings",
    "power_cap",
    "nvidia_gpu_vf_curve",
    "max_core_clock",
];

/// LACT temperature sensor used for fan control (matches the known-good config).
pub const TEMPERATURE_KEY: &str = "edge";
/// How often LACT re-evaluates the fan (ms).
pub const FAN_INTERVAL_MS: u64 = 500;

const SPEED_EPS: f64 = 0.005;
const POWER_EPS_W: f64 = 0.5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FanMode {
    Static,
    Curve,
}

/// LACT `fan_control_settings`. LACT requires every field except the three
/// trailing options; the renderer always fills all of them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FanSettings {
    pub mode: FanMode,
    #[serde(default = "default_static_speed")]
    pub static_speed: f64,
    pub temperature_key: String,
    pub interval_ms: u64,
    #[serde(deserialize_with = "de_int_map")]
    pub curve: BTreeMap<u32, f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spindown_delay_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_threshold: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_threshold: Option<u64>,
}

fn default_static_speed() -> f64 {
    0.5
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VfPointCfg {
    pub clockspeed_offset: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voltage: Option<u32>,
}

/// The subset of a LACT per-GPU config that nvprofile reads and writes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LactGpuConfig {
    #[serde(default)]
    pub fan_control_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fan_control_settings: Option<FanSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power_cap: Option<f64>,
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "de_int_map"
    )]
    pub nvidia_gpu_vf_curve: BTreeMap<u32, VfPointCfg>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_core_clock: Option<i32>,
}

/// LACT writes integer map keys; JSON transports them as strings. Accept both.
fn de_int_map<'de, D, K, V>(d: D) -> std::result::Result<BTreeMap<K, V>, D::Error>
where
    D: Deserializer<'de>,
    K: Ord + TryFrom<i64>,
    V: Deserialize<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Key {
        Int(i64),
        Str(String),
    }
    struct V2<K, V>(std::marker::PhantomData<(K, V)>);
    impl<'de, K: Ord + TryFrom<i64>, V: Deserialize<'de>> Visitor<'de> for V2<K, V> {
        type Value = BTreeMap<K, V>;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a map with integer keys")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> std::result::Result<Self::Value, A::Error> {
            let mut out = BTreeMap::new();
            while let Some((k, v)) = a.next_entry::<Key, V>()? {
                let n = match &k {
                    Key::Int(n) => *n,
                    Key::Str(s) => s.parse::<i64>().map_err(de::Error::custom)?,
                };
                let key = K::try_from(n).map_err(|_| de::Error::custom(format!("key {n} out of range")))?;
                out.insert(key, v);
            }
            Ok(out)
        }
    }
    d.deserialize_map(V2(std::marker::PhantomData))
}

// ---------------------------------------------------------------------------
// Preset -> config
// ---------------------------------------------------------------------------

/// Render a preset into the managed LACT block. Pure; does not validate
/// (see [`crate::validate`]) but fails if the preset cannot be expressed.
pub fn render(preset: &Preset, limits: &GpuLimits) -> Result<LactGpuConfig> {
    let mut cfg = LactGpuConfig { power_cap: preset.power_cap_w, ..Default::default() };

    match &preset.fan {
        FanSpec::Auto => {}
        FanSpec::Static { speed } => {
            cfg.fan_control_enabled = true;
            cfg.fan_control_settings = Some(FanSettings {
                mode: FanMode::Static,
                static_speed: *speed,
                temperature_key: TEMPERATURE_KEY.to_owned(),
                interval_ms: FAN_INTERVAL_MS,
                // LACT insists on a curve even in static mode; a flat one
                // spanning the useful range mirrors the known-good config.
                curve: BTreeMap::from([(40, *speed), (80, *speed)]),
                spindown_delay_ms: Some(0),
                change_threshold: Some(0),
                auto_threshold: Some(0),
            });
        }
        FanSpec::Curve { points, zero_rpm_below_c, hysteresis_c, spindown_delay_ms } => {
            cfg.fan_control_enabled = true;
            cfg.fan_control_settings = Some(FanSettings {
                mode: FanMode::Curve,
                static_speed: points.first().map_or_else(default_static_speed, |p| p.speed),
                temperature_key: TEMPERATURE_KEY.to_owned(),
                interval_ms: FAN_INTERVAL_MS,
                curve: points.iter().map(|p| (p.temp_c, p.speed)).collect(),
                spindown_delay_ms: Some(*spindown_delay_ms),
                change_threshold: Some(u64::from(*hysteresis_c)),
                auto_threshold: Some(u64::from(zero_rpm_below_c.unwrap_or(0))),
            });
        }
    }

    match &preset.vf {
        VfSpec::Stock => {}
        VfSpec::Offsets { points } => {
            for p in points {
                cfg.nvidia_gpu_vf_curve.insert(
                    p.index,
                    VfPointCfg { clockspeed_offset: p.offset_mhz, voltage: p.voltage_mv },
                );
            }
        }
        VfSpec::Flat { offset_mhz } => {
            if limits.vf_points.is_empty() {
                return Err(EngineError::Unsupported(
                    "flat offset needs the driver V/F table, which the backend did not report".into(),
                ));
            }
            for p in &limits.vf_points {
                cfg.nvidia_gpu_vf_curve.insert(
                    p.index,
                    VfPointCfg { clockspeed_offset: *offset_mhz, voltage: Some(p.voltage_mv) },
                );
            }
        }
        VfSpec::MaxClock { mhz } => {
            cfg.max_core_clock = Some(i32::try_from(*mhz).map_err(|_| {
                EngineError::Unsupported(format!("max clock {mhz} MHz out of range"))
            })?);
        }
    }
    Ok(cfg)
}

// ---------------------------------------------------------------------------
// Config -> settings (reverse), comparison, diff
// ---------------------------------------------------------------------------

impl LactGpuConfig {
    /// Express the applied config in preset vocabulary.
    pub fn to_settings(&self) -> Result<AppliedSettings> {
        let fan = match (&self.fan_control_enabled, &self.fan_control_settings) {
            (true, Some(s)) => match s.mode {
                FanMode::Static => FanSpec::Static { speed: s.static_speed },
                FanMode::Curve => FanSpec::Curve {
                    points: s.curve.iter().map(|(t, v)| CurvePoint { temp_c: *t, speed: *v }).collect(),
                    zero_rpm_below_c: s.auto_threshold.filter(|t| *t > 0).map(|t| t as u32),
                    hysteresis_c: s.change_threshold.unwrap_or(0) as u32,
                    spindown_delay_ms: s.spindown_delay_ms.unwrap_or(0),
                },
            },
            _ => FanSpec::Auto,
        };
        let vf = match (self.nvidia_gpu_vf_curve.is_empty(), self.max_core_clock) {
            (true, None) => VfSpec::Stock,
            (true, Some(mhz)) => VfSpec::MaxClock { mhz: mhz.max(0) as u32 },
            (false, None) => VfSpec::Offsets {
                points: self
                    .nvidia_gpu_vf_curve
                    .iter()
                    .map(|(i, p)| VfOffset { index: *i, voltage_mv: p.voltage, offset_mhz: p.clockspeed_offset })
                    .collect(),
            },
            (false, Some(_)) => {
                return Err(EngineError::Unsupported(
                    "config combines a V/F curve with a max clock cap; presets cannot express both".into(),
                ))
            }
        };
        Ok(AppliedSettings { power_cap_w: self.power_cap, fan, vf })
    }

    /// Equality that ignores float noise and settings LACT treats as defaults.
    /// A power cap within 0.5 W of `default_power_w` equals "no override".
    pub fn same_as(&self, other: &Self, default_power_w: f64) -> bool {
        power_eq(self.power_cap, other.power_cap, default_power_w)
            && fan_eq(self, other)
            && self.nvidia_gpu_vf_curve == other.nvidia_gpu_vf_curve
            && self.max_core_clock == other.max_core_clock
    }

    /// Human-readable differences between `self` (applied) and `target`.
    pub fn changes_to(&self, target: &Self, default_power_w: f64) -> Vec<Change> {
        let mut out = Vec::new();
        if !power_eq(self.power_cap, target.power_cap, default_power_w) {
            out.push(Change {
                field: "power_cap".into(),
                current: power_summary(self.power_cap),
                target: power_summary(target.power_cap),
            });
        }
        if !fan_eq(self, target) {
            out.push(Change {
                field: "fan".into(),
                current: self.fan_summary(),
                target: target.fan_summary(),
            });
        }
        if self.nvidia_gpu_vf_curve != target.nvidia_gpu_vf_curve || self.max_core_clock != target.max_core_clock {
            let mut t = target.vf_summary();
            if !self.nvidia_gpu_vf_curve.is_empty() && !target.nvidia_gpu_vf_curve.is_empty() {
                let keys: std::collections::BTreeSet<_> = self
                    .nvidia_gpu_vf_curve
                    .keys()
                    .chain(target.nvidia_gpu_vf_curve.keys())
                    .collect();
                let n = keys
                    .iter()
                    .filter(|k| self.nvidia_gpu_vf_curve.get(**k) != target.nvidia_gpu_vf_curve.get(**k))
                    .count();
                t = format!("{t} ({n} points differ)");
            }
            out.push(Change { field: "vf".into(), current: self.vf_summary(), target: t });
        }
        out
    }

    fn fan_summary(&self) -> String {
        self.to_settings().map_or_else(|_| "?".into(), |s| s.fan.summary())
    }

    fn vf_summary(&self) -> String {
        match self.to_settings() {
            Ok(s) => s.vf.summary(),
            Err(_) => format!("{} points + max clock", self.nvidia_gpu_vf_curve.len()),
        }
    }
}

fn power_eq(a: Option<f64>, b: Option<f64>, default_w: f64) -> bool {
    let norm = |v: Option<f64>| v.filter(|w| (w - default_w).abs() > POWER_EPS_W);
    match (norm(a), norm(b)) {
        (None, None) => true,
        (Some(x), Some(y)) => (x - y).abs() <= POWER_EPS_W,
        _ => false,
    }
}

fn fan_eq(a: &LactGpuConfig, b: &LactGpuConfig) -> bool {
    let a_on = a.fan_control_enabled && a.fan_control_settings.is_some();
    let b_on = b.fan_control_enabled && b.fan_control_settings.is_some();
    match (a_on, b_on) {
        (false, false) => true,
        (true, true) => {
            let (x, y) = (a.fan_control_settings.as_ref().unwrap(), b.fan_control_settings.as_ref().unwrap());
            if x.mode != y.mode {
                return false;
            }
            match x.mode {
                FanMode::Static => (x.static_speed - y.static_speed).abs() <= SPEED_EPS,
                FanMode::Curve => {
                    x.curve.len() == y.curve.len()
                        && x.curve.iter().zip(&y.curve).all(|((ta, sa), (tb, sb))| ta == tb && (sa - sb).abs() <= SPEED_EPS)
                        && x.auto_threshold.unwrap_or(0) == y.auto_threshold.unwrap_or(0)
                        && x.change_threshold.unwrap_or(0) == y.change_threshold.unwrap_or(0)
                        && x.spindown_delay_ms.unwrap_or(0) == y.spindown_delay_ms.unwrap_or(0)
                }
            }
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Overlay / merge
// ---------------------------------------------------------------------------

/// Overlay the managed keys onto a JSON per-GPU block (daemon wire format),
/// leaving every other key as-is. Managed keys that the target leaves unset are
/// removed so a reset really clears them.
pub fn overlay_json(block: &mut serde_json::Map<String, serde_json::Value>, cfg: &LactGpuConfig) -> Result<()> {
    let rendered = serde_json::to_value(cfg).map_err(|e| EngineError::Config(e.to_string()))?;
    let rendered = rendered.as_object().cloned().unwrap_or_default();
    for key in MANAGED_KEYS {
        match rendered.get(key) {
            Some(v) => {
                block.insert(key.to_owned(), v.clone());
            }
            None => {
                block.remove(key);
            }
        }
    }
    Ok(())
}

/// Same as [`overlay_json`] for a YAML block (integer keys preserved).
pub fn overlay_yaml(block: &mut Mapping, cfg: &LactGpuConfig) -> Result<()> {
    let rendered = serde_norway::to_value(cfg).map_err(|e| EngineError::Config(e.to_string()))?;
    let rendered = rendered.as_mapping().cloned().unwrap_or_default();
    for key in MANAGED_KEYS {
        let k = Yaml::String(key.to_owned());
        match rendered.get(&k) {
            Some(v) => {
                block.insert(k, v.clone());
            }
            None => {
                block.remove(&k);
            }
        }
    }
    Ok(())
}

/// Extract the managed subset from a JSON per-GPU block.
pub fn extract_json(block: &serde_json::Map<String, serde_json::Value>) -> Result<LactGpuConfig> {
    let subset: serde_json::Map<_, _> = block
        .iter()
        .filter(|(k, _)| MANAGED_KEYS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    serde_json::from_value(serde_json::Value::Object(subset))
        .map_err(|e| EngineError::Config(format!("per-GPU config: {e}")))
}

/// Extract the managed subset from a YAML per-GPU block.
pub fn extract_yaml(block: &Mapping) -> Result<LactGpuConfig> {
    let mut subset = Mapping::new();
    for key in MANAGED_KEYS {
        let k = Yaml::String(key.to_owned());
        if let Some(v) = block.get(&k) {
            subset.insert(k, v.clone());
        }
    }
    serde_norway::from_value(Yaml::Mapping(subset)).map_err(|e| EngineError::Config(format!("per-GPU config: {e}")))
}

fn gpus_mapping<'a>(doc: &'a mut Mapping, create: bool) -> Result<&'a mut Mapping> {
    // Active LACT profile? Then the live per-GPU settings live inside it.
    let profile = doc
        .get(Yaml::String("current_profile".into()))
        .and_then(Yaml::as_str)
        .map(str::to_owned);
    let root: &mut Mapping = match profile {
        Some(name) => {
            let p = doc
                .get_mut(Yaml::String("profiles".into()))
                .and_then(Yaml::as_mapping_mut)
                .and_then(|m| m.get_mut(Yaml::String(name.clone())))
                .and_then(Yaml::as_mapping_mut)
                .ok_or_else(|| EngineError::Config(format!("current_profile '{name}' not found under profiles")))?;
            p
        }
        None => doc,
    };
    let key = Yaml::String("gpus".into());
    if matches!(root.get(&key), Some(Yaml::Null)) || (create && !root.contains_key(&key)) {
        root.insert(key.clone(), Yaml::Mapping(Mapping::new()));
    }
    root.get_mut(&key)
        .and_then(Yaml::as_mapping_mut)
        .ok_or_else(|| EngineError::Config("no `gpus` map".into()))
}

/// Read the managed settings of `gpu_id` from a config file's text.
/// A GPU without an entry yields the default (all overrides off).
pub fn read_gpu_from_yaml(text: &str, gpu_id: &str) -> Result<LactGpuConfig> {
    let mut doc = parse_doc(text)?;
    let gpus = match gpus_mapping(&mut doc, false) {
        Ok(g) => g,
        Err(_) => return Ok(LactGpuConfig::default()),
    };
    match gpus.get(Yaml::String(gpu_id.to_owned())) {
        Some(Yaml::Mapping(m)) => extract_yaml(m),
        _ => Ok(LactGpuConfig::default()),
    }
}

/// GPU ids that have an entry in the (active) `gpus` section of a config file.
pub fn gpu_ids_in_yaml(text: &str) -> Result<Vec<String>> {
    let mut doc = parse_doc(text)?;
    Ok(gpus_mapping(&mut doc, false)
        .map(|g| g.keys().filter_map(|k| k.as_str().map(str::to_owned)).collect())
        .unwrap_or_default())
}

fn parse_doc(text: &str) -> Result<Mapping> {
    if text.trim().is_empty() {
        return Ok(Mapping::new());
    }
    match serde_norway::from_str::<Yaml>(text).map_err(|e| EngineError::Config(e.to_string()))? {
        Yaml::Mapping(m) => Ok(m),
        Yaml::Null => Ok(Mapping::new()),
        _ => Err(EngineError::Config("top level is not a map".into())),
    }
}

/// Merge `cfg` into the existing LACT config text for `gpu_id` and return the
/// full new document. Unrelated keys, other GPUs and profiles are preserved.
pub fn merge_into_config(existing: &str, gpu_id: &str, cfg: &LactGpuConfig) -> Result<String> {
    let mut doc = parse_doc(existing)?;
    {
        let gpus = gpus_mapping(&mut doc, true)?;
        let key = Yaml::String(gpu_id.to_owned());
        let mut block = match gpus.get(&key) {
            Some(Yaml::Mapping(m)) => m.clone(),
            _ => Mapping::new(),
        };
        overlay_yaml(&mut block, cfg)?;
        gpus.insert(key, Yaml::Mapping(block));
    }
    serde_norway::to_string(&Yaml::Mapping(doc)).map_err(|e| EngineError::Config(e.to_string()))
}

/// The managed block as YAML for display; V/F points are collapsed to a count
/// when `elide_vf` is set.
pub fn render_block_yaml(cfg: &LactGpuConfig, elide_vf: bool) -> Result<String> {
    let mut v = serde_norway::to_value(cfg).map_err(|e| EngineError::Config(e.to_string()))?;
    if elide_vf {
        if let Some(m) = v.as_mapping_mut() {
            let k = Yaml::String("nvidia_gpu_vf_curve".into());
            if let Some(Yaml::Mapping(curve)) = m.get(&k) {
                let n = curve.len();
                m.insert(k, Yaml::String(format!("<{n} points>")));
            }
        }
    }
    serde_norway::to_string(&v).map_err(|e| EngineError::Config(e.to_string()))
}
