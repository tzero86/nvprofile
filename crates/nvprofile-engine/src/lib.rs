//! # nvprofile-engine
//!
//! UI-agnostic engine for managing NVIDIA GPU power / fan / V-F profiles through
//! the [LACT](https://github.com/ilya-zlobintsev/LACT) daemon. It contains all
//! logic and has no terminal or UI dependency; every public type is
//! `serde`-serializable so a Tauri or web layer can ship them as JSON.
//!
//! See `README.md` for the API tour and the apply path.

pub mod backend;
pub mod backup;
pub mod builtin;
pub mod engine;
pub mod error;
pub mod lact;
pub mod lact_config;
pub mod mock;
pub mod store;
pub mod types;
pub mod validate;

pub use backend::{ApplyStatus, Backend};
pub use builtin::{builtin_presets, is_builtin_id};
pub use engine::{apply_preset, cleanup_pending, current_as_preset, detect_active, diff, plan_apply, privileged_argv, reset, status};
pub use error::{EngineError, Result};
pub use lact::{LactBackend, LactOptions};
pub use lact_config::LactGpuConfig;
pub use mock::MockBackend;
pub use store::{Catalog, Dirs, PresetStore};
pub use types::*;
pub use validate::{is_valid_id, slugify};
