//! Shared wiring: engine handles + privileged-write execution.

use std::io;
use std::os::unix::fs::MetadataExt;
use std::process::Command;

use nvprofile_engine::{
    cleanup_pending, Dirs, EngineError, LactBackend, LactOptions, PresetStore, PrivilegedWrite,
};

pub struct Ctx {
    pub dirs: Dirs,
    pub store: PresetStore,
    pub opts: LactOptions,
}

impl Ctx {
    pub fn new(gpu: Option<String>) -> Result<Self, EngineError> {
        let dirs = Dirs::from_env()?;
        let mut opts = LactOptions::from_env(dirs.pending.clone());
        opts.gpu_id = gpu;
        Ok(Self { store: PresetStore::new(dirs.presets.clone()), dirs, opts })
    }

    pub fn backend(&self) -> Result<LactBackend, EngineError> {
        LactBackend::connect(self.opts.clone())
    }
}

fn is_root() -> bool {
    std::fs::metadata("/proc/self").is_ok_and(|m| m.uid() == 0)
}

/// Run the privileged install with the real terminal so sudo can prompt, then
/// remove the staged file. The caller must have left any alternate screen.
pub fn run_privileged(write: &PrivilegedWrite) -> io::Result<()> {
    let mut argv: Vec<&str> = write.command.iter().map(String::as_str).collect();
    if is_root() && argv.first() == Some(&"sudo") {
        argv.remove(0);
    }
    let (prog, args) = argv.split_first().ok_or_else(|| io::Error::other("empty privileged command"))?;
    let status = Command::new(prog).args(args).status()?;
    cleanup_pending(write);
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("`{}` exited with {status}", write.command.join(" "))))
    }
}

/// Give the daemon a moment to notice a file replaced by a privileged install
/// (it watches the config), then report whether `expected` is active.
pub fn settle() {
    std::thread::sleep(std::time::Duration::from_millis(1200));
}
