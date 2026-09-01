use std::sync::{Mutex, OnceLock};

/// Captured output for tests running with `--json` in-process.
#[derive(Default)]
pub struct JsonOutput {
    /// pretty JSON documents printed to stdout.
    pub out: Vec<String>,
    /// JSON error objects printed to stderr.
    pub err: Vec<String>,
}

static CAPTURE: OnceLock<Mutex<Option<JsonOutput>>> = OnceLock::new();

fn capture() -> &'static Mutex<Option<JsonOutput>> {
    CAPTURE.get_or_init(|| Mutex::new(None))
}

/// Test seam: while a capture is installed, `emit_json` and
/// `emit_json_error` append instead of printing.
pub fn set_capture(c: JsonOutput) {
    *capture().lock().unwrap() = Some(c);
}

/// Test seam: take the captured output; `None` while no capture is set.
pub fn take_capture() -> Option<JsonOutput> {
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

/// Print an error in the requested mode and return the process exit code
/// Errors are always recorded in the history log.
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

#[cfg(test)]
pub fn dbg_trace(msg: impl std::fmt::Display) {
    eprintln!("[chef-dbg] {msg}");
}
