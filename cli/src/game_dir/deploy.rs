//! Staged deployment transaction: stage -> backup occupant -> remove
//! occupant -> move into place -> write state, with rollback on failure
//! and the undo-script recording that lets `chef remove` undo it.

use std::path::{Path, PathBuf};

use anyhow::Context;
use log::warn;

use crate::packages::ResolvedVersion;
use crate::utils::walk::copy_tree;

use super::backup::{backup_root_for, new_backup_dir};
use super::remove::remove_install_files;
use super::state::{Install, ManagedFile, StateFile};
use super::undo::Op;
use super::{DigestVerdict, compare_digest, dir_hash_key, unix_now};

// ---------------------------------------------------------------------------
// Staged deployment transaction
// ---------------------------------------------------------------------------

/// One file staged for deployment.
#[derive(Debug, Clone)]
pub struct DeployFile {
    /// Destination path relative to the game root (always `/`-separated).
    pub dest_rel: String,
    /// Source path in the store.
    pub src: PathBuf,
    pub sha256: String,
}

pub struct DeployRequest<'a> {
    pub product: &'a ResolvedVersion,
    /// Ids occupying the same slot (the product's id plus its `replaces`
    /// partners); any existing install among them is evicted first.
    pub slot: &'a [String],
    pub version: &'a str,
    pub files: &'a [DeployFile],
    pub dry_run: bool,
}

pub struct DeployOutcome {
    pub replaced: Option<Install>,
    pub preserved_modified: Vec<String>,
    /// User files backed up so the deploy could proceed; restored by
    /// 'chef remove'.
    pub displaced: Vec<String>,
}

/// Execute the staged transaction:
/// stage -> backup occupant -> remove occupant -> move staged into place ->
/// write state atomically. On failure after backup: restore snapshots, delete
/// staged files, leave previous state untouched, exit 1 with a report.
///
/// Caller must hold the game-dir lock.
pub fn deploy(game_dir: &Path, req: DeployRequest<'_>) -> anyhow::Result<DeployOutcome> {
    let mut state = StateFile::load()?;
    let key = dir_hash_key(game_dir);

    let id = req.product.id.as_str();

    // Collision policy: files not managed by chef are backed up and their
    // paths taken; 'chef remove' restores them. Compare case-insensitively -
    // game directories live on case-insensitive filesystems and manifests
    // may differ in case from recorded paths. Managed paths are the union
    // across the whole slot (every install this deploy replaces).
    let existing_paths: Vec<String> = req
        .slot
        .iter()
        .filter_map(|sid| state.install_of(&key, sid))
        .flat_map(|i| i.managed_paths().into_iter().map(|p| p.to_lowercase()))
        .collect();
    let mut collisions = Vec::new();

    for f in req.files {
        let dest = game_dir.join(&f.dest_rel);
        if dest.exists()
            && !existing_paths
                .iter()
                .any(|p| *p == f.dest_rel.to_lowercase())
        {
            collisions.push(f.dest_rel.clone());
        }
    }

    collisions.sort();

    if req.dry_run {
        return Ok(DeployOutcome {
            replaced: None,
            preserved_modified: vec![],
            displaced: vec![],
        });
    }

    // 1. Stage new payload copies.
    let staging = tempfile::tempdir()?;

    for f in req.files {
        let tmp_target = staging.path().join(&f.dest_rel);
        std::fs::create_dir_all(tmp_target.parent().unwrap_or_else(|| Path::new(".")))?;
        std::fs::copy(&f.src, &tmp_target)?;
    }

    let previous = req
        .slot
        .iter()
        .filter_map(|sid| state.install_of(&key, sid).cloned())
        .next();

    // 2. Snapshot the current occupant, skipping user-modified files (sec.5.4).
    let mut preserved: Vec<String> = Vec::new();
    let mut backed_up: Vec<(String, PathBuf)> = Vec::new();
    let backup_dir = if previous.is_some() || !collisions.is_empty() {
        Some(new_backup_dir(game_dir)?)
    } else {
        None
    };

    if let Some(prev) = &previous {
        for mf in &prev.files {
            let abs = game_dir.join(&mf.path);
            if !abs.exists() {
                continue;
            }
            if matches!(compare_digest(&abs, &mf.sha256), DigestVerdict::Match) {
                let snap = backup_dir.as_ref().unwrap().join(&mf.path);
                std::fs::create_dir_all(snap.parent().unwrap_or_else(|| Path::new(".")))?;
                std::fs::copy(&abs, &snap).with_context(|| {
                    format!("snapshotting {} -> {}", abs.display(), snap.display())
                })?;
                backed_up.push((mf.path.clone(), snap));
            } else {
                preserved.push(mf.path.clone());
            }
        }
    }

    // Snapshot the user files we are about to displace.
    for rel in &collisions {
        let bdir_ref = backup_dir.as_ref().unwrap();
        let abs = game_dir.join(rel);
        let snap = bdir_ref.join(rel);
        std::fs::create_dir_all(snap.parent().unwrap_or_else(|| Path::new(".")))?;
        std::fs::copy(&abs, &snap).with_context(|| format!("backing up {rel}"))?;
        backed_up.push((rel.clone(), snap));
    }

    // Carry over snapshots of user files displaced by earlier installs of
    // this package, so a single backup folder serves this generation.
    let mut displaced: Vec<String> = collisions.clone();

    if let Some(prev) = &previous
        && !prev.displaced.is_empty()
    {
        let old_root = prev
            .backup
            .as_ref()
            .map(|b| backup_root_for(game_dir).join(b));
        for rel in &prev.displaced {
            if displaced.iter().any(|d| d.eq_ignore_ascii_case(rel)) {
                continue;
            }
            let Some(old_root) = &old_root else {
                continue;
            };
            let from = old_root.join(rel);
            if from.exists() {
                let to = backup_dir.as_ref().unwrap().join(rel);
                std::fs::create_dir_all(to.parent().unwrap_or_else(|| Path::new(".")))?;
                std::fs::copy(&from, &to)?;
                displaced.push(rel.clone());
            }
        }
    }

    // Record the backup folder name so `remove` can restore it (sec.5.4).
    let backup_name = backup_dir
        .as_ref()
        .and_then(|p| p.file_name().map(|f| f.to_string_lossy().to_string()));

    let owned_dirs = owned_dirs_of(req.files);

    // Undo script: record every filesystem operation this deploy performs,
    // built after all snapshot + carry-over work so each `before` refers to
    // the final backup content (the carried-over user snapshot wins over an
    // occupant snapshot at the same path, restoring the user's own file on
    // remove). `chef remove` plays this back in reverse.
    let mut script: Vec<Op> = Vec::new();
    let snap_of = |b: &Path, rel: &str| b.join(rel).exists().then(|| rel.to_string());
    for f in req.files {
        let before = backup_dir.as_deref().and_then(|b| snap_of(b, &f.dest_rel));
        script.push(Op::Overwrite {
            to: f.dest_rel.clone(),
            digest: f.sha256.clone(),
            before,
        });
    }
    if let Some(prev) = &previous {
        let new_paths: Vec<String> = req
            .files
            .iter()
            .map(|f| f.dest_rel.to_lowercase())
            .collect();
        for mf in &prev.files {
            // Paths the new version ships are covered by the Overwrite ops
            // above. Files the user modified are never deleted (preserved),
            // so they get no op either.
            if new_paths.iter().any(|p| *p == mf.path.to_lowercase()) {
                continue;
            }
            let abs = game_dir.join(&mf.path);
            if !abs.exists() || matches!(compare_digest(&abs, &mf.sha256), DigestVerdict::Modified)
            {
                continue;
            }
            let before = backup_dir.as_deref().and_then(|b| snap_of(b, &mf.path));
            script.push(Op::Delete {
                path: mf.path.clone(),
                before,
            });
        }
    }

    // Rollback bookkeeping: snapshots of occupant files + newly deployed files.
    let new_install = Install {
        package: req.product.id.clone(),
        version: req.version.to_string(),
        files: req
            .files
            .iter()
            .map(|f| ManagedFile {
                path: f.dest_rel.clone(),
                source: f
                    .src
                    .strip_prefix(crate::packages::store_root())
                    .map(|r| format!("store/{}", r.to_string_lossy().replace('\\', "/")))
                    .unwrap_or_else(|_| f.src.display().to_string()),
                sha256: f.sha256.clone(),
            })
            .collect(),
        owned_dirs: owned_dirs.clone(),
        backup: backup_name,
        displaced: displaced.clone(),
        script,
        at: unix_now(),
    };

    // 3-5. Remove occupant -> move staged into place -> write state atomically.
    // A failure anywhere in here triggers the rollback below.
    let result = (|| -> anyhow::Result<()> {
        if let Some(prev) = &previous {
            remove_install_files(game_dir, prev, &preserved)?;
        }
        copy_tree(staging.path(), game_dir)?;
        let dir_state = state.dirs.entry(key.clone()).or_default();
        // Evict every slot occupant, then record the new product.
        for sid in req.slot {
            dir_state.installs.remove(sid);
        }
        dir_state.installs.insert(id.to_string(), new_install);
        state.save()?;
        Ok(())
    })();

    if let Err(e) = result {
        #[cfg(test)]
        crate::emit::dbg_trace(format_args!("deploy FAILED id={id}: {e:#}"));
        // Rollback: restore snapshots over pre-existing files, delete newly
        // deployed files, leave previous state untouched.
        let mut report = vec![format!("deployment failed: {e:#}")];

        for (rel, snap) in &backed_up {
            let abs = game_dir.join(rel);
            let _ = std::fs::create_dir_all(abs.parent().unwrap_or_else(|| Path::new(".")));
            if std::fs::copy(snap, &abs).is_ok() {
                report.push(format!("restored snapshot: {rel}"));
            } else {
                report.push(format!("FAILED to restore snapshot: {rel}"));
            }
        }

        for f in req.files {
            let abs = game_dir.join(&f.dest_rel);
            if abs.exists() {
                let _ = std::fs::remove_file(&abs);
                report.push(format!("deleted staged file: {}", f.dest_rel));
            }
        }

        anyhow::bail!(
            "rolled back - previous state intact\n{}",
            report.join("\n  ")
        );
    }

    #[cfg(test)]
    crate::emit::dbg_trace(format_args!(
        "deploy OK id={id} version={} game={} home={} key={key} state_dirs={}",
        req.version,
        game_dir.display(),
        crate::packages::chef_home().display(),
        state.dirs.len()
    ));

    // 6. Release happens via Drop.
    for p in &preserved {
        warn!("preserved user-modified file: {p}");
    }

    Ok(DeployOutcome {
        replaced: previous,
        preserved_modified: preserved,
        displaced,
    })
}

/// Top-level directories used by the staged files (owned by the package
/// and pruned when empty on replace/remove). Derived from the payload
/// paths - the catalog declares nothing extra.
fn owned_dirs_of(files: &[DeployFile]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for f in files {
        let Some(first) = f.dest_rel.split('/').next() else {
            continue;
        };

        if !first.is_empty()
            && f.dest_rel.contains('/')
            && !out.iter().any(|d| d.eq_ignore_ascii_case(first))
        {
            out.push(first.to_string());
        }
    }
    out
}
