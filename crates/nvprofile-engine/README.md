# nvprofile-engine

UI-agnostic engine behind [`nvprofile`](../../README.md). No terminal dependencies; all public
types implement `serde::{Serialize, Deserialize}`, so a Tauri command or web handler can return
them as JSON unchanged.

## API tour

```rust
use nvprofile_engine::*;

let dirs = Dirs::from_env()?;
let backend = LactBackend::connect(LactOptions::default())?;  // daemon socket, or degraded mode
let store = PresetStore::new(dirs.presets.clone());
let catalog = store.load_all()?;                               // built-ins + user presets
let night = store.get("night")?;

let st: Status = status(&backend, &catalog.presets)?;          // stats + active preset
let plan = plan_apply(&backend, &night)?;                      // what would change
let res = apply_preset(&backend, &dirs, &night, /*dry_run*/ false)?;
if let ApplyOutcome::PrivilegeRequired { write, .. } = res.outcome {
    // run privileged_argv(&write) with sudo, then cleanup_pending(&write)
}
reset(&backend, &dirs, false)?;
```

| Item | Role |
|---|---|
| `Backend` trait | stats, info, limits, applied config, apply. Implementations: `LactBackend`, `MockBackend` (tests) |
| `Preset { id, name, description, power_cap_w, fan, vf }` | `FanSpec` = auto / static / curve (+ zero-RPM threshold); `VfSpec` = stock / offsets / max clock |
| `status`, `diff`, `plan_apply`, `apply_preset`, `reset` | core operations, all take `&dyn Backend` |
| `detect_active` | which preset matches the applied config, else custom |
| `PresetStore`, `Dirs` | built-in + user presets (`~/.config/nvprofile/presets`), backups under `~/.local/state/nvprofile/backups` |
| `validate` | checks a preset against the live GPU limits before any write |
| `EngineError` | serializable error enum |

Exact JSON shapes: serialize any returned type with `serde_json`; `nvprofile status --json` and
`nvprofile presets --json` print real examples.

## Apply path

Primary: daemon socket (`/run/lactd.sock`): `get_gpu_config` -> overlay owned keys -> `set_gpu_config` ->
`confirm_pending_config` (the daemon reverts after `apply_settings_timer` unless confirmed). Fallback when the
socket is unusable: stage a merged config and return a `PrivilegedWrite` for the caller to run via `sudo install`.
The engine never invokes sudo itself.
