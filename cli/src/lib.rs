pub mod cli;
pub mod commands;
pub mod game_dir;
pub mod match_names;
pub mod packages;
pub mod store;
pub mod utils;

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

#[cfg(test)]
mod tests;

/// Debug tracing for diagnosis (test builds only): writes `[chef-dbg]`
/// lines to stderr, which libtest shows in each failing test's captured
/// output. Never compiled into the shipping CLI binary.
#[cfg(test)]
pub fn dbg_trace(msg: impl std::fmt::Display) {
    eprintln!("[chef-dbg] {msg}");
}

/// Error taxonomy backing the exit codes from :
/// `0` success, `1` operational error, `2` ambiguous package match.
#[derive(Debug)]
pub enum ChefError {
    /// Ambiguous package name - candidates printed, exit code 2.
    Ambiguous(Vec<String>),
    /// Every other operational failure - exit code 1.
    Other(anyhow::Error),
    /// Failure(s) already reported to stderr by an inner loop that kept
    /// going; main exits 1 without printing again. Carries the first
    /// error so callers (tests) can still inspect it.
    Reported(Box<anyhow::Error>),
}

impl From<anyhow::Error> for ChefError {
    fn from(e: anyhow::Error) -> Self {
        ChefError::Other(e)
    }
}

impl std::fmt::Display for ChefError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChefError::Ambiguous(c) => {
                writeln!(f, "ambiguous package name - did you mean:")?;
                for c in c {
                    writeln!(f, "  {c}")?;
                }
                Ok(())
            }
            ChefError::Other(e) => write!(f, "{e:#}"),
            ChefError::Reported(_) => write!(f, "operation failed"),
        }
    }
}

impl std::error::Error for ChefError {}

pub type Result<T> = std::result::Result<T, ChefError>;

/// Run one CLI invocation in human mode; used by integration tests.
pub fn run(cmd: cli::Cmd) -> Result<()> {
    run_mode(cmd, false)
}

/// Run one CLI invocation. `json` switches every command (result and
/// errors) to structured output for scripted invocations.
pub fn run_mode(cmd: cli::Cmd, json: bool) -> Result<()> {
    commands::dispatch(cmd, json)
}

// --------------------------------------------------------------------------
// Run history log: every session appends its messages (and the command it
// ran) to `history.log` under the data home, for debugging. Rotates at a
// size cap so the file stays bounded.
// --------------------------------------------------------------------------

const HISTORY_MAX_BYTES: u64 = 5 * 1024 * 1024;

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

// --------------------------------------------------------------------------
// JSON emission and error reporting (also the seam tests capture through)
// --------------------------------------------------------------------------

/// Captured output for tests running with `--json` in-process.
#[derive(Default)]
pub struct CapturedOutput {
    /// pretty JSON documents printed to stdout.
    pub out: Vec<String>,
    /// JSON error objects printed to stderr.
    pub err: Vec<String>,
}

static CAPTURE: OnceLock<Mutex<Option<CapturedOutput>>> = OnceLock::new();

fn capture() -> &'static Mutex<Option<CapturedOutput>> {
    CAPTURE.get_or_init(|| Mutex::new(None))
}

/// Test seam: while a capture is installed, `emit_json` and
/// `emit_json_error` append instead of printing.
pub fn set_capture(c: CapturedOutput) {
    *capture().lock().unwrap() = Some(c);
}

/// Test seam: take the captured output; `None` while no capture is set.
pub fn take_capture() -> Option<CapturedOutput> {
    let mut g = capture().lock().unwrap();
    g.take()
}

/// Print a JSON document as the command result (stdout).
pub fn emit_json(v: &serde_json::Value) {
    let s = serde_json::to_string_pretty(v).unwrap();
    if let Some(c) = capture().lock().unwrap().as_mut() {
        c.out.push(s);
    } else {
        println!("{s}");
    }
}

/// Print one JSON error object (stderr), e.g. from an inner loop that
/// keeps going after failures.
pub fn emit_json_error(v: &serde_json::Value) {
    let s = serde_json::to_string(v).unwrap();
    if let Some(c) = capture().lock().unwrap().as_mut() {
        c.err.push(s);
    } else {
        eprintln!("{s}");
    }
}

/// JSON shape for any `chef` error: `{"error": ...}` plus `candidates`
/// for ambiguous package names.
pub fn json_error(e: &ChefError) -> String {
    match e {
        ChefError::Ambiguous(cands) => serde_json::json!({
            "error": "ambiguous package name - did you mean:",
            "candidates": cands,
        })
        .to_string(),
        ChefError::Other(e) => serde_json::json!({ "error": e.to_string() }).to_string(),
        ChefError::Reported(_) => String::new(), // already reported
    }
}

/// Print an error in the requested mode and return the process exit code:
/// `2` ambiguous, `1` any other failure, `1` reported (nothing printed -
/// the inner loop already reported it). Errors are always recorded in the
/// run history log.
pub fn write_error(e: &ChefError, json: bool) -> i32 {
    match e {
        ChefError::Ambiguous(_) => {
            if json {
                log::error!("{e}");
                emit_json_error(
                    &serde_json::from_str::<serde_json::Value>(&json_error(e)).unwrap(),
                );
            } else {
                log::error!("error: {e}");
            }
            2
        }
        ChefError::Reported(_) => 1,
        ChefError::Other(_) => {
            if json {
                log::error!("{e}");
                emit_json_error(
                    &serde_json::from_str::<serde_json::Value>(&json_error(e)).unwrap(),
                );
            } else {
                log::error!("error: {e}");
            }
            1
        }
    }
}
