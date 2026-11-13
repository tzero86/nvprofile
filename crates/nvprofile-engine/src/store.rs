//! Built-in + user preset store. User presets are `<id>.yaml` files in
//! `~/.config/nvprofile/presets/` (no root needed).

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::builtin::{builtin_presets, is_builtin_id};
use crate::error::{EngineError, Result};
use crate::types::{GpuLimits, Preset};
use crate::validate::{self, is_valid_id};

/// Filesystem locations used by the engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Dirs {
    pub presets: PathBuf,
    pub backups: PathBuf,
    /// Staging area for configs that need a privileged install.
    pub pending: PathBuf,
}

impl Dirs {
    /// XDG locations: `~/.config/nvprofile/presets`, `~/.local/state/nvprofile/{backups,pending}`.
    pub fn from_env() -> Result<Self> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let xdg = |var: &str, fallback: &str| -> Result<PathBuf> {
            match std::env::var_os(var).filter(|v| !v.is_empty()) {
                Some(v) => Ok(PathBuf::from(v)),
                None => home
                    .clone()
                    .map(|h| h.join(fallback))
                    .ok_or_else(|| EngineError::Config("HOME is not set".into())),
            }
        };
        let config = xdg("XDG_CONFIG_HOME", ".config")?.join("nvprofile");
        let state = xdg("XDG_STATE_HOME", ".local/state")?.join("nvprofile");
        Ok(Self { presets: config.join("presets"), backups: state.join("backups"), pending: state.join("pending") })
    }

    /// Everything under one root (tests, sandboxes).
    pub fn under(root: &Path) -> Self {
        Self { presets: root.join("presets"), backups: root.join("backups"), pending: root.join("pending") }
    }
}

/// Result of loading the catalog: valid presets plus per-file problems.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    pub presets: Vec<Preset>,
    pub problems: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PresetStore {
    dir: PathBuf,
}

impl PresetStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn path_for(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.yaml"))
    }

    /// Built-ins first, then user presets sorted by id. Unreadable or colliding
    /// user files are reported in `problems` instead of failing the whole load.
    pub fn load_all(&self) -> Result<Catalog> {
        let mut presets = builtin_presets();
        let mut problems = Vec::new();
        let rd = match fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Catalog { presets, problems }),
            Err(e) => return Err(EngineError::io(&self.dir, &e)),
        };
        let mut files: Vec<PathBuf> = rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "yaml"))
            .collect();
        files.sort();
        for path in files {
            match Self::read_file(&path) {
                Ok(p) if is_builtin_id(&p.id) => {
                    problems.push(format!("{}: id '{}' is reserved for a built-in preset", path.display(), p.id));
                }
                Ok(p) if presets.iter().any(|q| q.id == p.id) => {
                    problems.push(format!("{}: duplicate id '{}'", path.display(), p.id));
                }
                Ok(p) => presets.push(p),
                Err(e) => problems.push(format!("{}: {e}", path.display())),
            }
        }
        Ok(Catalog { presets, problems })
    }

    fn read_file(path: &Path) -> Result<Preset> {
        let text = fs::read_to_string(path).map_err(|e| EngineError::io(path, &e))?;
        let mut p: Preset = serde_norway::from_str(&text).map_err(|e| EngineError::Config(e.to_string()))?;
        p.builtin = false;
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
        if p.id != stem {
            return Err(EngineError::Config(format!("id '{}' does not match file name '{stem}'", p.id)));
        }
        if !is_valid_id(&p.id) {
            return Err(EngineError::Config(format!("invalid id '{}'", p.id)));
        }
        Ok(p)
    }

    pub fn get(&self, id: &str) -> Result<Preset> {
        self.load_all()?
            .presets
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| EngineError::PresetNotFound(id.to_owned()))
    }

    /// Validate against `limits` and write `<id>.yaml`. Fails for built-in ids;
    /// existing user presets are replaced only when `overwrite` is set.
    pub fn save(&self, preset: &Preset, limits: &GpuLimits, overwrite: bool) -> Result<PathBuf> {
        if is_builtin_id(&preset.id) {
            return Err(EngineError::BuiltinReadOnly(preset.id.clone()));
        }
        validate::validate(preset, limits)?;
        let path = self.path_for(&preset.id);
        if path.exists() && !overwrite {
            return Err(EngineError::PresetExists(preset.id.clone()));
        }
        fs::create_dir_all(&self.dir).map_err(|e| EngineError::io(&self.dir, &e))?;
        let mut stored = preset.clone();
        stored.builtin = false;
        let text = serde_norway::to_string(&stored).map_err(|e| EngineError::Config(e.to_string()))?;
        let tmp = self.dir.join(format!(".{}.yaml.tmp", preset.id));
        fs::write(&tmp, text).map_err(|e| EngineError::io(&tmp, &e))?;
        fs::rename(&tmp, &path).map_err(|e| EngineError::io(&path, &e))?;
        Ok(path)
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        if is_builtin_id(id) {
            return Err(EngineError::BuiltinReadOnly(id.to_owned()));
        }
        if !is_valid_id(id) {
            return Err(EngineError::PresetNotFound(id.to_owned()));
        }
        let path = self.path_for(id);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(EngineError::PresetNotFound(id.to_owned())),
            Err(e) => Err(EngineError::io(&path, &e)),
        }
    }
}
