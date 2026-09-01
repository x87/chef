//! Backup folders: per-game-dir snapshots under the data home.

use std::path::{Path, PathBuf};

use crate::packages::chef_home;

use super::{dir_hash, unix_now};

// ---------------------------------------------------------------------------
// Backups
// ---------------------------------------------------------------------------

pub fn backup_root_for(game_dir: &Path) -> PathBuf {
    chef_home().join("backups").join(dir_hash(game_dir))
}

pub(crate) fn new_backup_dir(game_dir: &Path) -> anyhow::Result<PathBuf> {
    let ts = unix_now().to_string();
    let root = backup_root_for(game_dir);
    // The timestamp is second-resolution: successive deploys within the
    // same second must not share a backup folder (generations would mix
    // and the displaced-file carry-over would copy a path onto itself).
    let mut dir = root.join(&ts);
    let mut n = 0;

    while dir.exists() {
        n += 1;
        dir = root.join(format!("{ts}-{n}"));
    }

    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}
