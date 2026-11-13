//! In-memory backend for tests and UI development. Never touches the system.

use std::path::PathBuf;
use std::sync::Mutex;

use crate::backend::{ApplyStatus, Backend};
use crate::error::{EngineError, Result};
use crate::lact_config::LactGpuConfig;
use crate::types::*;

pub struct MockBackend {
    pub info: GpuInfo,
    pub limits: GpuLimits,
    pub path: ApplyPath,
    pub config_file: Option<PathBuf>,
    stats: Mutex<Stats>,
    applied: Mutex<LactGpuConfig>,
    history: Mutex<Vec<LactGpuConfig>>,
    fail_apply: Mutex<Option<EngineError>>,
}

impl MockBackend {
    /// A mock modelled on the RTX 3080 Ti this project targets.
    pub fn rtx3080ti() -> Self {
        Self {
            info: GpuInfo {
                id: "MOCK:0000".into(),
                name: "Mock RTX 3080 Ti".into(),
                subsystem: None,
                driver: Some("mock".into()),
                vbios: None,
                family: Some("Ampere".into()),
                vram_total_mb: Some(12288),
                capabilities: vec!["ConfigurableFanControl".into(), "AutoFanThreshold".into()],
            },
            limits: GpuLimits {
                power_min_w: 100.0,
                power_max_w: 365.0,
                power_default_w: 350.0,
                fan_min: 77.0 / 255.0,
                clock_min_mhz: 210,
                clock_max_mhz: 2100,
                offset_min_mhz: -1000,
                offset_max_mhz: 1000,
                vf_points: (0..127)
                    .map(|i| VfPointInfo { index: i, voltage_mv: 450 + i * 6, base_freq_mhz: 210 + i * 14 })
                    .collect(),
            },
            path: ApplyPath::DaemonSocket,
            config_file: None,
            stats: Mutex::new(Stats { temp_c: Some(42.0), fan_pct: Some(30.0), power_draw_w: Some(35.0), ..Stats::default() }),
            applied: Mutex::new(LactGpuConfig::default()),
            history: Mutex::new(Vec::new()),
            fail_apply: Mutex::new(None),
        }
    }

    pub fn set_stats(&self, s: Stats) {
        *self.stats.lock().unwrap() = s;
    }

    pub fn set_applied(&self, c: LactGpuConfig) {
        *self.applied.lock().unwrap() = c;
    }

    /// Make the next `apply` fail with `err`.
    pub fn fail_next_apply(&self, err: EngineError) {
        *self.fail_apply.lock().unwrap() = Some(err);
    }

    /// Every config passed to a successful `apply`, oldest first.
    pub fn applied_history(&self) -> Vec<LactGpuConfig> {
        self.history.lock().unwrap().clone()
    }
}

impl Backend for MockBackend {
    fn gpu_id(&self) -> &str {
        &self.info.id
    }
    fn gpu_info(&self) -> Result<GpuInfo> {
        Ok(self.info.clone())
    }
    fn limits(&self) -> Result<GpuLimits> {
        Ok(self.limits.clone())
    }
    fn stats(&self) -> Result<Stats> {
        Ok(self.stats.lock().unwrap().clone())
    }
    fn applied(&self) -> Result<LactGpuConfig> {
        Ok(self.applied.lock().unwrap().clone())
    }
    fn apply_path(&self) -> ApplyPath {
        self.path
    }
    fn config_file(&self) -> Option<PathBuf> {
        self.config_file.clone()
    }
    fn apply(&self, target: &LactGpuConfig) -> Result<ApplyStatus> {
        if let Some(e) = self.fail_apply.lock().unwrap().take() {
            return Err(e);
        }
        match self.path {
            ApplyPath::DaemonSocket => {
                *self.applied.lock().unwrap() = target.clone();
                self.history.lock().unwrap().push(target.clone());
                Ok(ApplyStatus::Applied { verified: true, notes: Vec::new() })
            }
            ApplyPath::PrivilegedInstall => {
                Ok(ApplyStatus::PrivilegeRequired(PrivilegedWrite::install("/tmp/mock-pending.yaml", "/etc/lact/config.yaml")))
            }
        }
    }
}
