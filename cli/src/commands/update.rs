use std::path::PathBuf;

use crate::ChefError;
use crate::game_dir;
use crate::handlers;

/// Update installed packages to the newest stable release: resolve the
/// game directory and delegate to the update handler.
pub fn run(
    pkg: Option<&str>,
    dir: Option<PathBuf>,
    dry_run: bool,
    json: bool,
) -> crate::Result<()> {
    let game_dir = game_dir::resolve_game_dir(dir.as_deref()).map_err(ChefError::Other)?;
    handlers::update::run(pkg, &game_dir, dry_run, json)
}
