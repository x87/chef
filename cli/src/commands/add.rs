use std::path::PathBuf;

use crate::ChefError;
use crate::commands::split_pkg_spec;
use crate::game_dir;
use crate::handlers;

/// Install packages: split each `name@spec` argument, resolve the game
/// directory, and hand the parsed request to the add handler.
pub fn run(
    inputs: &[String],
    dir: Option<PathBuf>,
    dry_run: bool,
    json: bool,
) -> crate::Result<()> {
    let game_dir = game_dir::resolve_game_dir(dir.as_deref()).map_err(ChefError::Other)?;
    let specs: Vec<(&str, Option<&str>)> = inputs.iter().map(|s| split_pkg_spec(s)).collect();
    handlers::add::run(&specs, &game_dir, dry_run, json)
}
