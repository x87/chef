//! The undo script: every filesystem operation an install performs,
//! plus the reverse playback (`revert_op`) `chef remove` uses.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Undo script: every filesystem operation an install performed, each with
// its exact revert. `chef remove` is a pure playback of this script in
// reverse order, so no matter how the install transformed the game dir
// (adds, overwrites, renames, deletes), removing it returns the dir to the
// exact pre-install state.
// ---------------------------------------------------------------------------

/// One recorded filesystem operation with its revert (the action `remove`
/// plays back). Paths inside the game dir are `/`-separated and relative to
/// the game root; snapshot paths are relative to the install's backup folder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Op {
    /// Wrote the payload content to `to`, replacing whatever occupied the
    /// path (a previous install's file, a user file, or nothing). `digest`
    /// is the sha256 of the written bytes - remove uses it to detect later
    /// user edits and hand those back instead of reverting them.
    /// `before` is the snapshot of the prior occupant, if there was one.
    ///
    /// Revert: restore `before` over `to`, or delete `to` when there was
    /// no prior occupant (add X -> delete X; replace X with X2 -> replace
    /// X2 with X).
    Overwrite {
        to: String,
        digest: String,
        #[serde(default)]
        before: Option<String>,
    },
    /// Deleted an occupant file that the new version no longer ships.
    /// `before` is the snapshot of the deleted content.
    ///
    /// Revert: recreate the file from `before` (delete X -> create X).
    Delete {
        path: String,
        #[serde(default)]
        before: Option<String>,
    },
    /// Moved a file from `from` to `to`.
    ///
    /// Revert: rename `to` back to `from` (rename X to Y -> rename Y to X).
    /// Today's deployments copy payload files into place and never emit a
    /// rename; the variant exists so the model stays complete when install
    /// transformations (e.g. postinstall moves of existing game files)
    /// arrive.
    Rename { from: String, to: String },
}

/// Apply one recorded op's revert. `preserved` collects deployed files the
/// user modified after install (handed back, never reverted); `restored`
/// collects every path the playback brings back from a snapshot (or renames
/// back into place).
pub(crate) fn revert_op(
    op: &Op,
    game_dir: &Path,
    backup_dir: Option<&Path>,
    preserved: &mut Vec<String>,
    restored: &mut Vec<String>,
) {
    match op {
        Op::Overwrite { to, digest, before } => {
            let abs = game_dir.join(to);
            let untouched =
                crate::utils::fs::sha256_file(&abs).ok().as_deref() == Some(digest.as_str());
            if !untouched {
                // The deployed file was edited (or a user file now sits at
                // the path). Hand the path back to the user untouched -
                // unless the path is empty, in which case restoring the
                // snapshot is safe and returns the pre-install state.
                if abs.exists() {
                    preserved.push(to.clone());
                } else if let Some(snap) = snapshot(backup_dir, before.as_deref())
                    && copy_snapshot(&snap, &abs)
                {
                    restored.push(to.clone());
                }
                return;
            }
            match before {
                Some(_) => {
                    if let Some(snap) = snapshot(backup_dir, before.as_deref()) {
                        // replace X with X2 -> replace X2 with X
                        if copy_snapshot(&snap, &abs) {
                            restored.push(to.clone());
                        }
                    } else if abs.exists() {
                        // Snapshot pruned - fall back to a plain delete.
                        let _ = std::fs::remove_file(&abs);
                    }
                }
                None => {
                    // add X -> delete X
                    if abs.exists() {
                        let _ = std::fs::remove_file(&abs);
                    }
                }
            }
        }
        Op::Delete { path, before } => {
            // delete X -> create X (from the snapshot of the deleted file).
            if let Some(snap) = snapshot(backup_dir, before.as_deref()) {
                let abs = game_dir.join(path);
                if copy_snapshot(&snap, &abs) {
                    restored.push(path.clone());
                }
            }
        }
        Op::Rename { from, to } => {
            // rename X to Y -> rename Y to X
            let src = game_dir.join(to);
            let dst = game_dir.join(from);
            if src.exists() {
                if let Some(parent) = dst.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if std::fs::rename(&src, &dst).is_ok() {
                    restored.push(from.clone());
                }
            }
            // Renamed target missing (user moved or deleted it): nothing to
            // do - there is no state to revert.
        }
    }
}

/// Resolve a recorded snapshot path inside the install's backup folder.
fn snapshot(backup_dir: Option<&Path>, rel: Option<&str>) -> Option<PathBuf> {
    let rel = rel?;
    let b = backup_dir?;
    let p = b.join(rel);
    p.exists().then_some(p)
}

/// Copy one file into place, creating parent directories; true on success.
fn copy_snapshot(from: &Path, to: &Path) -> bool {
    if let Some(parent) = to.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return false;
    }
    std::fs::copy(from, to).is_ok()
}
