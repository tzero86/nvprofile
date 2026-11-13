//! Serializable error type shared by every engine entry point.

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub type Result<T> = std::result::Result<T, EngineError>;

/// Every fallible engine function returns this. It serializes as
/// `{"kind": "...", "detail": ...}` so UI layers can branch on `kind`.
#[derive(Debug, Clone, PartialEq, Error, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum EngineError {
    #[error("I/O error on {path}: {message}")]
    Io { path: String, message: String },
    #[error("LACT daemon unavailable: {0}")]
    DaemonUnavailable(String),
    #[error("LACT daemon error: {0}")]
    Daemon(String),
    #[error("unexpected daemon response: {0}")]
    Protocol(String),
    #[error("no supported GPU found: {0}")]
    NoGpu(String),
    #[error("invalid LACT config: {0}")]
    Config(String),
    #[error("preset '{0}' not found")]
    PresetNotFound(String),
    #[error("preset '{0}' already exists")]
    PresetExists(String),
    #[error("preset '{0}' is built-in and read-only")]
    BuiltinReadOnly(String),
    #[error("invalid preset: {}", .issues.join("; "))]
    Validation { issues: Vec<String> },
    #[error("unsupported: {0}")]
    Unsupported(String),
}

impl EngineError {
    pub(crate) fn io(path: impl AsRef<std::path::Path>, err: &std::io::Error) -> Self {
        EngineError::Io {
            path: path.as_ref().display().to_string(),
            message: err.to_string(),
        }
    }
}
