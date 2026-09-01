//! Advisory per-game-dir lock, keyed by the canonical dir hash.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use log::warn;

use crate::packages::chef_home;

use super::dir_hash;

// ---------------------------------------------------------------------------
// Advisory per-game-dir lock
// ---------------------------------------------------------------------------

const LOCK_STALE_SECS: u64 = 30;

pub struct Lock {
    path: PathBuf,
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Lock {
    pub fn acquire(game_dir: &Path) -> anyhow::Result<Lock> {
        let dir = chef_home().join("locks");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.lock", dir_hash(game_dir)));

        match std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
        {
            Ok(mut f) => {
                writeln!(f, "{}", std::process::id())?;
                Ok(Lock { path })
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Stale-lock takeover after 30 s.
                let stale = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                let age = stale
                    .map(|t| t.elapsed().map(|d| d.as_secs()).unwrap_or(u64::MAX))
                    .unwrap_or(u64::MAX);
                if age > LOCK_STALE_SECS {
                    warn!("taking over stale lock {} (age {age}s)", path.display());
                    std::fs::remove_file(&path)?;
                    Lock::acquire(game_dir)
                } else {
                    bail!(
                        "another chef process is working on this game directory (lock: {})",
                        path.display()
                    );
                }
            }
            Err(e) => Err(e).with_context(|| format!("cannot create lock {}", path.display())),
        }
    }
}
