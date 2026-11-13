//! Real backend: talks to the `lactd` daemon over its unix socket.
//!
//! Wire format (from `lact-schema`): one JSON object per line,
//! `{"command": "<snake_case>", "args": {...}}`, answered by one line
//! `{"status": "ok", "data": ...}` or `{"status": "error", "data": {...}}`.
//! Reads (`list_devices`, `device_info`, `device_stats`, `device_clocks_info`,
//! `get_gpu_config`) are side-effect free. Writes go through
//! `set_gpu_config` followed by `confirm_pending_config`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::backend::{ApplyStatus, Backend};
use crate::backup::now_stamp;
use crate::error::{EngineError, Result};
use crate::lact_config::{self, LactGpuConfig};
use crate::types::*;

pub const DEFAULT_SOCKET: &str = "/run/lactd.sock";
pub const DEFAULT_CONFIG: &str = "/etc/lact/config.yaml";
const IO_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct LactOptions {
    pub socket: PathBuf,
    pub config_path: PathBuf,
    /// Where a rendered config is staged when a privileged install is needed.
    pub pending_dir: PathBuf,
    /// Pick this GPU instead of the first NVIDIA one.
    pub gpu_id: Option<String>,
    /// `nvidia-smi` binary, used for read-only stats when the daemon socket
    /// is not usable.
    pub nvidia_smi: PathBuf,
}

impl LactOptions {
    /// Defaults, overridable with `NVPROFILE_LACT_SOCKET` / `NVPROFILE_LACT_CONFIG`
    /// (used by tests and sandboxes).
    pub fn from_env(pending_dir: PathBuf) -> Self {
        let var = |k: &str, d: &str| std::env::var_os(k).map_or_else(|| PathBuf::from(d), PathBuf::from);
        Self {
            socket: var("NVPROFILE_LACT_SOCKET", DEFAULT_SOCKET),
            config_path: var("NVPROFILE_LACT_CONFIG", DEFAULT_CONFIG),
            pending_dir,
            gpu_id: None,
            nvidia_smi: var("NVPROFILE_NVIDIA_SMI", "nvidia-smi"),
        }
    }
}

pub struct LactBackend {
    opts: LactOptions,
    id: String,
    name: String,
    /// False when the socket was unusable at connect time: reads fall back to
    /// `nvidia-smi` + the config file, writes need a privileged install.
    daemon: bool,
}

impl LactBackend {
    /// Connect to the daemon and pick the GPU to manage. If the socket cannot
    /// be used (daemon down, user not in its admin group) the backend runs in
    /// degraded mode: `nvidia-smi` for reads, privileged install for writes.
    pub fn connect(opts: LactOptions) -> Result<Self> {
        match call(&opts.socket, "list_devices", None) {
            Ok(devices) => Self::from_devices(opts, &devices),
            Err(EngineError::DaemonUnavailable(why)) => Self::degraded(opts, &why),
            Err(e) => Err(e),
        }
    }

    fn from_devices(opts: LactOptions, devices: &Value) -> Result<Self> {
        let devices = devices.as_array().ok_or_else(|| EngineError::Protocol("list_devices: not an array".into()))?;
        let pick = devices
            .iter()
            .find(|d| match &opts.gpu_id {
                Some(id) => d["id"].as_str() == Some(id),
                None => d["name"].as_str().is_some_and(|n| n.to_ascii_uppercase().contains("NVIDIA")),
            })
            .ok_or_else(|| {
                EngineError::NoGpu(match &opts.gpu_id {
                    Some(id) => format!("LACT does not list a GPU with id {id}"),
                    None => "LACT lists no NVIDIA GPU".into(),
                })
            })?;
        let id = pick["id"].as_str().ok_or_else(|| EngineError::Protocol("device without id".into()))?.to_owned();
        let name = pick["name"].as_str().unwrap_or("GPU").to_owned();
        Ok(Self { opts, id, name, daemon: true })
    }

    fn degraded(opts: LactOptions, why: &str) -> Result<Self> {
        let id = match &opts.gpu_id {
            Some(id) => id.clone(),
            None => {
                // Without the daemon the only source of a GPU id is the config file.
                let text = std::fs::read_to_string(&opts.config_path)
                    .map_err(|_| EngineError::DaemonUnavailable(why.to_owned()))?;
                let ids = lact_config::gpu_ids_in_yaml(&text)?;
                match ids.as_slice() {
                    [one] => one.clone(),
                    _ => {
                        return Err(EngineError::DaemonUnavailable(format!(
                            "{why}; and the GPU cannot be inferred from {} ({} candidates)",
                            opts.config_path.display(),
                            ids.len()
                        )))
                    }
                }
            }
        };
        let name = smi_query(&opts.nvidia_smi, &["name"]).ok().and_then(|v| v.into_iter().next()).unwrap_or_else(|| "NVIDIA GPU".into());
        Ok(Self { opts, id, name, daemon: false })
    }

    /// All GPUs the daemon knows: `(id, name)`.
    pub fn list_gpus(opts: &LactOptions) -> Result<Vec<(String, String)>> {
        let v = call(&opts.socket, "list_devices", None)?;
        Ok(v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|d| Some((d["id"].as_str()?.to_owned(), d["name"].as_str()?.to_owned())))
                    .collect()
            })
            .unwrap_or_default())
    }

    fn call(&self, command: &str, extra: Option<Value>) -> Result<Value> {
        let mut args = Map::new();
        args.insert("id".into(), json!(self.id));
        if let Some(Value::Object(m)) = extra {
            args.extend(m);
        }
        call(&self.opts.socket, command, Some(Value::Object(args)))
    }

    /// Current daemon-side per-GPU block as raw JSON (empty when unset).
    fn daemon_block(&self) -> Result<Map<String, Value>> {
        match self.call("get_gpu_config", None)? {
            Value::Object(m) => Ok(m),
            Value::Null => Ok(Map::new()),
            other => Err(EngineError::Protocol(format!("get_gpu_config: unexpected {other}"))),
        }
    }

    fn apply_via_socket(&self, target: &LactGpuConfig) -> Result<ApplyStatus> {
        let mut block = self.daemon_block()?;
        lact_config::overlay_json(&mut block, target)?;
        let timer = self
            .call("set_gpu_config", Some(json!({ "config": block })))?
            .as_u64()
            .unwrap_or(5);
        // The daemon applies the config but only persists it once confirmed;
        // otherwise it reverts after `apply_settings_timer` seconds.
        if let Err(e) = call(&self.opts.socket, "confirm_pending_config", Some(json!({ "command": "confirm" }))) {
            return Err(EngineError::Daemon(format!(
                "settings were applied but confirmation failed ({e}); the daemon reverts them in ~{timer}s"
            )));
        }

        let mut notes = Vec::new();
        let after = lact_config::extract_json(&self.daemon_block()?)?;
        // An infinite default disables the "equals driver default" shortcut: compare caps literally.
        let mut verified = after.same_as(target, f64::INFINITY);
        if !verified {
            notes.push("daemon config after apply differs from the requested config".into());
        }
        if let (Some(want), Ok(stats)) = (target.power_cap, self.stats()) {
            if let Some(got) = stats.power_cap_w {
                if (got - want).abs() > 0.5 {
                    verified = false;
                    notes.push(format!("GPU reports power cap {got:.1} W, expected {want:.1} W"));
                }
            }
        }
        Ok(ApplyStatus::Applied { verified, notes })
    }

    fn prepare_privileged(&self, target: &LactGpuConfig) -> Result<ApplyStatus> {
        let path = &self.opts.config_path;
        let existing = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(EngineError::io(path, &e)),
        };
        let merged = lact_config::merge_into_config(&existing, &self.id, target)?;
        std::fs::create_dir_all(&self.opts.pending_dir).map_err(|e| EngineError::io(&self.opts.pending_dir, &e))?;
        let src = self.opts.pending_dir.join(format!("config-{}.yaml", now_stamp()));
        std::fs::write(&src, merged).map_err(|e| EngineError::io(&src, &e))?;
        Ok(ApplyStatus::PrivilegeRequired(PrivilegedWrite::install(
            &src.display().to_string(),
            &path.display().to_string(),
        )))
    }
}

impl Backend for LactBackend {
    fn gpu_id(&self) -> &str {
        &self.id
    }

    fn gpu_info(&self) -> Result<GpuInfo> {
        if !self.daemon {
            let f = smi_query(&self.opts.nvidia_smi, &["name", "driver_version", "vbios_version", "memory.total"])?;
            return Ok(GpuInfo {
                id: self.id.clone(),
                name: f.first().cloned().unwrap_or_else(|| self.name.clone()),
                subsystem: None,
                driver: f.get(1).cloned(),
                vbios: f.get(2).cloned(),
                family: None,
                vram_total_mb: f.get(3).and_then(|v| v.parse().ok()),
                capabilities: Vec::new(),
            });
        }
        let info = self.call("device_info", None)?;
        let stats = self.call("device_stats", None)?;
        Ok(GpuInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            subsystem: info["pci_info"]["subsystem_pci_info"]["model"].as_str().map(str::to_owned),
            driver: info["driver"].as_str().map(str::to_owned),
            vbios: info["vbios_version"].as_str().map(str::to_owned),
            family: info["drm_info"]["family_name"].as_str().map(str::to_owned),
            vram_total_mb: stats["vram"]["total"].as_u64().map(|b| b / (1024 * 1024)),
            capabilities: info["flags"]
                .as_array()
                .map(|a| a.iter().filter_map(|f| f.as_str().map(str::to_owned)).collect())
                .unwrap_or_default(),
        })
    }

    fn limits(&self) -> Result<GpuLimits> {
        if !self.daemon {
            let f = smi_query(&self.opts.nvidia_smi, &["power.min_limit", "power.max_limit", "power.default_limit"])?;
            let num = |i: usize| {
                f.get(i)
                    .and_then(|v| v.parse::<f64>().ok())
                    .ok_or_else(|| EngineError::Protocol("nvidia-smi: missing power limits".into()))
            };
            // Without the daemon the fan floor, clock range and V/F table are
            // unknown: use the conservative NVIDIA defaults and no table.
            return Ok(GpuLimits {
                power_min_w: num(0)?,
                power_max_w: num(1)?,
                power_default_w: num(2)?,
                fan_min: 0.30,
                clock_min_mhz: 210,
                clock_max_mhz: 3000,
                offset_min_mhz: -1000,
                offset_max_mhz: 1000,
                vf_points: Vec::new(),
            });
        }
        let stats = self.call("device_stats", None)?;
        let clocks = self.call("device_clocks_info", None)?;
        let power = &stats["power"];
        let fan = &stats["fan"];
        let table = &clocks["table"]["value"];
        let fan_min = match (fan["pwm_min"].as_f64(), fan["pwm_max"].as_f64()) {
            (Some(lo), Some(hi)) if hi > 0.0 => lo / hi,
            _ => 0.30,
        };
        let range = |v: &Value| -> Option<(u32, u32)> { Some((v[0].as_u64()? as u32, v[1].as_u64()? as u32)) };
        let (clock_min, clock_max) = range(&table["gpu_clock_range"]).unwrap_or((210, 2100));
        let offsets = table["gpu_offsets"].as_object().and_then(|m| m.values().next());
        let offset_min = offsets.and_then(|o| o["min"].as_i64()).unwrap_or(-1000) as i32;
        let offset_max = offsets.and_then(|o| o["max"].as_i64()).unwrap_or(1000) as i32;
        let vf_points = table["gpu_vf_curve"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|p| {
                        Some(VfPointInfo {
                            index: p["index"].as_u64()? as u32,
                            voltage_mv: p["voltage"].as_u64()? as u32,
                            base_freq_mhz: p["base_freq"].as_u64().unwrap_or(0) as u32,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(GpuLimits {
            power_min_w: power["cap_min"].as_f64().ok_or_else(|| EngineError::Protocol("no power cap_min".into()))?,
            power_max_w: power["cap_max"].as_f64().ok_or_else(|| EngineError::Protocol("no power cap_max".into()))?,
            power_default_w: power["cap_default"]
                .as_f64()
                .ok_or_else(|| EngineError::Protocol("no power cap_default".into()))?,
            fan_min,
            clock_min_mhz: clock_min,
            clock_max_mhz: clock_max,
            offset_min_mhz: offset_min,
            offset_max_mhz: offset_max,
            vf_points,
        })
    }

    fn stats(&self) -> Result<Stats> {
        if !self.daemon {
            return smi_stats(&self.opts.nvidia_smi);
        }
        let s = self.call("device_stats", None)?;
        let temp = |keys: &[&str]| keys.iter().find_map(|k| s["temps"][*k]["current"].as_f64());
        let mb = |v: &Value| v.as_u64().map(|b| b / (1024 * 1024));
        Ok(Stats {
            temp_c: temp(&["GPU", "edge"]),
            hotspot_c: temp(&["GPU Hotspot", "hotspot", "junction"]),
            vram_temp_c: temp(&["VRAM", "mem"]),
            fan_pct: s["fan"]["pwm_current"].as_f64().map(|p| p / 255.0 * 100.0),
            fan_rpm: s["fan"]["speed_current"].as_u64().map(|v| v as u32),
            power_draw_w: s["power"]["current"].as_f64().or_else(|| s["power"]["average"].as_f64()),
            power_cap_w: s["power"]["cap_current"].as_f64(),
            gpu_clock_mhz: s["clockspeed"]["gpu_clockspeed"].as_u64().map(|v| v as u32),
            mem_clock_mhz: s["clockspeed"]["vram_clockspeed"].as_u64().map(|v| v as u32),
            voltage_mv: s["voltage"]["gpu"].as_u64().map(|v| v as u32),
            util_pct: s["busy_percent"].as_u64().map(|v| v as u32),
            vram_used_mb: mb(&s["vram"]["used"]),
            vram_total_mb: mb(&s["vram"]["total"]),
            throttle: s["throttle_info"].as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default(),
        })
    }

    fn applied(&self) -> Result<LactGpuConfig> {
        if self.daemon {
            return lact_config::extract_json(&self.daemon_block()?);
        }
        let path = &self.opts.config_path;
        let text = std::fs::read_to_string(path).map_err(|e| EngineError::io(path, &e))?;
        lact_config::read_gpu_from_yaml(&text, &self.id)
    }

    fn apply_path(&self) -> ApplyPath {
        if self.daemon {
            ApplyPath::DaemonSocket
        } else {
            ApplyPath::PrivilegedInstall
        }
    }

    fn config_file(&self) -> Option<PathBuf> {
        Some(self.opts.config_path.clone())
    }

    fn apply(&self, target: &LactGpuConfig) -> Result<ApplyStatus> {
        match self.apply_path() {
            ApplyPath::DaemonSocket => self.apply_via_socket(target),
            ApplyPath::PrivilegedInstall => self.prepare_privileged(target),
        }
    }
}

/// One request/response round trip on a fresh connection.
fn call(socket: &std::path::Path, command: &str, args: Option<Value>) -> Result<Value> {
    let unavailable = |e: std::io::Error| EngineError::DaemonUnavailable(format!("{}: {e}", socket.display()));
    let mut stream = UnixStream::connect(socket).map_err(unavailable)?;
    stream.set_read_timeout(Some(IO_TIMEOUT)).map_err(unavailable)?;
    stream.set_write_timeout(Some(IO_TIMEOUT)).map_err(unavailable)?;

    let mut req = json!({ "command": command });
    if let Some(a) = args {
        req["args"] = a;
    }
    let mut line = req.to_string();
    line.push('\n');
    stream.write_all(line.as_bytes()).map_err(unavailable)?;

    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply).map_err(unavailable)?;
    let v: Value = serde_json::from_str(&reply).map_err(|e| EngineError::Protocol(format!("{command}: {e}")))?;
    match v["status"].as_str() {
        Some("ok") => Ok(v["data"].clone()),
        Some("error") => Err(EngineError::Daemon(error_chain(&v["data"]))),
        _ => Err(EngineError::Protocol(format!("{command}: missing status in reply"))),
    }
}

/// Flatten a `serde_error::Error` (`{"description": .., "source": {..}}`).
fn error_chain(v: &Value) -> String {
    let mut parts = Vec::new();
    let mut cur = v;
    while let Some(d) = cur["description"].as_str() {
        parts.push(d.to_owned());
        cur = &cur["source"];
    }
    if parts.is_empty() {
        v.to_string()
    } else {
        parts.join(": ")
    }
}

// ---------------------------------------------------------------------------
// nvidia-smi fallback (read-only)
// ---------------------------------------------------------------------------

fn smi_query(bin: &std::path::Path, fields: &[&str]) -> Result<Vec<String>> {
    let out = std::process::Command::new(bin)
        .arg(format!("--query-gpu={}", fields.join(",")))
        .args(["--format=csv,noheader,nounits"])
        .output()
        .map_err(|e| EngineError::io(bin, &e))?;
    if !out.status.success() {
        return Err(EngineError::Protocol(format!(
            "nvidia-smi failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next().unwrap_or_default();
    Ok(line.split(',').map(|f| f.trim().to_owned()).collect())
}

/// Throttle bitmask bits from `nvidia-smi --help-query-gpu` (clocks_throttle_reasons).
fn throttle_names(mask: u64) -> Vec<String> {
    [
        (0x1, "GPU_IDLE"),
        (0x2, "APP_CLOCKS_SETTING"),
        (0x4, "SW_POWER_CAP"),
        (0x8, "HW_SLOWDOWN"),
        (0x10, "SYNC_BOOST"),
        (0x20, "SW_THERMAL_SLOWDOWN"),
        (0x40, "HW_THERMAL_SLOWDOWN"),
        (0x80, "HW_POWER_BRAKE_SLOWDOWN"),
    ]
    .iter()
    .filter(|(bit, name)| mask & bit != 0 && *name != "GPU_IDLE")
    .map(|(_, n)| (*n).to_owned())
    .collect()
}

fn smi_stats(bin: &std::path::Path) -> Result<Stats> {
    let f = smi_query(
        bin,
        &[
            "temperature.gpu",
            "fan.speed",
            "power.draw",
            "power.limit",
            "clocks.gr",
            "clocks.mem",
            "utilization.gpu",
            "memory.used",
            "memory.total",
            "clocks_throttle_reasons.active",
        ],
    )?;
    let num = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok());
    Ok(Stats {
        temp_c: num(0),
        fan_pct: num(1),
        power_draw_w: num(2),
        power_cap_w: num(3),
        gpu_clock_mhz: num(4).map(|v| v as u32),
        mem_clock_mhz: num(5).map(|v| v as u32),
        util_pct: num(6).map(|v| v as u32),
        vram_used_mb: num(7).map(|v| v as u64),
        vram_total_mb: num(8).map(|v| v as u64),
        throttle: f
            .get(9)
            .and_then(|v| u64::from_str_radix(v.trim_start_matches("0x"), 16).ok())
            .map(throttle_names)
            .unwrap_or_default(),
        ..Stats::default()
    })
}
