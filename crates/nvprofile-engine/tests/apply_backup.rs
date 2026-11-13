mod common;
use common::*;
use nvprofile_engine::lact_config::*;
use nvprofile_engine::*;

fn dirs() -> (tempfile::TempDir, Dirs) {
    let t = tempfile::tempdir().unwrap();
    let d = Dirs::under(t.path());
    (t, d)
}

#[test]
fn backup_stamps() {
    assert_eq!(backup::stamp_from_unix(0), "19700101T000000Z");
    assert_eq!(backup::stamp_from_unix(1_700_000_000), "20231114T221320Z");
    assert_eq!(backup::stamp_from_unix(951_782_400 + 86_399), "20000229T235959Z");
}

#[test]
fn backup_rotation_keeps_the_newest_ten() {
    let t = tempfile::tempdir().unwrap();
    let src = t.path().join("config.yaml");
    let dir = t.path().join("backups");
    for i in 0..13u64 {
        std::fs::write(&src, format!("gen: {i}\n")).unwrap();
        backup::backup_file_at(&src, &dir, &backup::stamp_from_unix(1_700_000_000 + i * 60), backup::KEEP).unwrap();
    }
    let left = backup::list(&dir).unwrap();
    assert_eq!(left.len(), 10);
    assert_eq!(std::fs::read_to_string(&left[0]).unwrap(), "gen: 3\n", "oldest three were rotated out");
    assert_eq!(std::fs::read_to_string(left.last().unwrap()).unwrap(), "gen: 12\n");
}

#[test]
fn backups_in_the_same_second_do_not_collide_and_stay_ordered() {
    let t = tempfile::tempdir().unwrap();
    let src = t.path().join("c.yaml");
    let dir = t.path().join("b");
    for i in 0..3 {
        std::fs::write(&src, format!("{i}")).unwrap();
        backup::backup_file_at(&src, &dir, "20260101T000000Z", 10).unwrap();
    }
    let l = backup::list(&dir).unwrap();
    let contents: Vec<_> = l.iter().map(|p| std::fs::read_to_string(p).unwrap()).collect();
    assert_eq!(contents, ["0", "1", "2"]);
}

#[test]
fn apply_changes_state_and_backs_up_the_config() {
    let (t, d) = dirs();
    let cfg = t.path().join("lact.yaml");
    std::fs::write(&cfg, FIXTURE_CONFIG).unwrap();
    let mut b = MockBackend::rtx3080ti();
    b.config_file = Some(cfg);
    b.set_applied(render(&preset("profile5"), &b.limits).unwrap());

    let r = apply_preset(&b, &d, &preset("night"), false).unwrap();
    let ApplyOutcome::Applied { verified, backup, .. } = &r.outcome else { panic!("{:?}", r.outcome) };
    assert!(verified);
    assert_eq!(std::fs::read_to_string(backup.as_ref().unwrap()).unwrap(), FIXTURE_CONFIG);
    assert_eq!(r.plan.path, ApplyPath::DaemonSocket);
    assert_eq!(b.applied_history().len(), 1);
    assert_eq!(detect_active(&b.applied().unwrap(), &b.limits, &builtin_presets()).unwrap().preset_id.as_deref(), Some("night"));

    // applying the same preset again sends nothing
    let again = apply_preset(&b, &d, &preset("night"), false).unwrap();
    assert_eq!(again.outcome, ApplyOutcome::NoChange);
    assert_eq!(b.applied_history().len(), 1);
}

#[test]
fn dry_run_writes_nothing() {
    let (t, d) = dirs();
    let cfg = t.path().join("lact.yaml");
    std::fs::write(&cfg, FIXTURE_CONFIG).unwrap();
    let mut b = MockBackend::rtx3080ti();
    b.config_file = Some(cfg);
    let r = apply_preset(&b, &d, &preset("night"), true).unwrap();
    assert_eq!(r.outcome, ApplyOutcome::DryRun);
    assert!(b.applied_history().is_empty());
    assert!(!d.backups.exists());
    assert!(r.plan.config_block.contains("power_cap: 130"));
    assert!(r.plan.config_block.contains("<127 points>"));
    assert_eq!(r.plan.changes.len(), 3);
}

#[test]
fn stock_plan_lists_the_keys_it_removes() {
    let (_t, d) = dirs();
    let b = MockBackend::rtx3080ti();
    b.set_applied(render(&preset("profile5"), &b.limits).unwrap());
    let r = apply_preset(&b, &d, &preset("stock"), true).unwrap();
    assert!(r.plan.config_block.contains("fan_control_enabled: false"));
    assert!(r.plan.config_block.contains("# removed from the GPU entry: fan_control_settings, power_cap, nvidia_gpu_vf_curve, max_core_clock"), "{}", r.plan.config_block);
}

#[test]
fn invalid_presets_are_refused_before_any_write() {
    let (_t, d) = dirs();
    let b = MockBackend::rtx3080ti();
    let p = Preset { power_cap_w: Some(50.0), ..preset("night") };
    assert!(matches!(apply_preset(&b, &d, &p, false), Err(EngineError::Validation { .. })));
    assert!(b.applied_history().is_empty());
}

#[test]
fn privileged_path_returns_a_plan_not_a_write() {
    let (_t, d) = dirs();
    let mut b = MockBackend::rtx3080ti();
    b.path = ApplyPath::PrivilegedInstall;
    let r = apply_preset(&b, &d, &preset("night"), false).unwrap();
    let ApplyOutcome::PrivilegeRequired { write, .. } = r.outcome else { panic!() };
    assert_eq!(write.command[..4], ["sudo", "install", "-m", "644"]);
    assert_eq!(privileged_argv(&write), write.command.as_slice());
    assert!(!r.plan.warnings.is_empty());
    assert!(b.applied_history().is_empty());
}

#[test]
fn backend_failure_surfaces_as_engine_error() {
    let (_t, d) = dirs();
    let b = MockBackend::rtx3080ti();
    b.fail_next_apply(EngineError::Daemon("pending change".into()));
    assert_eq!(apply_preset(&b, &d, &preset("night"), false).unwrap_err(), EngineError::Daemon("pending change".into()));
}

#[test]
fn reset_applies_stock() {
    let (_t, d) = dirs();
    let b = MockBackend::rtx3080ti();
    b.set_applied(render(&preset("profile5"), &b.limits).unwrap());
    let r = reset(&b, &d, false).unwrap();
    assert!(matches!(r.outcome, ApplyOutcome::Applied { .. }));
    assert_eq!(b.applied().unwrap(), LactGpuConfig::default());
}

#[test]
fn status_bundles_everything_and_serializes() {
    let b = MockBackend::rtx3080ti();
    let s = status(&b, &builtin_presets()).unwrap();
    assert_eq!(s.active.preset_id.as_deref(), Some("stock"));
    let j = serde_json::to_value(&s).unwrap();
    assert_eq!(j["apply_path"], "daemon_socket");
    assert_eq!(j["active"]["applied"]["fan"]["type"], "auto");
    assert_eq!(j["limits"]["power_min_w"], 100.0);
    let back: Status = serde_json::from_value(j).unwrap();
    assert_eq!(back, s);
}

#[test]
fn json_shapes_are_stable() {
    let p = serde_json::to_value(preset("night")).unwrap();
    assert_eq!(p["fan"]["type"], "curve");
    assert_eq!(p["fan"]["zero_rpm_below_c"], 50);
    assert_eq!(p["vf"]["type"], "offsets");
    assert_eq!(p["builtin"], true);
    let e = serde_json::to_value(EngineError::Validation { issues: vec!["x".into()] }).unwrap();
    assert_eq!(e["kind"], "validation");
    assert_eq!(e["detail"]["issues"][0], "x");
    let e = serde_json::to_value(EngineError::PresetNotFound("a".into())).unwrap();
    assert_eq!(e, serde_json::json!({"kind":"preset_not_found","detail":"a"}));
    let o = serde_json::to_value(ApplyOutcome::NoChange).unwrap();
    assert_eq!(o, serde_json::json!({"status":"no_change"}));
}
