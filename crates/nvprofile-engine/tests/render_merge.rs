mod common;
use common::*;
use nvprofile_engine::lact_config::*;
use nvprofile_engine::*;
use serde_norway::Value;

fn yaml(s: &str) -> Value {
    serde_norway::from_str(s).unwrap()
}

#[test]
fn profile5_renders_the_known_good_fan_block() {
    let cfg = render(&preset("profile5"), &limits()).unwrap();
    assert_eq!(cfg.power_cap, Some(178.5));
    assert!(cfg.fan_control_enabled);
    let text = render_block_yaml(&cfg, true).unwrap();
    let v = yaml(&text);
    let fan = &v["fan_control_settings"];
    assert_eq!(fan["mode"].as_str(), Some("static"));
    assert_eq!(fan["static_speed"].as_f64(), Some(0.51));
    assert_eq!(fan["temperature_key"].as_str(), Some("edge"));
    assert_eq!(fan["interval_ms"].as_u64(), Some(500));
    assert_eq!(fan["curve"][40].as_f64(), Some(0.51));
    assert_eq!(fan["curve"][80].as_f64(), Some(0.51));
    for k in ["spindown_delay_ms", "change_threshold", "auto_threshold"] {
        assert_eq!(fan[k].as_u64(), Some(0), "{k}");
    }
    assert_eq!(cfg.nvidia_gpu_vf_curve.len(), 127);
}

#[test]
fn every_fan_variant_emits_all_required_fields() {
    for id in ["profile5", "night", "desk"] {
        let cfg = render(&preset(id), &limits()).unwrap();
        let v = serde_norway::to_value(&cfg).unwrap();
        let fan = &v["fan_control_settings"];
        for k in ["mode", "temperature_key", "interval_ms", "curve", "spindown_delay_ms", "change_threshold", "auto_threshold"] {
            assert!(!fan[k].is_null(), "{id}: missing {k}");
        }
    }
}

#[test]
fn merging_profile5_into_the_known_good_config_is_a_no_op() {
    let cfg = render(&preset("profile5"), &limits()).unwrap();
    let merged = merge_into_config(FIXTURE_CONFIG, GPU_ID, &cfg).unwrap();
    assert_eq!(yaml(&merged), yaml(FIXTURE_CONFIG));
    assert_eq!(merged, FIXTURE_CONFIG, "textual round trip");
}

#[test]
fn reading_the_known_good_config_matches_profile5_exactly() {
    let applied = read_gpu_from_yaml(FIXTURE_CONFIG, GPU_ID).unwrap();
    assert_eq!(applied, render(&preset("profile5"), &limits()).unwrap());
}

const BUSY: &str = r#"
version: 7
daemon:
  log_level: info
  admin_group: sudo
apply_settings_timer: 5
gpus:
  AAAA:
    power_cap: 100.0
    pmfw_options:
      zero_rpm: true
    gpu_clock_offsets:
      0: -64
    power_mizer_mode: Auto
  BBBB:
    fan_control_enabled: false
    power_cap: 250.0
    nvidia_gpu_vf_curve:
      3:
        clockspeed_offset: 10
        voltage: 700
profiles:
  gaming:
    gpus:
      BBBB:
        power_cap: 300.0
current_profile: null
auto_switch_profiles: false
"#;

#[test]
fn merge_preserves_unrelated_keys_and_other_gpus() {
    let cfg = render(&preset("night"), &limits()).unwrap();
    let out = yaml(&merge_into_config(BUSY, "AAAA", &cfg).unwrap());
    let before = yaml(BUSY);
    for k in ["version", "daemon", "apply_settings_timer", "profiles", "current_profile", "auto_switch_profiles"] {
        assert_eq!(out[k], before[k], "{k}");
    }
    assert_eq!(out["gpus"]["BBBB"], before["gpus"]["BBBB"]);
    let a = &out["gpus"]["AAAA"];
    assert_eq!(a["pmfw_options"], before["gpus"]["AAAA"]["pmfw_options"]);
    assert_eq!(a["gpu_clock_offsets"], before["gpus"]["AAAA"]["gpu_clock_offsets"]);
    assert_eq!(a["power_mizer_mode"], before["gpus"]["AAAA"]["power_mizer_mode"]);
    assert_eq!(a["power_cap"].as_f64(), Some(130.0));
    assert_eq!(a["fan_control_settings"]["mode"].as_str(), Some("curve"));
    assert_eq!(a["fan_control_settings"]["auto_threshold"].as_u64(), Some(50));
}

#[test]
fn stock_clears_managed_keys_only() {
    let cfg = render(&preset("stock"), &limits()).unwrap();
    let out = yaml(&merge_into_config(BUSY, "BBBB", &cfg).unwrap());
    let b = &out["gpus"]["BBBB"];
    assert!(b["power_cap"].is_null());
    assert!(b["nvidia_gpu_vf_curve"].is_null());
    assert!(b["fan_control_settings"].is_null());
    assert_eq!(b["fan_control_enabled"].as_bool(), Some(false));
    assert_eq!(out["gpus"]["AAAA"], yaml(BUSY)["gpus"]["AAAA"]);
}

#[test]
fn merge_creates_missing_gpu_and_gpus_section() {
    let cfg = render(&preset("profile5"), &limits()).unwrap();
    let out = yaml(&merge_into_config("version: 7\n", "NEW", &cfg).unwrap());
    assert_eq!(out["gpus"]["NEW"]["power_cap"].as_f64(), Some(178.5));
    assert_eq!(out["version"].as_u64(), Some(7));
}

#[test]
fn merge_targets_the_active_lact_profile() {
    let text = "version: 7\ncurrent_profile: gaming\nprofiles:\n  gaming:\n    gpus:\n      G:\n        power_cap: 300.0\n";
    let cfg = render(&preset("night"), &limits()).unwrap();
    let out = yaml(&merge_into_config(text, "G", &cfg).unwrap());
    assert_eq!(out["profiles"]["gaming"]["gpus"]["G"]["power_cap"].as_f64(), Some(130.0));
    assert!(out["gpus"].is_null());
    let read = read_gpu_from_yaml(&merge_into_config(text, "G", &cfg).unwrap(), "G").unwrap();
    assert_eq!(read, cfg);
}

#[test]
fn overlay_json_removes_cleared_keys_and_keeps_unknown() {
    let mut block = serde_json::json!({"power_cap": 100.0, "power_mizer_mode": "Auto", "nvidia_gpu_vf_curve": {"1": {"clockspeed_offset": 5}}})
        .as_object()
        .unwrap()
        .clone();
    overlay_json(&mut block, &LactGpuConfig::default()).unwrap();
    assert!(!block.contains_key("power_cap"));
    assert!(!block.contains_key("nvidia_gpu_vf_curve"));
    assert_eq!(block["power_mizer_mode"], "Auto");
    assert_eq!(block["fan_control_enabled"], false);
}

#[test]
fn flat_offset_expands_over_the_driver_table_and_needs_it() {
    let p = Preset { vf: VfSpec::Flat { offset_mhz: 30 }, ..preset("stock") };
    let cfg = render(&p, &limits()).unwrap();
    assert_eq!(cfg.nvidia_gpu_vf_curve.len(), 127);
    assert!(cfg.nvidia_gpu_vf_curve.values().all(|v| v.clockspeed_offset == 30 && v.voltage.is_some()));
    let mut empty = limits();
    empty.vf_points.clear();
    assert!(matches!(render(&p, &empty), Err(EngineError::Unsupported(_))));
}

#[test]
fn max_clock_renders_and_round_trips() {
    let p = Preset { vf: VfSpec::MaxClock { mhz: 1700 }, ..preset("stock") };
    let cfg = render(&p, &limits()).unwrap();
    assert_eq!(cfg.max_core_clock, Some(1700));
    assert_eq!(cfg.to_settings().unwrap().vf, VfSpec::MaxClock { mhz: 1700 });
}

#[test]
fn json_and_yaml_int_keys_both_parse() {
    let j: LactGpuConfig = serde_json::from_str(r#"{"nvidia_gpu_vf_curve":{"7":{"clockspeed_offset":1,"voltage":600}},"fan_control_enabled":false}"#).unwrap();
    let y: LactGpuConfig = serde_norway::from_str("nvidia_gpu_vf_curve:\n  7:\n    clockspeed_offset: 1\n    voltage: 600\n").unwrap();
    assert_eq!(j.nvidia_gpu_vf_curve, y.nvidia_gpu_vf_curve);
}
