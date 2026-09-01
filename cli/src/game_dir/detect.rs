//! Game detection from executables in the target directory.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::bail;

use crate::packages::PackagesFile;

// ---------------------------------------------------------------------------
// Game detection
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DetectedGame {
    pub exe: String,
    /// Game family ("sa" / "iii" / "vc") from the catalog exe map.
    pub family: String,
}

/// Human-readable name for a game family (the catalog's game ids).
pub fn family_title(family: &str) -> &'static str {
    match family {
        "gta-sa" => "GTA San Andreas",
        "gta-3" => "GTA III",
        "gta-vc" => "GTA Vice City",
        _ => "unknown game",
    }
}

/// Detect the game from executable names in the target directory (root only)
/// using the catalog's exe -> game-id map. Multiple recognized exes mapping
/// to different games -> ambiguous error. Exe names match case-insensitively
/// on all platforms.
pub fn detect_game(pkgs: &PackagesFile, dir: &Path) -> anyhow::Result<Option<DetectedGame>> {
    let mut found: BTreeMap<String, String> = BTreeMap::new(); // game -> exe
    for e in std::fs::read_dir(dir)?.flatten() {
        let name = e.file_name().to_string_lossy().to_lowercase();
        if let Some(game) = pkgs.games.get(&name) {
            found.entry(game.clone()).or_insert(name);
        }
    }
    if found.is_empty() {
        return Ok(None);
    }
    if found.len() > 1 {
        let list = found
            .iter()
            .map(|(f, e)| format!("{e} -> {}", family_title(f)))
            .collect::<Vec<_>>()
            .join(", ");
        bail!(
            "ambiguous game detection in {}: multiple game executables found ({list})",
            dir.display()
        );
    }
    let (family, exe) = found.into_iter().next().unwrap();
    Ok(Some(DetectedGame { exe, family }))
}
