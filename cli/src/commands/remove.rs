use std::path::PathBuf;

use crate::ChefError;
use crate::commands::split_pkg_spec;
use crate::game_dir;
use crate::handlers;
use crate::packages;

/// Remove packages. Every argument is validated here, before anything is
/// touched: a version spec must be an exact semver or a numeric prefix
/// (e.g. `5`, `5.4`), matched against the installed version by the
/// handler.
pub fn run(pkgs: &[String], dir: Option<PathBuf>, json: bool) -> crate::Result<()> {
    let game_dir = game_dir::resolve_game_dir(dir.as_deref()).map_err(ChefError::Other)?;

    let mut specs: Vec<(&str, Option<&str>)> = Vec::new();
    for pkg in pkgs {
        let (name, spec) = split_pkg_spec(pkg);
        let expect: Option<&str> = match spec {
            None => None,
            Some(s) => {
                let norm = packages::strip_v_prefix(s);
                let is_exact = packages::is_exact_version(norm);
                let is_prefix = !norm.is_empty()
                    && !norm.ends_with('.')
                    && norm.chars().all(|c| c.is_ascii_digit() || c == '.');
                if !(is_exact || is_prefix) {
                    return Err(ChefError::Other(anyhow::anyhow!(
                        "remove requires an exact version (e.g. {name}@4.4.4)"
                    )));
                }
                Some(norm)
            }
        };
        specs.push((name, expect));
    }

    handlers::remove::run(&specs, &game_dir, json)
}
