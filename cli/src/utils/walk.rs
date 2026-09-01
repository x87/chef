use std::{
    fs::{canonicalize, copy, create_dir_all, read_dir},
    path::{Path, PathBuf},
};

/// Every file under `root`, recursively. Unreadable subdirectories are
/// skipped and the result is globally sorted for deterministic order.
pub fn files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Copy `src` into `dst`, mirroring subdirectories (empty ones included).
/// Errors propagate - deploy relies on this for transactional staging.
pub fn copy_tree(src: &Path, dst: &Path) -> anyhow::Result<()> {
    create_dir_all(dst)?;
    for sources in read_dir(src)?.flatten() {
        let sp = sources.path();
        let dp = dst.join(sources.file_name());

        if sp.is_dir() {
            copy_tree(&sp, &dp)?;
        } else {
            copy(&sp, &dp)?;
        }
    }
    Ok(())
}

/// Copy everything under `backup_dir` back into `game_dir`, skipping
/// `skip` paths. Returns the relative paths actually restored.
pub fn restore_tree(backup_dir: &Path, game_dir: &Path, skip: &[String]) -> Vec<String> {
    let mut restored = Vec::new();
    for p in files(backup_dir) {
        let rel = match p.strip_prefix(backup_dir) {
            Ok(r) => r.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        if skip.contains(&rel) {
            continue;
        }
        let abs = game_dir.join(&rel);
        let _ = create_dir_all(abs.parent().unwrap_or_else(|| Path::new(".")));
        if copy(&p, &abs).is_ok() {
            restored.push(rel);
        }
    }
    restored
}

/// Remove `dir` and empty ancestors up to (but not including) `stop_at`.
/// Depth-first: children go before parents.
pub fn prune_empty_tree(dir: &Path, stop_at: &Path) {
    if !dir.is_dir() || paths_equal(dir, stop_at) {
        return;
    }
    // Depth-first: clean children first so parents can become empty.
    let children: Vec<PathBuf> = read_dir(dir)
        .map(|it| {
            it.flatten()
                .filter_map(|e| {
                    let p = e.path();
                    if p.is_dir() { Some(p) } else { None }
                })
                .collect()
        })
        .unwrap_or_default();

    for child in children {
        prune_empty_tree(&child, stop_at);
    }

    let empty = std::fs::read_dir(dir)
        .map(|mut i| i.next().is_none())
        .unwrap_or(false);

    if empty {
        let _ = std::fs::remove_dir(dir);
    }
}

/// Path equality that resolves symlinks where possible, falling back to a
/// plain string comparison.
pub fn paths_equal(a: &Path, b: &Path) -> bool {
    match (canonicalize(a), canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}
