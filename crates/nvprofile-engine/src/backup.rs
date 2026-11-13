//! Timestamped backups of the LACT config with rotation.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{EngineError, Result};

/// How many backups are kept.
pub const KEEP: usize = 10;
const PREFIX: &str = "config-";

/// `YYYYMMDDTHHMMSSZ` (UTC) for a unix timestamp.
pub fn stamp_from_unix(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

pub fn now_stamp() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    stamp_from_unix(secs)
}

/// Copy `src` into `dir` as `config-<stamp>-NN.yaml` and keep only the newest
/// `keep` backups. Returns the new backup path.
pub fn backup_file_at(src: &Path, dir: &Path, stamp: &str, keep: usize) -> Result<PathBuf> {
    fs::create_dir_all(dir).map_err(|e| EngineError::io(dir, &e))?;
    let dest = (0u32..)
        .map(|n| dir.join(format!("{PREFIX}{stamp}-{n:02}.yaml")))
        .find(|p| !p.exists())
        .expect("unbounded range");
    fs::copy(src, &dest).map_err(|e| EngineError::io(src, &e))?;
    rotate(dir, keep)?;
    Ok(dest)
}

pub fn backup_file(src: &Path, dir: &Path) -> Result<PathBuf> {
    backup_file_at(src, dir, &now_stamp(), KEEP)
}

/// Existing backups, oldest first.
pub fn list(dir: &Path) -> Result<Vec<PathBuf>> {
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(EngineError::io(dir, &e)),
    };
    let mut v: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(PREFIX) && n.ends_with(".yaml"))
        })
        .collect();
    v.sort();
    Ok(v)
}

fn rotate(dir: &Path, keep: usize) -> Result<()> {
    let all = list(dir)?;
    for old in all.iter().take(all.len().saturating_sub(keep)) {
        fs::remove_file(old).map_err(|e| EngineError::io(old, &e))?;
    }
    Ok(())
}
