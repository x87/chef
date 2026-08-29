use std::path::PathBuf;

use log::{error, info};

use crate::ChefError;
use crate::commands::add::split_pkg_spec;
use crate::game_dir;
use crate::match_names;
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

pub fn run(pkgs: &[String], dir: Option<PathBuf>, json: bool) -> crate::Result<()> {
    let mut first: Option<ChefError> = None;
    let mut removed: Vec<Removed> = Vec::new();

    for pkg in pkgs {
        match remove_one(pkg, dir.clone()) {
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
                    crate::emit_json_error(&serde_json::json!({ "error": format!("{e:#}") }));
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
            crate::emit_json(&serde_json::json!({ "remove": removed }));
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

fn remove_one(pkg: &str, dir: Option<PathBuf>) -> crate::Result<Removed> {
    let game_dir = game_dir::resolve_game_dir(dir.as_deref()).map_err(ChefError::Other)?;
    let (pkgs, _lf) = packages::load_metadata(false).map_err(ChefError::Other)?;
    let (name, spec) = split_pkg_spec(pkg);

    // remove takes a version when given - exact or prefix (e.g. 5, 5.4) is accepted
    // and matched against the installed version in `remove_install`.
    let expect_version: Option<String> = match spec {
        None => None,
        Some(s) => {
            let norm = s.trim().strip_prefix(['v', 'V']).unwrap_or(s.trim());
            let is_exact = semver::Version::parse(norm).is_ok();
            let is_prefix = !norm.is_empty()
                && !norm.ends_with('.')
                && norm.chars().all(|c| c.is_ascii_digit() || c == '.');
            if is_exact || is_prefix {
                Some(norm.to_string())
            } else {
                return Err(ChefError::Other(anyhow::anyhow!(
                    "remove requires an exact version (e.g. {name}@4.4.4)"
                )));
            }
        }
    };

    // Resolve the package id.
    let game = game_dir::detect_game(&pkgs, &game_dir).map_err(ChefError::Other)?;
    let id = match_names::resolve_id(&pkgs, name, game.as_ref().map(|g| g.family.as_str()))?;

    let _lock = game_dir::Lock::acquire(&game_dir).map_err(ChefError::Other)?;
    let (inst, preserved, restored) =
        game_dir::remove_install(&game_dir, &id, expect_version.as_deref())
            .map_err(ChefError::Other)?;
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
