//! The abstraction between engine logic and whatever talks to the GPU.

use std::path::PathBuf;

use crate::error::Result;
use crate::lact_config::LactGpuConfig;
use crate::types::*;

/// What a backend did with a config.
#[derive(Debug, Clone, PartialEq)]
pub enum ApplyStatus {
    Applied { verified: bool, notes: Vec<String> },
    /// Nothing was changed; the caller must run this with elevated rights.
    PrivilegeRequired(PrivilegedWrite),
}

/// A GPU control backend. [`crate::LactBackend`] is the real one,
/// [`crate::MockBackend`] is for tests and UI development.
///
/// Engine functions take `&dyn Backend`, so a consumer can swap implementations.
pub trait Backend {
    /// Id of the GPU this backend manages.
    fn gpu_id(&self) -> &str;
    fn gpu_info(&self) -> Result<GpuInfo>;
    fn limits(&self) -> Result<GpuLimits>;
    fn stats(&self) -> Result<Stats>;
    /// The managed subset of the currently applied per-GPU config.
    fn applied(&self) -> Result<LactGpuConfig>;
    /// How [`Backend::apply`] would deliver settings right now.
    fn apply_path(&self) -> ApplyPath;
    /// Config file to back up before any write (`None` if there is none).
    fn config_file(&self) -> Option<PathBuf>;
    /// Deliver `target`. Must only be called after validation and backup.
    fn apply(&self, target: &LactGpuConfig) -> Result<ApplyStatus>;
}
