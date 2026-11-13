#![allow(dead_code)]
use nvprofile_engine::*;

pub const FIXTURE_CONFIG: &str = include_str!("../fixtures/lact-config.yaml");
pub const GPU_ID: &str = "10DE:2208-1569:2208-0000:0a:00.0";

pub fn preset(id: &str) -> Preset {
    builtin_presets().into_iter().find(|p| p.id == id).unwrap()
}

pub fn limits() -> GpuLimits {
    MockBackend::rtx3080ti().limits
}
