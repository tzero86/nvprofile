//! High-level operations. Every function takes `&dyn Backend`.

use std::path::Path;

use crate::backend::{ApplyStatus, Backend};
use crate::backup;
use crate::builtin::builtin_presets;
use crate::error::{EngineError, Result};
use crate::lact_config::{self, LactGpuConfig};
use crate::types::*;
use crate::validate;

/// Work out which preset (if any) the applied config corresponds to.
pub fn detect_active(applied: &LactGpuConfig, limits: &GpuLimits, presets: &[Preset]) -> Result<ActiveState> {
    let settings = applied.to_settings()?;
    let hit = presets.iter().find(|p| {
        lact_config::render(p, limits).is_ok_and(|r| applied.same_as(&r, limits.power_default_w))
    });
    Ok(ActiveState {
        preset_id: hit.map(|p| p.id.clone()),
        preset_name: hit.map(|p| p.name.clone()),
        applied: settings,
    })
}

/// Info + limits + live stats + active preset in one call.
pub fn status(backend: &dyn Backend, presets: &[Preset]) -> Result<Status> {
    let limits = backend.limits()?;
    let active = detect_active(&backend.applied()?, &limits, presets)?;
    Ok(Status {
        gpu: backend.gpu_info()?,
        stats: backend.stats()?,
        limits,
        active,
        apply_path: backend.apply_path(),
    })
}

/// Differences between what is applied and `preset`.
pub fn diff(backend: &dyn Backend, preset: &Preset) -> Result<Diff> {
    let limits = backend.limits()?;
    let target = lact_config::render(preset, &limits)?;
    let changes = backend.applied()?.changes_to(&target, limits.power_default_w);
    Ok(Diff { preset_id: preset.id.clone(), matches: changes.is_empty(), changes })
}

/// Everything `apply_preset` would do, without doing it.
pub fn plan_apply(backend: &dyn Backend, preset: &Preset) -> Result<ApplyPlan> {
    let limits = backend.limits()?;
    validate::validate(preset, &limits)?;
    let target = lact_config::render(preset, &limits)?;
    let changes = backend.applied()?.changes_to(&target, limits.power_default_w);
    let path = backend.apply_path();
    let mut warnings = Vec::new();
    if path == ApplyPath::PrivilegedInstall {
        warnings.push("the LACT socket is not usable; applying needs root (sudo install)".into());
    }
    let mut config_block = lact_config::render_block_yaml(&target, true)?;
    let rendered = serde_norway::to_value(&target).map_err(|e| EngineError::Config(e.to_string()))?;
    let cleared: Vec<&str> = lact_config::MANAGED_KEYS
        .iter()
        .copied()
        .filter(|k| rendered.get(*k).is_none())
        .collect();
    if !cleared.is_empty() {
        config_block.push_str(&format!("# removed from the GPU entry: {}\n", cleared.join(", ")));
    }
    Ok(ApplyPlan { preset_id: preset.id.clone(), path, changes, config_block, warnings })
}

/// Validate, back up the LACT config, then apply `preset`.
///
/// With `dry_run` nothing is written anywhere. When the outcome is
/// [`ApplyOutcome::PrivilegeRequired`] the front-end must run the returned
/// command; the engine never elevates by itself.
pub fn apply_preset(backend: &dyn Backend, dirs: &crate::store::Dirs, preset: &Preset, dry_run: bool) -> Result<ApplyResult> {
    let plan = plan_apply(backend, preset)?;
    if dry_run {
        return Ok(ApplyResult { plan, outcome: ApplyOutcome::DryRun });
    }
    if plan.changes.is_empty() {
        return Ok(ApplyResult { plan, outcome: ApplyOutcome::NoChange });
    }
    let limits = backend.limits()?;
    let target = lact_config::render(preset, &limits)?;
    let backup = match backend.config_file().filter(|p| p.exists()) {
        Some(src) => Some(backup::backup_file(&src, &dirs.backups)?.display().to_string()),
        None => None,
    };
    let outcome = match backend.apply(&target)? {
        ApplyStatus::Applied { verified, notes } => ApplyOutcome::Applied { verified, notes, backup },
        ApplyStatus::PrivilegeRequired(write) => ApplyOutcome::PrivilegeRequired { write, backup },
    };
    Ok(ApplyResult { plan, outcome })
}

/// Reset to stock (apply the built-in `stock` preset).
pub fn reset(backend: &dyn Backend, dirs: &crate::store::Dirs, dry_run: bool) -> Result<ApplyResult> {
    let stock = builtin_presets().into_iter().find(|p| p.id == "stock").expect("stock preset exists");
    apply_preset(backend, dirs, &stock, dry_run)
}

/// Capture the currently applied settings as a (not yet saved) preset.
pub fn current_as_preset(backend: &dyn Backend, name: &str, description: &str) -> Result<Preset> {
    let id = validate::slugify(name);
    if id.is_empty() {
        return Err(EngineError::Validation { issues: vec![format!("name '{name}' has no usable characters")] });
    }
    let s = backend.applied()?.to_settings()?;
    Ok(Preset {
        id,
        name: name.trim().to_owned(),
        description: description.to_owned(),
        power_cap_w: s.power_cap_w,
        fan: s.fan,
        vf: s.vf,
        builtin: false,
    })
}

/// Run the privileged install a [`PrivilegedWrite`] describes and nothing else;
/// handy for front-ends that don't want to build the command themselves.
/// Returns the argv; spawning (and the password prompt) is the caller's job.
pub fn privileged_argv(write: &PrivilegedWrite) -> &[String] {
    &write.command
}

/// Remove a staged pending config after a privileged install finished.
pub fn cleanup_pending(write: &PrivilegedWrite) {
    let _ = std::fs::remove_file(Path::new(&write.source));
}
