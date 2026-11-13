mod common;
use common::*;
use nvprofile_engine::lact_config::*;
use nvprofile_engine::*;

#[test]
fn builtin_presets_ship_and_validate() {
    let ids: Vec<_> = builtin_presets().iter().map(|p| p.id.clone()).collect();
    assert_eq!(ids, ["stock", "profile5", "night", "desk"]);
    for p in builtin_presets() {
        assert!(p.builtin);
        assert_eq!(validate::issues(&p, &limits()), Vec::<String>::new(), "{}", p.id);
    }
    let VfSpec::Offsets { points } = preset("profile5").vf else { panic!() };
    assert_eq!(points.len(), 127);
    assert_eq!((points[0].index, points[0].voltage_mv, points[0].offset_mhz), (0, Some(450), 55));
    assert_eq!((points[126].index, points[126].voltage_mv, points[126].offset_mhz), (126, Some(1237), -35));
}

fn bad(p: &Preset) -> Vec<String> {
    validate::issues(p, &limits())
}

#[test]
fn power_range_is_enforced() {
    let mk = |w| Preset { power_cap_w: Some(w), ..preset("stock") };
    assert!(bad(&mk(99.0)).iter().any(|i| i.contains("power cap")));
    assert!(bad(&mk(366.0)).iter().any(|i| i.contains("power cap")));
    assert!(bad(&mk(100.0)).is_empty());
    assert!(bad(&mk(365.0)).is_empty());
}

#[test]
fn fan_minimum_and_zero_rpm_threshold() {
    let st = |s| Preset { fan: FanSpec::Static { speed: s }, ..preset("stock") };
    assert!(bad(&st(0.20)).iter().any(|i| i.contains("minimum")));
    assert!(bad(&st(0.30)).is_empty(), "0.30 passes against 77/255 within tolerance");
    assert!(bad(&st(1.2)).iter().any(|i| i.contains("outside 0..1")));

    let curve = |zero| Preset {
        fan: FanSpec::Curve {
            points: vec![CurvePoint { temp_c: 40, speed: 0.1 }, CurvePoint { temp_c: 60, speed: 0.4 }],
            zero_rpm_below_c: zero,
            hysteresis_c: 0,
            spindown_delay_ms: 0,
        },
        ..preset("stock")
    };
    assert!(bad(&curve(None)).iter().any(|i| i.contains("minimum")));
    assert!(bad(&curve(Some(50))).is_empty(), "10% at 40C is driver-owned below a 50C zero-RPM threshold");
    assert!(bad(&curve(Some(35))).iter().any(|i| i.contains("minimum")));
}

#[test]
fn curve_shape_is_enforced() {
    let mk = |pts: &[(u32, f64)]| Preset {
        fan: FanSpec::Curve {
            points: pts.iter().map(|(t, s)| CurvePoint { temp_c: *t, speed: *s }).collect(),
            zero_rpm_below_c: None,
            hysteresis_c: 0,
            spindown_delay_ms: 0,
        },
        ..preset("stock")
    };
    assert!(bad(&mk(&[(50, 0.5)])).iter().any(|i| i.contains("at least 2")));
    assert!(bad(&mk(&[(50, 0.5), (50, 0.6)])).iter().any(|i| i.contains("strictly increase")));
    assert!(bad(&mk(&[(50, 0.6), (60, 0.5)])).iter().any(|i| i.contains("must not decrease")));
    assert!(bad(&mk(&[(50, 0.5), (60, 0.6)])).is_empty());
}

#[test]
fn offsets_are_range_checked() {
    let mk = |idx: u32, off: i32| Preset {
        vf: VfSpec::Offsets { points: vec![VfOffset { index: idx, voltage_mv: None, offset_mhz: off }] },
        ..preset("stock")
    };
    assert!(bad(&mk(3, 1001)).iter().any(|i| i.contains("offset")));
    assert!(bad(&mk(3, -1001)).iter().any(|i| i.contains("offset")));
    assert!(bad(&mk(3, 1000)).is_empty());
    assert!(bad(&mk(500, 0)).iter().any(|i| i.contains("does not exist")));
    let dup = Preset {
        vf: VfSpec::Offsets {
            points: vec![VfOffset { index: 1, voltage_mv: None, offset_mhz: 0 }, VfOffset { index: 1, voltage_mv: None, offset_mhz: 1 }],
        },
        ..preset("stock")
    };
    assert!(bad(&dup).iter().any(|i| i.contains("duplicate")));
    assert!(bad(&Preset { vf: VfSpec::Flat { offset_mhz: 2000 }, ..preset("stock") }).iter().any(|i| i.contains("flat")));
    assert!(bad(&Preset { vf: VfSpec::MaxClock { mhz: 5000 }, ..preset("stock") }).iter().any(|i| i.contains("max clock")));
}

#[test]
fn ids_and_slugs() {
    assert!(is_valid_id("night-2"));
    for b in ["", "Night", "-x", "a b", "../x", &"a".repeat(33)] {
        assert!(!is_valid_id(b), "{b:?}");
    }
    assert_eq!(slugify("  Quiet Render!! "), "quiet-render");
    assert_eq!(slugify("***"), "");
    assert!(is_valid_id(&slugify(&"x".repeat(80))));
}

// ---- active detection ----

fn detect(applied: &LactGpuConfig) -> ActiveState {
    detect_active(applied, &limits(), &builtin_presets()).unwrap()
}

#[test]
fn detects_each_builtin_and_stock_for_empty_config() {
    for id in ["profile5", "night", "desk"] {
        let a = render(&preset(id), &limits()).unwrap();
        assert_eq!(detect(&a).preset_id.as_deref(), Some(id));
    }
    assert_eq!(detect(&LactGpuConfig::default()).preset_id.as_deref(), Some("stock"));
}

#[test]
fn power_cap_equal_to_default_still_counts_as_stock() {
    let a = LactGpuConfig { power_cap: Some(350.0), ..Default::default() };
    assert_eq!(detect(&a).preset_id.as_deref(), Some("stock"));
}

#[test]
fn any_deviation_is_custom() {
    let base = render(&preset("profile5"), &limits()).unwrap();

    let mut a = base.clone();
    a.power_cap = Some(180.0);
    let st = detect(&a);
    assert_eq!(st.preset_id, None);
    assert_eq!(st.applied.power_cap_w, Some(180.0));

    let mut a = base.clone();
    a.nvidia_gpu_vf_curve.get_mut(&50).unwrap().clockspeed_offset += 5;
    assert_eq!(detect(&a).preset_id, None);

    let mut a = base.clone();
    a.fan_control_settings.as_mut().unwrap().static_speed = 0.55;
    assert_eq!(detect(&a).preset_id, None);

    let mut a = base;
    a.fan_control_enabled = false;
    assert_eq!(detect(&a).preset_id, None);
}

#[test]
fn float_noise_in_fan_speed_is_tolerated() {
    let mut a = render(&preset("profile5"), &limits()).unwrap();
    a.fan_control_settings.as_mut().unwrap().static_speed = 0.5100001;
    assert_eq!(detect(&a).preset_id.as_deref(), Some("profile5"));
}

#[test]
fn diff_lists_exactly_the_differing_fields() {
    let b = MockBackend::rtx3080ti();
    b.set_applied(render(&preset("profile5"), &b.limits).unwrap());
    assert!(diff(&b, &preset("profile5")).unwrap().matches);
    let d = diff(&b, &preset("night")).unwrap();
    assert!(!d.matches);
    let fields: Vec<_> = d.changes.iter().map(|c| c.field.as_str()).collect();
    assert_eq!(fields, ["power_cap", "fan"], "night shares the V/F curve with profile5");
    assert_eq!(d.changes[0].current, "178.5 W");
    assert_eq!(d.changes[0].target, "130.0 W");
    let d = diff(&b, &preset("stock")).unwrap();
    assert_eq!(d.changes.len(), 3);
    assert!(d.changes[2].target.contains("stock"));
}

#[test]
fn settings_round_trip_through_current_as_preset() {
    let b = MockBackend::rtx3080ti();
    b.set_applied(render(&preset("night"), &b.limits).unwrap());
    let p = current_as_preset(&b, "My Night!", "copy").unwrap();
    assert_eq!(p.id, "my-night");
    assert_eq!(p.fan, preset("night").fan);
    assert_eq!(p.power_cap_w, Some(130.0));
    validate::validate(&p, &b.limits).unwrap();
    assert_eq!(render(&p, &b.limits).unwrap(), render(&preset("night"), &b.limits).unwrap());
}

// ---- store ----

#[test]
fn store_save_list_delete_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let store = PresetStore::new(dir.path().join("presets"));
    let p = Preset {
        id: "quiet".into(),
        name: "Quiet".into(),
        description: "d".into(),
        power_cap_w: Some(150.0),
        fan: FanSpec::Static { speed: 0.4 },
        vf: VfSpec::Flat { offset_mhz: 20 },
        builtin: false,
    };
    store.save(&p, &limits(), false).unwrap();
    assert_eq!(store.save(&p, &limits(), false), Err(EngineError::PresetExists("quiet".into())));
    store.save(&Preset { power_cap_w: Some(160.0), ..p.clone() }, &limits(), true).unwrap();

    let cat = store.load_all().unwrap();
    assert!(cat.problems.is_empty());
    assert_eq!(cat.presets.len(), 5);
    assert_eq!(cat.presets[4].power_cap_w, Some(160.0));
    assert!(!cat.presets[4].builtin);
    assert_eq!(store.get("quiet").unwrap().fan, FanSpec::Static { speed: 0.4 });

    store.delete("quiet").unwrap();
    assert_eq!(store.delete("quiet"), Err(EngineError::PresetNotFound("quiet".into())));
    assert_eq!(store.load_all().unwrap().presets.len(), 4);
}

#[test]
fn store_rejects_builtin_ids_and_invalid_presets() {
    let dir = tempfile::tempdir().unwrap();
    let store = PresetStore::new(dir.path());
    let p = Preset { id: "night".into(), ..preset("night") };
    assert_eq!(store.save(&p, &limits(), true), Err(EngineError::BuiltinReadOnly("night".into())));
    assert_eq!(store.delete("stock"), Err(EngineError::BuiltinReadOnly("stock".into())));
    let bad = Preset { id: "x".into(), power_cap_w: Some(10.0), ..preset("night") };
    assert!(matches!(store.save(&bad, &limits(), false), Err(EngineError::Validation { .. })));
    assert!(store.delete("../etc").is_err());
}

#[test]
fn store_reports_broken_and_colliding_files_without_failing() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("broken.yaml"), "id: [").unwrap();
    std::fs::write(dir.path().join("mismatch.yaml"), serde_norway::to_string(&Preset { id: "other".into(), ..preset("night") }).unwrap()).unwrap();
    std::fs::write(dir.path().join("night.yaml"), serde_norway::to_string(&preset("night")).unwrap()).unwrap();
    std::fs::write(dir.path().join("notes.txt"), "ignored").unwrap();
    let cat = PresetStore::new(dir.path()).load_all().unwrap();
    assert_eq!(cat.presets.len(), 4);
    assert_eq!(cat.problems.len(), 3, "{:?}", cat.problems);
}

#[test]
fn user_yaml_is_hand_writable() {
    let text = "id: gaming-lite\nname: Gaming lite\npower_cap_w: 200\nfan:\n  type: curve\n  points:\n    - {temp_c: 40, speed: 0.35}\n    - {temp_c: 70, speed: 0.6}\n  zero_rpm_below_c: 40\nvf:\n  type: stock\n";
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("gaming-lite.yaml"), text).unwrap();
    let cat = PresetStore::new(dir.path()).load_all().unwrap();
    let p = cat.presets.last().unwrap();
    assert_eq!(p.id, "gaming-lite");
    assert_eq!(p.description, "");
    validate::validate(p, &limits()).unwrap();
}
