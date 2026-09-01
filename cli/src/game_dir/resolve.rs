//! Game directory resolution and safety guards.

use std::path::{Path, PathBuf};

use anyhow::bail;

use crate::packages::chef_home;
use crate::utils::walk::paths_equal;

// ---------------------------------------------------------------------------
// Directory resolution
// ---------------------------------------------------------------------------

/// `--dir` -> cwd; canonicalized before use.
pub fn resolve_game_dir(flag: Option<&Path>) -> anyhow::Result<PathBuf> {
    let chosen: PathBuf = flag
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    // Canonicalize; create the dir only if it plausibly exists as a target.
    // `dunce` strips the `\\?\` prefix std adds on Windows so state keys
    // and output stay readable.
    let canon = if chosen.exists() {
        dunce::canonicalize(&chosen)?
    } else {
        bail!("game directory {} does not exist", chosen.display())
    };
    safety_guard(&canon)?;
    #[cfg(test)]
    crate::emit::dbg_trace(format_args!(
        "resolve_game_dir in={} -> out={} key={}",
        chosen.display(),
        canon.display(),
        super::dir_hash(&canon)
    ));
    Ok(canon)
}

/// Safe guards: never treat the data home, the user's
/// home or the filesystem root as a game directory.
fn safety_guard(dir: &Path) -> anyhow::Result<()> {
    let bad = [
        ("the data home", Some(chef_home())),
        ("the user home", dirs::home_dir()),
        ("the filesystem root", Some(PathBuf::from("/"))),
    ];
    for (label, p) in bad.iter() {
        if let Some(p) = p
            && paths_equal(dir, p)
        {
            bail!(
                "refusing to operate on {label} ({}) as a game directory",
                p.display()
            );
        }
    }
    Ok(())
}
