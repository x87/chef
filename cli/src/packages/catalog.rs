//! The package catalog data model and slot helpers.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct PackagesFile {
    pub schema: u32,
    pub packages: Vec<PackageEntry>,
    /// Lowercase game-executable name -> game id (e.g. "gta_sa.exe" ->
    /// "gta-sa"). Drives game detection.
    pub games: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PackageEntry {
    /// Canonical id - the addressable name, the state key and the store
    /// path segment (e.g. "cleo.sa", "silents-asi-loader.sa").
    pub id: String,
    /// User-facing product name (e.g. "CLEO", "CLEO Redux").
    pub name: String,
    /// Extra names users can type to select this package (shortcuts like
    /// "sal", "cleo5", "iii.cleo").
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Ids this package replaces in a game directory (the as-i-loader pair
    /// lists each other). Installing either evicts the other.
    #[serde(default)]
    pub replaces: Vec<String>,
    pub versions: Vec<VersionEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VersionEntry {
    /// Semver string; kept exactly as released (e.g. "1.5.0").
    /// Whether this version is a preview/prerelease is stored separately
    /// in `preview` (GitHub `prerelease:true`) so the version itself is
    /// never mutated to inject `-beta.1`.
    pub version: String,
    /// When true, this version is a preview / prerelease even if
    /// `version` is a plain semver like `1.5.0`.
    #[serde(default, alias = "prerelease", alias = "pre_release")]
    pub preview: bool,
    /// Human-facing release page (informational; never downloaded).
    #[serde(default)]
    pub release: Option<String>,
    /// Download URLs for this version's archives. When several are listed
    /// (arch variants), chef picks one suitable for the current platform.
    pub assets: Vec<String>,
    /// Archive-entry globs excluded from the payload. Consumed by
    /// `gen_hashes`; `packages.lock` already reflects them.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Game ids this version applies to (e.g. "gta-sa"). Empty means
    /// game-agnostic.
    #[serde(default)]
    pub games: Vec<String>,
    /// Package ids required to be present alongside this version.
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub postinstall: Option<PostInstall>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PostInstall {
    /// Archive entry name -> deployed file name (e.g. UAL ships
    /// dinput8.dll that SA must see as vorbisFile.dll).
    #[serde(default)]
    pub rename: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LockFile {
    pub schema: u32,
    /// When the lock was generated (informational).
    pub generated_at: u64,
    /// Keyed by asset URL.
    pub assets: BTreeMap<String, LockedAsset>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LockedAsset {
    pub url: String,
    pub sha256: String,
    /// Digest of every kept payload entry inside the archive, after
    /// excludes. This list is the deployment manifest for the version.
    #[serde(default)]
    pub files: Vec<LockedFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LockedFile {
    /// Archive-relative path (`/`-separated).
    pub path: String,
    pub sha256: String,
}

impl PackagesFile {
    pub fn pkg(&self, id: &str) -> Option<&PackageEntry> {
        self.packages.iter().find(|p| p.id == id)
    }

    /// Whole catalog as a sorted id list (canonical ordering for reports).
    pub fn sorted_ids(&self) -> Vec<&str> {
        let mut ids: Vec<&str> = self.packages.iter().map(|p| p.id.as_str()).collect();
        ids.sort_unstable();
        ids
    }

    /// Does the package have any version record that applies to `game`?
    pub fn covers_game(&self, pkg_id: &str, game: &str) -> bool {
        self.pkg(pkg_id)
            .is_some_and(|p| p.versions.iter().any(|v| version_covers_game(v, game)))
    }
}

pub fn version_covers_game(v: &VersionEntry, game: &str) -> bool {
    v.games.is_empty() || v.games.iter().any(|g| g == game)
}

/// The ids occupying the same slot as `id` - itself plus every id linked
/// through mutual `replaces` pairs (transitively). Only one of them can be
/// installed in a game directory at a time.
pub fn slot_ids(pkgs: &PackagesFile, id: &str) -> Vec<String> {
    // FIFO crawl over mutual `replaces` pairs: `id`, the ids it replaces,
    // ids that replace it, and so on transitively.
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let mut queue = vec![id.to_string()];
    let mut at = 0;
    while at < queue.len() {
        let cur = queue[at].clone();
        at += 1;
        if !seen.insert(cur.clone()) {
            continue;
        }
        out.push(cur.clone());
        if let Some(p) = pkgs.pkg(&cur) {
            for r in &p.replaces {
                if !seen.contains(r) {
                    queue.push(r.clone());
                }
            }
        }
        for p in &pkgs.packages {
            if p.id != cur && p.replaces.iter().any(|r| r == &cur) && !seen.contains(&p.id) {
                queue.push(p.id.clone());
            }
        }
    }
    out
}

/// The shared slot engine used by dependency checks and replacement:
/// `slot_ids` restricted to ids that actually exist in the catalog.
/// Always includes `id` itself even when it is unknown.
pub fn existent_slot(pkgs: &PackagesFile, id: &str) -> Vec<String> {
    slot_ids(pkgs, id)
        .into_iter()
        .filter(|s| s == id || pkgs.pkg(s).is_some())
        .collect()
}
