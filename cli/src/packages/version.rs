//! Version-spec resolution (`pkg@spec`) and the resolved-release model.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, bail};

use super::assets::select_asset_url;
use super::catalog::{
    LockFile, LockedAsset, PackagesFile, VersionEntry, existent_slot, version_covers_game,
};

// ---------------------------------------------------------------------------
// Version spec resolution (`pkg@spec`)
// ---------------------------------------------------------------------------

/// A user-supplied version spec after `pkg@`.
#[derive(Debug, Clone, PartialEq)]
enum VerSpec {
    /// Newest tracked release, pre-releases included.
    Latest,
    /// Newest release that is not a pre-release.
    Stable,
    /// Newest pre-release only.
    Preview,
    Exact(semver::Version),
    Prefix(String),
}

fn parse_ver_spec(spec: &str) -> anyhow::Result<VerSpec> {
    match spec.trim().to_lowercase().as_str() {
        "" | "stable" => return Ok(VerSpec::Stable),
        "latest" => return Ok(VerSpec::Latest),
        "preview" | "beta" => return Ok(VerSpec::Preview),
        _ => {}
    }
    let norm = spec.trim().strip_prefix(['v', 'V']).unwrap_or(spec.trim());
    if let Ok(v) = semver::Version::parse(norm) {
        return Ok(VerSpec::Exact(v));
    }
    if !norm.is_empty() && norm.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return Ok(VerSpec::Prefix(norm.trim_end_matches('.').to_string()));
    }
    bail!(
        "invalid version spec '{spec}' (expected latest, stable, preview, or a version like 5, 5.4, 4.4.4)"
    )
}

fn prefix_matches(v: &semver::Version, pref: &str) -> bool {
    let s = v.to_string();
    s == *pref || s.starts_with(&format!("{pref}."))
}

fn is_preview_str(s: &str) -> bool {
    let lower = s.to_lowercase();
    lower.contains("beta")
        || lower.contains("preview")
        || lower.contains("alpha")
        || lower.contains("rc")
}

pub(crate) fn parse_version_loose(s: &str) -> Option<semver::Version> {
    if let Ok(v) = semver::Version::parse(s) {
        return Some(v);
    }
    // Normalize ".beta" / ".preview" (and underscore variants) to "-beta"
    // for semver.
    let mut normalized = s.to_string();

    for (from, to) in [
        (".beta", "-beta"),
        (".preview", "-preview"),
        (".alpha", "-alpha"),
        (".rc", "-rc"),
        ("_beta", "-beta"),
        ("_preview", "-preview"),
        ("_alpha", "-alpha"),
        ("_rc", "-rc"),
    ] {
        normalized = normalized.replace(from, to);
    }

    if normalized != s
        && let Ok(v) = semver::Version::parse(&normalized)
    {
        return Some(v);
    }
    // Fallback: if raw contains beta/preview treat as preview with synthetic pre
    if is_preview_str(s) {
        // try to extract leading numeric version (e.g. "1.5.0.beta" -> "1.5.0")
        let base = s
            .split(['.', '-', '_'])
            .take(3)
            .collect::<Vec<_>>()
            .join(".");
        // not robust, but for ordering we can at least return base if parseable
        if let Ok(mut v) = semver::Version::parse(&base) {
            v.pre = semver::Prerelease::new("beta").unwrap_or_default();
            return Some(v);
        }
    }
    None
}

fn is_preview_rec(rec: &VersionEntry, v: &semver::Version) -> bool {
    rec.preview || !v.pre.is_empty() || is_preview_str(&rec.version)
}

/// Version text for progress messages ("installing CLEO 5.4.0..."): the
/// generic `0.0.0` label (given to continuously-updated releases like
/// WidescreenFix) renders blank, so "installing WidescreenFix..." stays
/// tidy. Table cells use [`list_version`] instead.
pub fn display_version(v: &str) -> &str {
    if v == "0.0.0" { "" } else { v }
}

/// Message version word: `display_version` plus its leading separator, so
/// "installing CLEO 5.4.0..." and "installing WidescreenFix..." stay tidy.
pub fn version_word(v: &str) -> String {
    match display_version(v) {
        "" => String::new(),
        ver => format!(" {ver}"),
    }
}

/// Version text in tables and columns (menu's AVAILABLE/INSTALLED cells,
/// `which` lines, "not tracked" errors): the generic `0.0.0` label reads
/// as `<no version>` so a continuously-updated package does not look
/// empty; real versions show as released.
pub fn list_version(v: &str) -> &str {
    if v == "0.0.0" { "<no version>" } else { v }
}

/// A fully resolved deployable version: one package, one concrete semver,
/// one asset URL with its verified digest, and the payload file list taken
/// straight from the lock.
#[derive(Debug, Clone)]
pub struct ResolvedVersion {
    /// Package id - state key and store path segment.
    pub id: String,
    /// User-facing product name.
    pub name: String,
    pub version: String,
    /// Human-facing release page, when declared.
    pub release: Option<String>,
    /// Chosen asset URL.
    pub url: String,
    /// Archive digest from the lock.
    pub asset_sha256: String,
    /// Payload to deploy: (deployed path, archive entry path). Postinstall
    /// renames have been applied to the deployed side.
    pub payload: Vec<(String, String)>,
    /// Package ids this version requires to be present.
    pub dependencies: Vec<String>,
    /// Ids occupying the same slot (this one + `replaces` partners).
    pub slot: Vec<String>,
}

impl ResolvedVersion {
    /// Store/cache path segment (the package id - versions never collide
    /// within one id after the game-aware selection).
    pub fn store_key(&self) -> &str {
        &self.id
    }
}

/// Assemble the deploy payload for one version record: lock inner files
/// (excludes already applied) mapped through the postinstall renames.
fn resolve_payload(
    locked: &LockedAsset,
    rename: Option<&BTreeMap<String, String>>,
) -> Vec<(String, String)> {
    locked
        .files
        .iter()
        .map(|f| {
            let deployed = rename
                .and_then(|r| r.get(&f.path))
                .cloned()
                .unwrap_or_else(|| f.path.clone());
            (deployed, f.path.clone())
        })
        .collect()
}

/// Resolve a version spec for one package id, restricted to the detected
/// game. `game` filters version records by their `games` list (None keeps
/// every record, deduping identical semvers). Preview = semver pre-release.
pub fn resolve_spec(
    pkgs: &PackagesFile,
    lock: &LockFile,
    id: &str,
    game: Option<&str>,
    spec: Option<&str>,
) -> anyhow::Result<ResolvedVersion> {
    let pkg = pkgs
        .pkg(id)
        .with_context(|| format!("unknown package '{id}'"))?;
    let vs = parse_ver_spec(spec.unwrap_or("stable"))?;

    #[derive(Clone)]
    struct Entry<'a> {
        version: semver::Version,
        rec: &'a VersionEntry,
        url: String,
        locked: &'a LockedAsset,
    }
    let mut entries: Vec<Entry> = Vec::new();
    for rec in &pkg.versions {
        if let Some(g) = game
            && !version_covers_game(rec, g)
        {
            continue;
        }
        let Some(v) = parse_version_loose(&rec.version) else {
            continue;
        };
        // Identical semvers appear when one version carries separate
        // game/rename records; the game filter keeps the right one, and
        // without a game the first record wins.
        if entries.iter().any(|e| e.version == v) {
            continue;
        }
        let url = select_asset_url(&rec.assets)?;
        let locked = lock.assets.get(&url).with_context(|| {
            format!("no digest recorded for {url} - regenerate packages.lock (tools/gen_hashes)")
        })?;
        entries.push(Entry {
            version: v,
            rec,
            url,
            locked,
        });
    }
    if entries.is_empty() {
        bail!("no tracked releases for {id} - run 'chef menu --refresh' or check packages.json");
    }
    entries.sort_by(|a, b| b.version.cmp(&a.version));

    let pick = |it: Vec<Entry<'_>>| {
        it.into_iter().next().map(|e| ResolvedVersion {
            id: id.to_string(),
            name: pkg.name.clone(),
            version: e.version.to_string(),
            release: e.rec.release.clone(),
            url: e.url,
            asset_sha256: e.locked.sha256.clone(),
            payload: resolve_payload(e.locked, e.rec.postinstall.as_ref().map(|p| &p.rename)),
            dependencies: e.rec.dependencies.clone(),
            slot: existent_slot(pkgs, id),
        })
    };

    match vs {
        VerSpec::Latest => {
            pick(entries).ok_or_else(|| anyhow::anyhow!("no releases found for {id}"))
        }
        VerSpec::Stable => pick(
            entries
                .into_iter()
                .filter(|e| !is_preview_rec(e.rec, &e.version))
                .collect(),
        )
        .ok_or_else(|| anyhow::anyhow!("no stable (non-prerelease) release found for {id}")),
        VerSpec::Preview => pick(
            entries
                .into_iter()
                .filter(|e| is_preview_rec(e.rec, &e.version))
                .collect(),
        )
        .ok_or_else(|| anyhow::anyhow!("no preview releases available for {id}")),
        VerSpec::Exact(want) => entries
            .into_iter()
            .find(|e| e.version == want)
            .map(|e| ResolvedVersion {
                id: id.to_string(),
                name: pkg.name.clone(),
                version: e.version.to_string(),
                release: e.rec.release.clone(),
                url: e.url,
                asset_sha256: e.locked.sha256.clone(),
                payload: resolve_payload(e.locked, e.rec.postinstall.as_ref().map(|p| &p.rename)),
                dependencies: e.rec.dependencies.clone(),
                slot: existent_slot(pkgs, id),
            })
            .ok_or_else(|| {
                let mut avail: Vec<String> = pkg
                    .versions
                    .iter()
                    .map(|v| list_version(&v.version).to_string())
                    .collect();
                avail.sort();
                avail.dedup();
                anyhow::anyhow!(
                    "version {want} not tracked (available: {})",
                    avail.join(", ")
                )
            }),
        VerSpec::Prefix(pref) => {
            let majors = {
                let mut ms: Vec<u64> = entries.iter().map(|e| e.version.major).collect();
                ms.sort_unstable();
                ms.dedup();
                ms
            };
            let stable: Vec<Entry> = entries
                .iter()
                .filter(|e| !is_preview_rec(e.rec, &e.version) && prefix_matches(&e.version, &pref))
                .cloned()
                .collect();
            pick(stable).ok_or_else(|| {
                let ml = majors
                    .iter()
                    .map(|m| m.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                anyhow::anyhow!(
                    "version '{pref}' does not match any tracked release (available majors: {ml})"
                )
            })
        }
    }
}

/// Versions a package offers, restricted to the detected game: newest
/// stable per major plus the single newest preview when it outranks
/// everything listed. Returns (semver, is_preview) newest first.
pub fn available_versions(
    pkgs: &PackagesFile,
    lock: &LockFile,
    id: &str,
    game: Option<&str>,
) -> Vec<(String, bool)> {
    let Some(pkg) = pkgs.pkg(id) else {
        return Vec::new();
    };
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut best_stable: BTreeMap<u64, semver::Version> = BTreeMap::new();
    let mut newest_preview: Option<semver::Version> = None;
    for rec in &pkg.versions {
        if let Some(g) = game
            && !version_covers_game(rec, g)
        {
            continue;
        }
        // Only versions with a locked asset are offered (a never-hashed
        // entry cannot be installed).
        let asset_ok = rec.assets.iter().any(|a| lock.assets.contains_key(a));
        if !asset_ok {
            continue;
        }
        let Some(v) = parse_version_loose(&rec.version) else {
            continue;
        };
        if !seen.insert(v.to_string()) {
            continue;
        }
        if !is_preview_rec(rec, &v) {
            best_stable
                .entry(v.major)
                .and_modify(|b| {
                    if v > *b {
                        *b = v.clone()
                    }
                })
                .or_insert(v.clone());
        } else {
            newest_preview = Some(match newest_preview {
                Some(cur) if cur >= v => cur,
                _ => v.clone(),
            });
        }
    }
    let mut out: Vec<(semver::Version, bool)> = Vec::new();
    for (_, v) in best_stable {
        out.push((v, false));
    }
    if let Some(pv) = newest_preview
        && out.iter().all(|(v, _)| pv > *v)
    {
        out.push((pv, true));
    }
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out.dedup();
    out.into_iter()
        .map(|(v, pre)| (v.to_string(), pre))
        .collect()
}
