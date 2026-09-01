//! On-disk presence of recorded installs: what is present, missing or
//! moved, and pruning of installs whose files are all gone.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::dir_hash_key;
use super::state::{Install, StateFile};

// ---------------------------------------------------------------------------
// On-disk presence of a recorded install
// ---------------------------------------------------------------------------

/// Classification of a recorded install's files on disk. `present` and
/// `missing` are complementary; a missing file is `moved` when its digest
/// matches a file found elsewhere in the game root.
#[derive(Debug, Default)]
pub struct InstallCheck {
    /// Relative paths present at their recorded location.
    pub present: Vec<String>,
    /// Recorded relative paths that are absent from the game dir.
    pub missing: Vec<String>,
    /// (recorded path, found absolute path) for each missing file whose
    /// digest appeared elsewhere in the game root.
    pub moved: Vec<(String, PathBuf)>,
}

impl InstallCheck {
    pub fn all_present(&self) -> bool {
        self.missing.is_empty()
    }

    /// Nothing of the install remains in the game dir (and nothing was
    /// found moved) - the recording is stale.
    pub fn fully_gone(&self) -> bool {
        self.present.is_empty() && self.moved.is_empty()
    }
}

/// Classify a recorded install against the game dir: which recorded paths
/// are present, which are missing, and whether any missing file was moved
/// (digest match elsewhere in the root). Root-only, matching chef v1's
/// discovery scope.
pub fn check_install(game_dir: &Path, inst: &Install) -> InstallCheck {
    let mut out = InstallCheck::default();

    for mf in &inst.files {
        if game_dir.join(&mf.path).exists() {
            out.present.push(mf.path.clone());
        } else {
            out.missing.push(mf.path.clone());
        }
    }

    if out.missing.is_empty() {
        return out;
    }

    // Index root files by digest once (first match wins, mirroring the
    // older per-file scans) and look the missing digests up in it.
    let expected: BTreeMap<&str, &str> = inst
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.sha256.as_str()))
        .collect();
    let mut by_digest: BTreeMap<String, PathBuf> = BTreeMap::new();

    if let Ok(entries) = std::fs::read_dir(game_dir) {
        for e in entries.flatten() {
            let p = e.path();
            if !p.is_file() {
                continue;
            }
            if let Some(d) = crate::utils::fs::sha256_file(&p).ok()
                && !by_digest.contains_key(&d)
            {
                by_digest.insert(d, p);
            }
        }
    }

    for rel in &out.missing {
        if let Some(exp) = expected.get(rel.as_str())
            && let Some(found) = by_digest.get(*exp)
        {
            out.moved.push((rel.clone(), found.clone()));
        }
    }
    out
}

/// Live-refresh pass: drop recorded installs whose files are all gone and
/// not found (moved) elsewhere in the root, so manual deletes stop showing
/// as installed. Returns true when anything was pruned.
pub fn prune_stale_state(game_dir: &Path, state: &mut StateFile) -> bool {
    let key = dir_hash_key(game_dir);
    let Some(dir_state) = state.dirs.get(&key) else {
        return false;
    };
    let to_prune: Vec<String> = dir_state
        .installs
        .values()
        .filter(|inst| check_install(game_dir, inst).fully_gone())
        .map(|inst| inst.package.clone())
        .collect();

    if to_prune.is_empty() {
        return false;
    }

    let dir_state = state.dirs.get_mut(&key).expect("entry exists");

    for id in &to_prune {
        dir_state.installs.remove(id);
    }

    if dir_state.installs.is_empty() {
        state.dirs.remove(&key);
    }
    true
}
