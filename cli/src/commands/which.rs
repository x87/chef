use std::path::PathBuf;

use crate::ChefError;
use crate::commands::split_pkg_spec;
use crate::game_dir;
use crate::handlers;

/// Report what is installed in a game directory. The optional package
/// argument is split into `(name, spec)` here; classification happens in
/// the which handler.
pub fn run(pkg: Option<&str>, dir: Option<PathBuf>, json: bool) -> crate::Result<()> {
    let game_dir = game_dir::resolve_game_dir(dir.as_deref()).map_err(ChefError::Other)?;
    let pkg = pkg.map(|arg| split_pkg_spec(arg));
    handlers::which::run(pkg, &game_dir, json)
}
