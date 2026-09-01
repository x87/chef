//! The chef data home: where state, mirrors, store and backups live, plus
//! the test seam that redirects it.

use std::path::PathBuf;
use std::sync::Mutex;

use lazy_static::lazy_static;

// ---------------------------------------------------------------------------
// Data home / well-known paths
// ---------------------------------------------------------------------------

lazy_static! {
    static ref HOME_OVERRIDE: Mutex<Option<PathBuf>> = Mutex::new(None);
}

/// Test seam: point chef's data home at a sandbox without any environment
/// variable or CLI surface. Held by the integration tests for the lifetime
/// of each `TestEnv`; cleared by `clear_home_override()`.
pub fn set_home_override(p: PathBuf) {
    *HOME_OVERRIDE.lock().unwrap() = Some(p);
}

/// Clear the home override set by `set_home_override`.
pub fn clear_home_override() {
    *HOME_OVERRIDE.lock().unwrap() = None;
}

fn home_override() -> Option<PathBuf> {
    HOME_OVERRIDE.lock().unwrap().clone()
}

/// Data home: the platform app-data directory plus "Chef"
/// (`%LOCALAPPDATA%\Chef` on Windows), overridable only by the test seam
/// `set_home_override`.
pub fn chef_home() -> PathBuf {
    if let Some(p) = home_override() {
        return p;
    }
    dirs::data_local_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local/share")
        })
        .join(APP_DIR_NAME)
}

/// Data-home folder name: lowercase `chef` on Unix (matches the installer's
/// `~/.local/share/chef` default), `Chef` on Windows (`%LOCALAPPDATA%\Chef`).
#[cfg(unix)]
const APP_DIR_NAME: &str = "chef";
#[cfg(not(unix))]
const APP_DIR_NAME: &str = "Chef";

pub(crate) fn packages_mirror() -> PathBuf {
    chef_home().join("packages.json")
}

pub(crate) fn lock_mirror() -> PathBuf {
    chef_home().join("packages.lock")
}
