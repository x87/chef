//! Remove: reverse playback of an install's undo script (with a legacy
//! files/backup fallback for installs recorded before scripts existed).

use std::path::Path;

use anyhow::bail;

use crate::utils::walk::{prune_empty_tree, restore_tree};

use super::backup::backup_root_for;
use super::state::{Install, StateFile};
use super::undo::revert_op;
use super::{DigestVerdict, compare_digest, dir_hash_key};

/// Remove a managed installation. Installs recorded with an undo script
/// (every install since the script model landed) are removed by playing the
/// script back in reverse - the exact inverse of the transformation the
/// install performed. Installs recorded before that fall back to the legacy
/// files/backup/displaced logic, which is behaviourally equivalent for
/// simple deploys. Caller must hold the lock.
pub fn remove_install(
    game_dir: &Path,
    id: &str,
    expect_version: Option<&str>,
) -> anyhow::Result<(Install, Vec<String>, Vec<String>)> {
    let mut state = StateFile::load()?;
    let key = dir_hash_key(game_dir);
    let inst = state
        .dirs
        .get(&key)
        .and_then(|d| d.installs.get(id))
        .cloned()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no installation with id '{id}' found in {}",
                game_dir.display()
            )
        })?;

    if let Some(v) = expect_version {
        let v_norm = v.strip_prefix('v').unwrap_or(v).trim_end_matches('.');
        let matches = inst.version == v_norm
            || inst.version == v
            || inst.version.starts_with(&format!("{v_norm}."));
        if !matches {
            bail!(
                "installed version of {} is {}, not {v}",
                inst.package,
                inst.version
            );
        }
    }

    let (preserved, restored) = if inst.script.is_empty() {
        remove_install_legacy(game_dir, &inst)
    } else {
        // Play the uninstall script back in reverse. Each revert restores
        // the pre-install state at that path; deployed files the user
        // changed after install are handed back untouched instead.
        let mut preserved = Vec::new();
        let mut restored = Vec::new();
        let backup_dir = inst
            .backup
            .as_ref()
            .map(|b| backup_root_for(game_dir).join(b));
        for op in inst.script.iter().rev() {
            revert_op(
                op,
                game_dir,
                backup_dir.as_deref(),
                &mut preserved,
                &mut restored,
            );
        }
        preserved.sort();
        restored.sort();
        (preserved, restored)
    };

    // Prune owned directories (only when empty).
    prune_owned_dirs(game_dir, &inst);

    state
        .dirs
        .get_mut(&key)
        .expect("entry exists")
        .installs
        .remove(id);
    state.save()?;

    Ok((inst, preserved, restored))
}

/// Legacy removal for installs recorded before chef tracked undo scripts:
/// delete managed files with matching digests (preserving user edits),
/// restore the backup tree, then restore displaced user snapshots into
/// free paths.
fn remove_install_legacy(game_dir: &Path, inst: &Install) -> (Vec<String>, Vec<String>) {
    let mut preserved = Vec::new();

    for mf in &inst.files {
        let abs = game_dir.join(&mf.path);
        if !abs.exists() {
            continue;
        }
        if matches!(compare_digest(&abs, &mf.sha256), DigestVerdict::Match) {
            let _ = std::fs::remove_file(&abs);
        } else {
            preserved.push(mf.path.clone());
        }
    }

    // Restore backup where applicable - never over a user-modified file.
    // Report every file that actually came back, exactly once.
    let mut restored: Vec<String> = Vec::new();
    if let Some(bdir) = &inst.backup {
        let bpath = backup_root_for(game_dir).join(bdir);
        if bpath.exists() {
            restored.extend(restore_tree(&bpath, game_dir, &preserved));
        }
        // Snapshots of displaced user files not already covered above
        // (e.g. when the backup dir was pruned).
        for rel in &inst.displaced {
            if restored.contains(rel) || preserved.contains(rel) {
                continue;
            }
            if let Some(snap) = bpath.join(rel).exists().then(|| bpath.join(rel)) {
                let abs = game_dir.join(rel);
                if !abs.exists() {
                    let _ = std::fs::create_dir_all(abs.parent().unwrap_or_else(|| Path::new(".")));
                    if std::fs::copy(&snap, &abs).is_ok() {
                        restored.push(rel.clone());
                    }
                }
            }
        }
    }

    (preserved, restored)
}

pub(crate) fn remove_install_files(
    game_dir: &Path,
    inst: &Install,
    preserved: &[String],
) -> anyhow::Result<()> {
    for mf in &inst.files {
        if preserved.contains(&mf.path) {
            continue;
        }
        let abs = game_dir.join(&mf.path);
        let _ = std::fs::remove_file(&abs);
    }
    prune_owned_dirs(game_dir, inst);
    Ok(())
}

fn prune_owned_dirs(game_dir: &Path, inst: &Install) {
    for od in &inst.owned_dirs {
        let dir = game_dir.join(od.replace('/', std::path::MAIN_SEPARATOR_STR));
        prune_empty_tree(&dir, game_dir);
    }
}
