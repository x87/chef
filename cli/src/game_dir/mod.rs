//! Game-directory layer: target resolution and safety guards, game
//! detection, persistent install state, the advisory per-game-dir lock,
//! backup snapshots, staged deploy transactions, and the undo-script model
//! that makes `chef remove` a pure reverse playback of an install's
//! filesystem operations.
//!
//! The public API is re-exported here so external callers keep using
//! `crate::game_dir::*` paths.

pub mod backup;
pub mod check;
pub mod deploy;
pub mod detect;
pub mod lock;
pub mod remove;
pub mod resolve;
pub mod state;
pub mod undo;

pub use backup::backup_root_for;
pub use check::{InstallCheck, check_install, prune_stale_state};
pub use deploy::{DeployFile, DeployOutcome, DeployRequest, deploy};
pub use detect::{DetectedGame, detect_game, family_title};
pub use lock::Lock;
pub use remove::{RemoveReport, remove_install};
pub use resolve::resolve_game_dir;
pub use state::{
    GameDirState, Install, ManagedFile, SUPPORTED_STATE_SCHEMA, StateFile, state_path,
};
pub use undo::Op;

use std::path::Path;

/// Top-level directories used by deploy payload paths (owned by the
/// package and pruned when empty on replace/remove). Derived from the
/// payload paths - the catalog declares nothing extra.
pub(crate) fn owned_dirs_of<'a>(paths: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for path in paths {
        let Some(first) = path.split('/').next() else {
            continue;
        };
        if !first.is_empty()
            && path.contains('/')
            && !out.iter().any(|d| d.eq_ignore_ascii_case(first))
        {
            out.push(first.to_string());
        }
    }
    out
}

fn dir_hash(dir: &Path) -> String {
    // Canonicalize so every spelling of the same directory (raw path, 8.3
    // short names, `..` components, trailing separators, case) maps to one
    // state key - matching what `resolve_game_dir` feeds the commands. CI
    // runners set %TEMP% to a short name (e.g. C:\Users\RUNNER~1), so the
    // test harness's raw path and the CLI's canonical path used to diverge.
    let canon = dunce::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let mut h = crc32fast::Hasher::new();
    h.update(canon.to_string_lossy().to_lowercase().as_bytes());
    format!("{:08x}", h.finalize())
}

pub fn dir_hash_key(game_dir: &Path) -> String {
    dir_hash(game_dir)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

enum DigestVerdict {
    Match,
    Modified,
}

fn compare_digest(path: &Path, expected: &str) -> DigestVerdict {
    match crate::utils::fs::sha256_file(path) {
        Ok(got) if got == expected => DigestVerdict::Match,
        _ => DigestVerdict::Modified,
    }
}
