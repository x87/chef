//! Persistent install state (`state.json`): per-game-dir installs with
//! their managed files and undo scripts.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::bail;
use serde::{Deserialize, Serialize};

use crate::packages::chef_home;
use crate::utils::fs::write_atomic;

use super::undo::Op;

pub const SUPPORTED_STATE_SCHEMA: u32 = 2;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedFile {
    /// Relative path inside the game directory (`/`-separated).
    pub path: String,
    pub source: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Install {
    pub package: String,
    pub version: String,
    pub files: Vec<ManagedFile>,
    #[serde(default)]
    pub owned_dirs: Vec<String>,
    #[serde(default)]
    pub backup: Option<String>,
    /// User files that were displaced (backed up) so this install could
    /// take their paths; restored on remove. Snapshots live in this
    /// install's backup folder.
    #[serde(default)]
    pub displaced: Vec<String>,
    /// The uninstall script: every filesystem operation this install
    /// performed, played back in reverse by `chef remove`. Empty for
    /// installs recorded before chef tracked scripts (schema stayed 2;
    /// remove falls back to `files`/`backup`/`displaced` for those).
    #[serde(default)]
    pub script: Vec<Op>,
    pub at: u64,
}

impl Install {
    pub fn managed_paths(&self) -> Vec<String> {
        self.files.iter().map(|f| f.path.clone()).collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GameDirState {
    /// Keyed by the persistent package id - at most one entry per id.
    pub installs: BTreeMap<String, Install>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateFile {
    pub schema: u32,
    #[serde(default)]
    pub dirs: BTreeMap<String, GameDirState>,
}

pub fn state_path() -> PathBuf {
    chef_home().join("state.json")
}

impl StateFile {
    pub fn load() -> anyhow::Result<StateFile> {
        let path = state_path();

        #[cfg(test)]
        crate::emit::dbg_trace(format_args!(
            "StateFile::load path={} exists={}",
            path.display(),
            path.exists()
        ));

        if !path.exists() {
            return Ok(StateFile {
                schema: SUPPORTED_STATE_SCHEMA,
                dirs: BTreeMap::new(),
            });
        }

        let bytes = std::fs::read(&path)?;
        let st: StateFile = serde_json::from_slice(&bytes).map_err(|e| {
            anyhow::anyhow!(
                "chef state at {} is corrupt ({e}); inspect the backups folder in the chef data directory before deleting it",
                path.display()
            )
        })?;

        if st.schema > SUPPORTED_STATE_SCHEMA {
            bail!(
                "state schema {} is newer than supported {} - please upgrade chef",
                st.schema,
                SUPPORTED_STATE_SCHEMA
            );
        }
        #[cfg(test)]
        crate::emit::dbg_trace(format_args!(
            "StateFile::load -> dirs={} keys=[{}]",
            st.dirs.len(),
            st.dirs.keys().cloned().collect::<Vec<_>>().join(",")
        ));
        Ok(st)
    }

    pub fn save(&self) -> anyhow::Result<()> {
        #[cfg(test)]
        crate::emit::dbg_trace(format_args!(
            "StateFile::save path={} dirs={} keys=[{}] installs=[{}]",
            state_path().display(),
            self.dirs.len(),
            self.dirs.keys().cloned().collect::<Vec<_>>().join(","),
            self.dirs
                .values()
                .flat_map(|d| d.installs.keys())
                .cloned()
                .collect::<Vec<_>>()
                .join(",")
        ));
        write_atomic(&state_path(), &serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }

    pub fn dir_state(&self, dir_key: &str) -> &GameDirState {
        use std::sync::LazyLock;
        static EMPTY: LazyLock<GameDirState> = LazyLock::new(GameDirState::default);
        self.dirs.get(dir_key).unwrap_or(&EMPTY)
    }

    pub fn install_of(&self, dir_key: &str, id: &str) -> Option<&Install> {
        self.dirs.get(dir_key)?.installs.get(id)
    }

    /// All installs recorded for one game directory.
    pub fn installs_in(&self, dir_key: &str) -> Vec<&Install> {
        self.dir_state(dir_key).installs.values().collect()
    }
}
