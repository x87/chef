//! Remove: reverse playback of an install's undo script (with a legacy
//! files/backup fallback for installs recorded before scripts existed).

use std::path::Path;

use anyhow::bail;

use crate::utils::walk::{prune_empty_tree, restore_tree};

use super::backup::backup_root_for;
use super::state::{Install, ManagedFile, StateFile};
use super::undo::revert_op;
use super::{DigestVerdict, compare_digest, dir_hash_key};

/// What removing one install did to the game directory and to the tracked
/// state (`chef remove` reports it).
pub struct RemoveReport {
    /// Files the user changed after install - handed back, not deleted.
    pub preserved: Vec<String>,
    /// The user's own files put back from the backups.
    pub restored: Vec<String>,
    /// The generation this install had replaced, tracked again because the
    /// playback put its payload back on disk. `None` when the package is
    /// gone for good.
    pub reinstated: Option<Install>,
}

/// Remove a managed installation. Installs recorded with an undo script
/// (every install since the script model landed) are removed by playing the
/// script back in reverse - the exact inverse of the transformation the
/// install performed. Installs recorded before that fall back to the legacy
/// files/backup/displaced logic, which is behaviourally equivalent for
/// simple deploys. Caller must hold the lock.
///
/// The playback can put a replaced generation's payload back on disk; that
/// generation is then reinstated in the state file (`RemoveReport::reinstated`)
/// so `which` keeps reporting it as chef-managed instead of leaving an
/// untracked file behind.
pub fn remove_install(
    game_dir: &Path,
    id: &str,
    expect_version: Option<&str>,
) -> anyhow::Result<(Install, RemoveReport)> {
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

    // Playback put back two kinds of file: the user's own displaced files,
    // and the payload of the generation this install replaced (the script
    // reverts to the state right before this install). The first kind is
    // the user's again; the second is still chef's, so that generation's
    // record is reinstated instead of being forgotten with this one.
    // Legacy installs (no script) never recorded a predecessor: their
    // restore report is passed through unchanged.
    let (user_restored, previous_restored): (Vec<String>, Vec<String>) = if inst.script.is_empty() {
        (restored, Vec::new())
    } else {
        restored
            .into_iter()
            .partition(|p| inst.displaced.iter().any(|d| d.eq_ignore_ascii_case(p)))
    };
    let reinstated = inst
        .previous
        .as_deref()
        .and_then(|prev| reinstate(prev, &previous_restored));

    // Prune owned directories (only when empty).
    prune_owned_dirs(game_dir, &inst);

    let dir_state = state.dirs.get_mut(&key).expect("entry exists");
    dir_state.installs.remove(id);
    if let Some(prev) = &reinstated {
        // Replaced ids may differ (slot partners, e.g. UAL evicting SAL).
        dir_state
            .installs
            .insert(prev.package.clone(), prev.clone());
    }
    state.save()?;

    Ok((
        inst,
        RemoveReport {
            preserved,
            restored: user_restored,
            reinstated,
        },
    ))
}

/// Rebuild the replaced generation for the state file: exactly the files the
/// playback put back (its own payload, not files that turned out to be the
/// user's), with the ops that remove those files kept so it can be removed
/// like any other install. `None` when nothing of it is on disk.
fn reinstate(prev: &Install, restored: &[String]) -> Option<Install> {
    let keep = |path: &str| restored.iter().any(|r| r.eq_ignore_ascii_case(path));
    let files: Vec<ManagedFile> = prev
        .files
        .iter()
        .filter(|f| keep(&f.path))
        .cloned()
        .collect();
    if files.is_empty() {
        return None;
    }
    Some(Install {
        package: prev.package.clone(),
        version: prev.version.clone(),
        owned_dirs: super::owned_dirs_of(files.iter().map(|f| f.path.as_str())),
        // Keep the snapshots that generation took: they are what `remove`
        // must put back when *it* is removed.
        backup: prev.backup.clone(),
        displaced: prev.displaced.iter().filter(|d| keep(d)).cloned().collect(),
        script: prev
            .script
            .iter()
            .filter(|op| keep(op.path()))
            .cloned()
            .collect(),
        previous: prev.previous.clone(),
        files,
        at: prev.at,
    })
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
