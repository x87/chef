use std::path::Path;

use log::{error, info};

use crate::ChefError;
use crate::game_dir;
use crate::packages;

/// One successfully removed package, with what chef restored.
#[derive(serde::Serialize)]
struct Removed {
    id: String,
    name: String,
    version: String,
    /// Files the user changed after install - handed back, not deleted.
    preserved: Vec<String>,
    /// Originals chef had displaced, now back from the backup.
    restored: Vec<String>,
}

/// Remove every `name@<version>` request in `specs` from `game_dir`.
/// Each failure is reported immediately; the loop keeps going and the
/// first error is returned after the whole batch (exit code 1).
pub fn run(specs: &[(&str, Option<&str>)], game_dir: &Path, json: bool) -> crate::Result<()> {
    let mut first: Option<ChefError> = None;
    let mut removed: Vec<Removed> = Vec::new();

    for spec in specs {
        match remove_one(spec, game_dir) {
            Ok(info) => {
                if json {
                    removed.push(info);
                } else {
                    info!(
                        "removed {}{}",
                        info.name,
                        packages::version_word(&info.version)
                    );
                    for p in &info.preserved {
                        info!("restored user-modified file: {p}");
                    }
                    for rel in &info.restored {
                        info!("restored your original file: {rel}");
                    }
                }
            }
            Err(e) => {
                if json {
                    log::error!("{e:#}");
                    crate::emit::emit_json_error(&serde_json::json!({ "error": format!("{e:#}") }));
                } else {
                    error!("error: {e:#}");
                }
                if first.is_none() {
                    first = Some(e);
                }
            }
        }
    }

    if json {
        if !removed.is_empty() {
            crate::emit::emit_json(&serde_json::json!({ "remove": removed }));
        }
        return match first {
            // Each failure already emitted its own JSON error object.
            Some(e) => Err(ChefError::Reported(Box::new(match e {
                ChefError::Other(inner) => inner,
                other => anyhow::anyhow!("{other}"),
            }))),
            None => Ok(()),
        };
    }

    match first {
        // Each failure is already on stderr; main only sets exit code 1.
        Some(e) => Err(ChefError::Reported(Box::new(match e {
            ChefError::Other(inner) => inner,
            other => anyhow::anyhow!("{other}"),
        }))),
        None => Ok(()),
    }
}

fn remove_one(spec: &(&str, Option<&str>), game_dir: &Path) -> crate::Result<Removed> {
    let (name, expect_version) = *spec;

    // Resolve the package id against the detected game (the command layer
    // already validated the version spec syntax).
    let (pkgs, _lf) = packages::load_metadata(false).map_err(ChefError::Other)?;
    let game = game_dir::detect_game(&pkgs, game_dir).map_err(ChefError::Other)?;
    let id = packages::resolve_id(&pkgs, name, game.as_ref().map(|g| g.family.as_str()))?;

    let _lock = game_dir::Lock::acquire(game_dir).map_err(ChefError::Other)?;
    let (inst, preserved, restored) =
        game_dir::remove_install(game_dir, &id, expect_version).map_err(ChefError::Other)?;
    drop(_lock);

    let title = pkgs
        .pkg(&inst.package)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| inst.package.clone());

    Ok(Removed {
        id: inst.package.clone(),
        name: title,
        version: inst.version.clone(),
        preserved,
        restored,
    })
}
