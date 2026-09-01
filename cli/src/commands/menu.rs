use std::path::PathBuf;

use crate::game_dir;
use crate::handlers;

/// Show available packages and versions. The game directory is resolved
/// best-effort (the menu tolerates running outside any game folder) and
/// passed through to the menu handler.
pub fn run(
    pkg: Option<&str>,
    dir: Option<PathBuf>,
    json: bool,
    refresh: bool,
) -> crate::Result<()> {
    let dir = game_dir::resolve_game_dir(dir.as_deref()).ok();
    handlers::menu::run(pkg, dir, json, refresh)
}
