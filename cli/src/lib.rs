use crate::emit::ChefError;
use std::path::PathBuf;

pub mod cli;
pub mod commands;
pub mod emit;
pub mod game_dir;
pub mod handlers;
pub mod packages;
pub mod utils;

const HISTORY_MAX_BYTES: u64 = 5 * 1024 * 1024;

#[cfg(test)]
mod tests;

pub type Result<T> = anyhow::Result<T, ChefError>;

/// Run one CLI invocation; used by integration tests.
pub fn run_test(cmd: cli::Cmd) -> Result<()> {
    run_mode(cmd, false)
}

/// Run one CLI invocation. `json` switches every command (result and
/// errors) to structured output for scripted invocations.
pub fn run_mode(cmd: cli::Cmd, json: bool) -> Result<()> {
    commands::dispatch(cmd, json)
}

/// `%LOCALAPPDATA%\Chef\history.log` (or the test home override).
pub fn history_path() -> PathBuf {
    packages::chef_home().join("history.log")
}

/// Open the run history log for appending; rotates to `history.log.old`
/// when the current file exceeds the cap. `None` when the data home
/// cannot be created.
pub fn open_history_log() -> Option<std::fs::File> {
    let path = history_path();
    std::fs::create_dir_all(path.parent()?).ok()?;
    if let Ok(meta) = std::fs::metadata(&path)
        && meta.len() > HISTORY_MAX_BYTES
    {
        let _ = std::fs::rename(&path, path.with_extension("log.old"));
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
}
