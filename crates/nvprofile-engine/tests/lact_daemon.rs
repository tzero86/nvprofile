//! Drives the real `LactBackend` against an in-process fake `lactd` that
//! speaks the lact-schema wire format and replays fixtures captured from a live
//! daemon. Nothing here touches /run/lactd.sock or /etc.

mod common;
use common::*;
use nvprofile_engine::*;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::{Arc, Mutex};

fn fixture(name: &str) -> Value {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

#[derive(Default)]
struct Daemon {
    block: Value,
    pending: bool,
    requests: Vec<Value>,
}

fn ok(data: Value) -> Value {
    json!({"status": "ok", "data": data})
}
fn err(msg: &str) -> Value {
    json!({"status": "error", "data": {"description": msg, "source": null}})
}

fn serve(sock: &Path, state: Arc<Mutex<Daemon>>) {
    let l = UnixListener::bind(sock).unwrap();
    std::thread::spawn(move || {
        for conn in l.incoming() {
            let Ok(conn) = conn else { return };
            let state = state.clone();
            std::thread::spawn(move || {
                let mut rd = BufReader::new(conn.try_clone().unwrap());
                let mut w = conn;
                let mut line = String::new();
                while rd.read_line(&mut line).unwrap_or(0) > 0 {
                    let req: Value = serde_json::from_str(&line).unwrap();
                    line.clear();
                    let mut st = state.lock().unwrap();
                    st.requests.push(req.clone());
                    let id_ok = req["args"]["id"] == GPU_ID;
                    let reply = match req["command"].as_str().unwrap() {
                        "ping" => ok(Value::Null),
                        "list_devices" => fixture("list_devices.json"),
                        "device_stats" if id_ok => {
                            // Reflect the persisted power cap like the real daemon does.
                            let mut v = fixture("device_stats.json");
                            v["data"]["power"]["cap_current"] = st.block.get("power_cap").cloned().unwrap_or(json!(350.0));
                            v
                        }
                        c @ ("device_info" | "device_clocks_info") if id_ok => fixture(&format!("{c}.json")),
                        "get_gpu_config" if id_ok => ok(st.block.clone()),
                        "set_gpu_config" if id_ok => {
                            if st.pending {
                                err("There is an unconfirmed configuration change pending")
                            } else {
                                st.block = req["args"]["config"].clone();
                                st.pending = true;
                                ok(json!(5))
                            }
                        }
                        "confirm_pending_config" => {
                            if std::mem::take(&mut st.pending) && req["args"] == json!({"command": "confirm"}) {
                                ok(Value::Null)
                            } else {
                                err("No pending config changes")
                            }
                        }
                        _ => err("Failed to deserialize"),
                    };
                    drop(st);
                    writeln!(w, "{reply}").unwrap();
                }
            });
        }
    });
}

struct Rig {
    _t: tempfile::TempDir,
    backend: LactBackend,
    state: Arc<Mutex<Daemon>>,
    dirs: Dirs,
    config: std::path::PathBuf,
}

fn rig(with_socket: bool) -> Rig {
    let t = tempfile::tempdir().unwrap();
    let sock = t.path().join("lactd.sock");
    let config = t.path().join("config.yaml");
    std::fs::write(&config, FIXTURE_CONFIG).unwrap();
    let state = Arc::new(Mutex::new(Daemon { block: fixture("get_gpu_config.json")["data"].clone(), ..Daemon::default() }));
    if with_socket {
        serve(&sock, state.clone());
    }
    let dirs = Dirs::under(t.path());
    let opts = LactOptions {
        socket: sock,
        config_path: config.clone(),
        pending_dir: dirs.pending.clone(),
        gpu_id: None,
        nvidia_smi: fake_smi(t.path()),
    };
    let backend = LactBackend::connect(opts).unwrap();
    Rig { _t: t, backend, state, dirs, config }
}

/// A stand-in `nvidia-smi` that answers the queries the engine makes.
fn fake_smi(dir: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join("fake-nvidia-smi");
    std::fs::write(
        &p,
        r#"#!/bin/sh
case "$1" in
  *power.min_limit*) echo "100.00, 365.00, 350.00" ;;
  *temperature.gpu*) echo "52, 51, 177.80, 178.50, 1720, 9251, 100, 7979, 12288, 0x0000000000000004" ;;
  *driver_version*) echo "NVIDIA GeForce RTX 3080 Ti, 595.99.02, 94.02.71.80.52, 12288" ;;
  *name*) echo "NVIDIA GeForce RTX 3080 Ti" ;;
  *) exit 2 ;;
esac
"#,
    )
    .unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

#[test]
fn reads_info_limits_stats_from_daemon_json() {
    let r = rig(true);
    let b = &r.backend;
    assert_eq!(b.gpu_id(), GPU_ID);
    let info = b.gpu_info().unwrap();
    assert_eq!(info.name, "NVIDIA GeForce RTX 3080 Ti");
    assert_eq!(info.driver.as_deref(), Some("nvidia 595.99.02"));
    assert_eq!(info.vram_total_mb, Some(12288));
    assert!(info.capabilities.contains(&"AutoFanThreshold".to_owned()));

    let l = b.limits().unwrap();
    assert_eq!((l.power_min_w, l.power_max_w, l.power_default_w), (100.0, 365.0, 350.0));
    assert!((l.fan_min - 77.0 / 255.0).abs() < 1e-9);
    assert_eq!((l.clock_min_mhz, l.clock_max_mhz), (210, 2100));
    assert_eq!((l.offset_min_mhz, l.offset_max_mhz), (-1000, 1000));
    assert_eq!(l.vf_points.len(), 127);
    assert_eq!((l.vf_points[0].index, l.vf_points[0].voltage_mv), (0, 450));

    let s = b.stats().unwrap();
    assert_eq!(s.power_cap_w, Some(178.5));
    assert_eq!(s.fan_rpm, Some(1991));
    assert!((s.fan_pct.unwrap() - 51.0).abs() < 0.5);
    assert_eq!(s.util_pct, Some(100));
    assert_eq!(s.throttle, ["SW_POWER_CAP"]);
    assert!(s.temp_c.is_some() && s.hotspot_c.is_some());
}

#[test]
fn live_daemon_config_is_detected_as_profile5() {
    let r = rig(true);
    let st = status(&r.backend, &builtin_presets()).unwrap();
    assert_eq!(st.active.preset_id.as_deref(), Some("profile5"));
    assert_eq!(st.apply_path, ApplyPath::DaemonSocket);
    assert!(diff(&r.backend, &preset("profile5")).unwrap().matches);
    // JSON the daemon sent round-trips through our typed view unchanged.
    let raw = fixture("get_gpu_config.json")["data"].clone();
    let typed = r.backend.applied().unwrap();
    assert_eq!(serde_json::to_value(&typed).unwrap(), raw);
}

#[test]
fn apply_goes_set_then_confirm_and_keeps_unknown_keys() {
    let r = rig(true);
    // Simulate a key we don't manage already present on the daemon side.
    r.state.lock().unwrap().block["power_mizer_mode"] = json!("Auto");
    let res = apply_preset(&r.backend, &r.dirs, &preset("night"), false).unwrap();
    let ApplyOutcome::Applied { verified, backup, notes } = res.outcome else { panic!() };
    assert!(verified, "{notes:?}");
    assert!(backup.is_some(), "config backed up before writing");

    let st = r.state.lock().unwrap();
    let cmds: Vec<_> = st.requests.iter().map(|q| q["command"].as_str().unwrap().to_owned()).collect();
    let set = cmds.iter().position(|c| c == "set_gpu_config").unwrap();
    assert_eq!(cmds[set + 1], "confirm_pending_config", "confirmed right after set: {cmds:?}");
    assert!(!st.pending);

    let sent = &st.requests[set]["args"];
    assert_eq!(sent["id"], GPU_ID);
    let c = &sent["config"];
    assert_eq!(c["power_cap"], 130.0);
    assert_eq!(c["power_mizer_mode"], "Auto");
    assert_eq!(c["fan_control_enabled"], true);
    assert_eq!(c["fan_control_settings"]["mode"], "curve");
    assert_eq!(c["fan_control_settings"]["auto_threshold"], 50);
    assert_eq!(c["fan_control_settings"]["curve"]["50"], 0.31);
    assert_eq!(c["nvidia_gpu_vf_curve"]["126"], json!({"clockspeed_offset": -35, "voltage": 1237}));
}

#[test]
fn reset_via_daemon_clears_overrides() {
    let r = rig(true);
    let res = reset(&r.backend, &r.dirs, false).unwrap();
    assert!(matches!(res.outcome, ApplyOutcome::Applied { verified: true, .. }));
    let st = r.state.lock().unwrap();
    let c = st.block.as_object().unwrap();
    assert!(!c.contains_key("power_cap") && !c.contains_key("nvidia_gpu_vf_curve") && !c.contains_key("fan_control_settings"));
    assert_eq!(c["fan_control_enabled"], false);
}

#[test]
fn pending_change_error_is_reported() {
    let r = rig(true);
    r.state.lock().unwrap().pending = true;
    let e = apply_preset(&r.backend, &r.dirs, &preset("night"), false).unwrap_err();
    assert!(matches!(&e, EngineError::Daemon(m) if m.contains("unconfirmed")), "{e}");
}

#[test]
fn dry_run_sends_no_write_requests() {
    let r = rig(true);
    apply_preset(&r.backend, &r.dirs, &preset("night"), true).unwrap();
    let st = r.state.lock().unwrap();
    assert!(st.requests.iter().all(|q| !["set_gpu_config", "confirm_pending_config"].contains(&q["command"].as_str().unwrap())));
}

#[test]
fn without_a_socket_the_privileged_plan_is_staged_and_merged() {
    let r = rig(false);
    assert_eq!(r.backend.apply_path(), ApplyPath::PrivilegedInstall);
    // applied() falls back to reading the config file.
    assert_eq!(status_applied(&r), Some("profile5".to_owned()));

    let res = apply_preset(&r.backend, &r.dirs, &preset("night"), false).unwrap();
    let ApplyOutcome::PrivilegeRequired { write, backup } = res.outcome else { panic!("{:?}", res.outcome) };
    assert_eq!(write.dest, r.config.display().to_string());
    assert!(write.source.starts_with(&r.dirs.pending.display().to_string()));
    assert_eq!(write.command, ["sudo", "install", "-m", "644", &write.source, &write.dest]);
    assert!(backup.is_some());
    // The destination is untouched; the staged file holds the merged config.
    assert_eq!(std::fs::read_to_string(&r.config).unwrap(), FIXTURE_CONFIG);
    let staged: serde_norway::Value = serde_norway::from_str(&std::fs::read_to_string(&write.source).unwrap()).unwrap();
    assert_eq!(staged["gpus"][GPU_ID]["power_cap"].as_f64(), Some(130.0));
    assert_eq!(staged["daemon"]["admin_group"].as_str(), Some("sudo"));
    cleanup_pending(&write);
    assert!(!Path::new(&write.source).exists());
}

fn status_applied(r: &Rig) -> Option<String> {
    let st = status(&r.backend, &builtin_presets()).unwrap();
    assert_eq!(st.apply_path, ApplyPath::PrivilegedInstall);
    st.active.preset_id
}

#[test]
fn degraded_mode_reads_through_nvidia_smi() {
    let r = rig(false);
    let b = &r.backend;
    assert_eq!(b.gpu_id(), GPU_ID, "inferred from the config file");
    let l = b.limits().unwrap();
    assert_eq!((l.power_min_w, l.power_max_w, l.power_default_w), (100.0, 365.0, 350.0));
    assert!(l.vf_points.is_empty());
    let s = b.stats().unwrap();
    assert_eq!((s.temp_c, s.fan_pct, s.power_draw_w, s.power_cap_w), (Some(52.0), Some(51.0), Some(177.8), Some(178.5)));
    assert_eq!(s.throttle, ["SW_POWER_CAP"]);
    assert_eq!(b.gpu_info().unwrap().driver.as_deref(), Some("595.99.02"));
}

#[test]
fn unknown_gpu_id_is_a_clean_error() {
    let t = tempfile::tempdir().unwrap();
    let sock = t.path().join("s");
    serve(&sock, Arc::new(Mutex::new(Daemon::default())));
    let opts = LactOptions { socket: sock, config_path: t.path().join("c"), pending_dir: t.path().join("p"), gpu_id: Some("nope".into()), nvidia_smi: "x".into() };
    assert!(matches!(LactBackend::connect(opts), Err(EngineError::NoGpu(_))));
}

#[test]
fn missing_socket_and_unusable_config_is_daemon_unavailable() {
    let t = tempfile::tempdir().unwrap();
    let opts = LactOptions { socket: t.path().join("none"), config_path: t.path().join("c"), pending_dir: t.path().join("p"), gpu_id: None, nvidia_smi: "x".into() };
    assert!(matches!(LactBackend::connect(opts), Err(EngineError::DaemonUnavailable(_))));
}
