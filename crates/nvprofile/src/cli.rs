//! Non-interactive commands. Human output is formatted here; `--json` prints
//! engine types verbatim.

use nvprofile_engine::*;
use serde::Serialize;

use crate::ctx::{run_privileged, settle, Ctx};

type R = Result<()>;

fn print_json<T: Serialize>(v: &T) -> R {
    println!("{}", serde_json::to_string_pretty(v).map_err(|e| EngineError::Config(e.to_string()))?);
    Ok(())
}

fn warn_problems(problems: &[String]) {
    for p in problems {
        eprintln!("nvprofile: warning: skipped user preset: {p}");
    }
}

fn find(catalog: &Catalog, id: &str) -> Result<Preset> {
    catalog
        .presets
        .iter()
        .find(|p| p.id == id)
        .cloned()
        .ok_or_else(|| EngineError::PresetNotFound(id.to_owned()))
}

fn fmt_opt(v: Option<f64>, unit: &str, digits: usize) -> String {
    v.map_or_else(|| "n/a".into(), |x| format!("{x:.digits$} {unit}"))
}

pub fn status(ctx: &Ctx, json: bool) -> R {
    let catalog = ctx.store.load_all()?;
    warn_problems(&catalog.problems);
    let backend = ctx.backend()?;
    let st = engine::status(&backend, &catalog.presets)?;
    if json {
        return print_json(&st);
    }
    let (g, s) = (&st.gpu, &st.stats);
    println!("{}  [{}]", g.name, g.id);
    println!(
        "  driver {}   vbios {}   vram {} MB",
        g.driver.as_deref().unwrap_or("?"),
        g.vbios.as_deref().unwrap_or("?"),
        g.vram_total_mb.map_or("?".into(), |v| v.to_string())
    );
    println!("  temp      {}  (hotspot {}, vram {})", fmt_opt(s.temp_c, "C", 0), fmt_opt(s.hotspot_c, "C", 0), fmt_opt(s.vram_temp_c, "C", 0));
    println!("  fan       {}  {}", fmt_opt(s.fan_pct, "%", 0), s.fan_rpm.map_or("n/a".into(), |r| format!("{r} rpm")));
    println!(
        "  power     {} of {} cap  (range {:.0}-{:.0} W, default {:.0} W)",
        fmt_opt(s.power_draw_w, "W", 1),
        fmt_opt(s.power_cap_w, "W", 1),
        st.limits.power_min_w,
        st.limits.power_max_w,
        st.limits.power_default_w
    );
    println!(
        "  clocks    gpu {} MHz   mem {} MHz   util {} %",
        s.gpu_clock_mhz.map_or("n/a".into(), |v| v.to_string()),
        s.mem_clock_mhz.map_or("n/a".into(), |v| v.to_string()),
        s.util_pct.map_or("n/a".into(), |v| v.to_string())
    );
    println!("  throttle  {}", if s.throttle.is_empty() { "none".into() } else { s.throttle.join(", ") });
    println!();
    match &st.active.preset_id {
        Some(id) => println!("active preset: {id}  ({})", st.active.preset_name.as_deref().unwrap_or("")),
        None => println!("active preset: custom (applied config matches no preset)"),
    }
    println!("  power {}   fan {}   v/f {}", power_summary(st.active.applied.power_cap_w), st.active.applied.fan.summary(), st.active.applied.vf.summary());
    println!("apply path: {}", path_text(st.apply_path));
    Ok(())
}

fn path_text(p: ApplyPath) -> &'static str {
    match p {
        ApplyPath::DaemonSocket => "LACT daemon socket (no root needed; daemon persists the config)",
        ApplyPath::PrivilegedInstall => "privileged install of /etc/lact/config.yaml (sudo)",
    }
}

pub fn presets(ctx: &Ctx, json: bool) -> R {
    let catalog = ctx.store.load_all()?;
    warn_problems(&catalog.problems);
    let active = ctx.backend().ok().and_then(|b| status_active(&b, &catalog.presets));
    if json {
        return print_json(&PresetList { presets: catalog.presets, problems: catalog.problems, active });
    }
    let active_id = active.as_ref().and_then(|a| a.preset_id.clone());
    for p in &catalog.presets {
        let mark = if active_id.as_deref() == Some(&p.id) { "*" } else { " " };
        println!(
            "{mark} {:<14} {:<30} {:<10} {:<52} {}{}",
            p.id,
            p.name,
            power_summary(p.power_cap_w),
            p.fan.summary(),
            p.vf.summary(),
            if p.builtin { "" } else { "  [user]" }
        );
    }
    match &active {
        Some(a) if a.preset_id.is_none() => println!("\n(custom: the applied config matches no preset)"),
        Some(_) => println!("\n* = active"),
        None => println!("\n(active preset unknown: LACT not reachable)"),
    }
    Ok(())
}

fn status_active(b: &LactBackend, presets: &[Preset]) -> Option<ActiveState> {
    let limits = b.limits().ok()?;
    detect_active(&b.applied().ok()?, &limits, presets).ok()
}

pub fn show(ctx: &Ctx, id: &str, json: bool) -> R {
    let catalog = ctx.store.load_all()?;
    warn_problems(&catalog.problems);
    let p = find(&catalog, id)?;
    if json {
        return print_json(&p);
    }
    println!("{}  ({}){}", p.name, p.id, if p.builtin { "  [built-in]" } else { "  [user]" });
    println!("{}\n", wrap(&p.description, 78));
    println!("  power cap  {}", power_summary(p.power_cap_w));
    println!("  fan        {}", p.fan.summary());
    if let FanSpec::Curve { points, hysteresis_c, spindown_delay_ms, .. } = &p.fan {
        for pt in points {
            println!("               {:>3} C -> {:>3.0} %", pt.temp_c, pt.speed * 100.0);
        }
        println!("               hysteresis {hysteresis_c} C, spin-down delay {spindown_delay_ms} ms");
    }
    println!("  v/f        {}", p.vf.summary());
    if let Ok(b) = ctx.backend() {
        let issues = validate::issues(&p, &b.limits()?);
        if issues.is_empty() {
            println!("\nvalid for this GPU");
        } else {
            println!("\nINVALID for this GPU:");
            issues.iter().for_each(|i| println!("  - {i}"));
        }
    }
    Ok(())
}

fn wrap(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut line = 0;
    for w in text.split_whitespace() {
        if line + w.len() + 1 > width && line > 0 {
            out.push('\n');
            line = 0;
        } else if line > 0 {
            out.push(' ');
            line += 1;
        }
        out.push_str(w);
        line += w.len();
    }
    out
}

pub fn diff(ctx: &Ctx, id: &str, json: bool) -> R {
    let catalog = ctx.store.load_all()?;
    warn_problems(&catalog.problems);
    let p = find(&catalog, id)?;
    let backend = ctx.backend()?;
    let d = engine::diff(&backend, &p)?;
    let active = status_active(&backend, &catalog.presets);
    if json {
        return print_json(&d);
    }
    if d.matches {
        let is_active = active.as_ref().and_then(|a| a.preset_id.as_deref()) == Some(id);
        println!("'{id}' matches the applied config{}", if is_active { ": it is the ACTIVE preset" } else { "" });
    } else {
        println!("'{id}' differs from the applied config:");
        for c in &d.changes {
            println!("  {:<10} {}  ->  {}", c.field, c.current, c.target);
        }
    }
    Ok(())
}

fn print_plan(plan: &ApplyPlan) {
    println!("preset:     {}", plan.preset_id);
    println!("apply path: {}", path_text(plan.path));
    if plan.changes.is_empty() {
        println!("changes:    none (already applied)");
    } else {
        println!("changes:");
        for c in &plan.changes {
            println!("  {:<10} {}  ->  {}", c.field, c.current, c.target);
        }
    }
    for w in &plan.warnings {
        println!("warning:    {w}");
    }
}

pub fn apply(ctx: &Ctx, id: &str, dry_run: bool, json: bool) -> R {
    let catalog = ctx.store.load_all()?;
    warn_problems(&catalog.problems);
    let p = find(&catalog, id)?;
    run_apply(ctx, &p, dry_run, json)
}

pub fn reset(ctx: &Ctx, dry_run: bool, json: bool) -> R {
    let stock = builtin_presets().into_iter().find(|p| p.id == "stock").expect("stock exists");
    run_apply(ctx, &stock, dry_run, json)
}

fn run_apply(ctx: &Ctx, preset: &Preset, dry_run: bool, json: bool) -> R {
    let backend = ctx.backend()?;
    let res = apply_preset(&backend, &ctx.dirs, preset, dry_run)?;
    if json {
        return print_json(&res);
    }
    print_plan(&res.plan);
    match &res.outcome {
        ApplyOutcome::DryRun if res.plan.changes.is_empty() => {
            println!("\ndry run: nothing would be sent; the applied config already matches.");
        }
        ApplyOutcome::DryRun => {
            println!("\nconfig block that will be written to the GPU entry (dry run: nothing changed):\n");
            print!("{}", indent(&res.plan.config_block));
            match res.plan.path {
                ApplyPath::DaemonSocket => println!(
                    "\nsteps: back up {} -> {} (keep {}), then set_gpu_config + confirm_pending_config on the daemon socket",
                    backend.config_file().map_or("config".into(), |p| p.display().to_string()),
                    ctx.dirs.backups.display(),
                    backup::KEEP
                ),
                ApplyPath::PrivilegedInstall => println!(
                    "\nsteps: back up the config, stage the merged file in {}, run: {}",
                    ctx.dirs.pending.display(),
                    PrivilegedWrite::install("<staged>", &backend.config_file().map_or(String::new(), |p| p.display().to_string())).command.join(" ")
                ),
            }
        }
        ApplyOutcome::NoChange => println!("\nnothing to do."),
        ApplyOutcome::Applied { verified, notes, backup } => {
            println!("\napplied '{}'{}", preset.id, if *verified { " (verified against the daemon)" } else { " (NOT verified)" });
            notes.iter().for_each(|n| println!("note: {n}"));
            if let Some(b) = backup {
                println!("previous config backed up to {b}");
            }
        }
        ApplyOutcome::PrivilegeRequired { write, backup } => {
            if let Some(b) = backup {
                println!("previous config backed up to {b}");
            }
            println!("\nrunning: {}", write.command.join(" "));
            run_privileged(write).map_err(|e| EngineError::Config(e.to_string()))?;
            settle();
            let d = engine::diff(&backend, preset)?;
            if d.matches {
                println!("applied '{}' (config installed; lactd watches the file and reloads it)", preset.id);
            } else {
                println!("config installed, but lactd has not picked it up yet; check `nvprofile status`");
            }
        }
    }
    Ok(())
}

fn indent(s: &str) -> String {
    s.lines().map(|l| format!("    {l}\n")).collect()
}

pub fn save(ctx: &Ctx, name: &str, description: &str, force: bool) -> R {
    let backend = ctx.backend()?;
    let preset = current_as_preset(&backend, name, description)?;
    let path = ctx.store.save(&preset, &backend.limits()?, force)?;
    println!("saved '{}' to {}", preset.id, path.display());
    println!("  power {}   fan {}   v/f {}", power_summary(preset.power_cap_w), preset.fan.summary(), preset.vf.summary());
    Ok(())
}

pub fn delete(ctx: &Ctx, id: &str) -> R {
    ctx.store.delete(id)?;
    println!("deleted '{id}'");
    Ok(())
}
