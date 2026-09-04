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
    let norm = strip_v_prefix(spec);
    if let Ok(v) = semver::Version::parse(norm) {
        return Ok(VerSpec::Exact(v));
    }
    // 4-part numeric releases (e.g. "2.0.0.6") map onto build metadata
    // ("2.0.0+6") so quad versions resolve as exact semvers.
    if let Some(quad) = normalize_quad(norm)
        && let Ok(v) = semver::Version::parse(&quad)
    {
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
    // 4-part numeric releases (e.g. "2.0.0.6") map onto build metadata
    // ("2.0.0+6") so quad versions are tracked, ordered as their base
    // release and considered stable (build metadata is not a prerelease).
    if let Some(quad) = normalize_quad(s)
        && let Ok(v) = semver::Version::parse(&quad)
    {
        return Some(v);
    }
    None
}

/// Map a 4-part numeric version ("2.0.0.6" = major.minor.patch.build) onto
/// semver build metadata ("2.0.0+6"). Returns None for anything that is not
/// exactly four dot-separated numeric components.
fn normalize_quad(s: &str) -> Option<String> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() == 4
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
    {
        Some(format!(
            "{}.{}.{}+{}",
            parts[0], parts[1], parts[2], parts[3]
        ))
    } else {
        None
    }
}

/// The version text users see: the exact upstream spelling whenever the
/// catalog entry is parseable as-is or is a 4-part numeric version, so
/// "2.0.0.6" shows as "2.0.0.6" rather than the intermediate semver build
/// form "2.0.0+6". Loose spellings that only parse after normalization
/// (e.g. "2.0.0.beta" -> 2.0.0-beta) still show the parsed form.
pub(crate) fn display_spelling(raw: &str, v: &semver::Version) -> String {
    if semver::Version::parse(raw).is_ok() || normalize_quad(raw).is_some() {
        raw.to_string()
    } else {
        v.to_string()
    }
}

// ---------------------------------------------------------------------------
// Shared version primitives. Consumers access these through `packages::*`;
// the semver crate stays confined to this module.
// ---------------------------------------------------------------------------

/// Strip a leading `v`/`V` and surrounding whitespace from a version text
/// ("v2.1" -> "2.1").
pub(crate) fn strip_v_prefix(s: &str) -> &str {
    s.trim().strip_prefix(['v', 'V']).unwrap_or(s.trim())
}

/// Is `s` a complete exact version ("4.4.4")? Strict semver: a quad like
/// "2.0.0.6" is not exact here - it flows through the loose machinery.
pub(crate) fn is_exact_version(s: &str) -> bool {
    semver::Version::parse(s).is_ok()
}

/// Strict parse with a proper error, for release tags that must parse
/// (chef's own version tags).
pub(crate) fn parse_strict(s: &str) -> anyhow::Result<semver::Version> {
    semver::Version::parse(s).map_err(|e| anyhow::anyhow!("invalid version '{s}': {e}"))
}

/// Chef's own current version, from the crate manifest.
pub(crate) fn current_version() -> semver::Version {
    semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap()
}

/// Parse a GitHub release JSON object's `tag_name` ("v1.2.3") into a
/// version.
pub(crate) fn parse_tag(v: &serde_json::Value) -> Option<semver::Version> {
    let tag = v.get("tag_name")?.as_str()?;
    semver::Version::parse(strip_v_prefix(tag)).ok()
}

/// Order two version texts newest-first: parsed versions by their numeric
/// components (unparseable sorts as 0.0.0), tie-breaking by string.
pub(crate) fn version_cmp_desc(a: &str, b: &str) -> std::cmp::Ordering {
    let ka = parse_version_loose(a)
        .map(|v| (v.major, v.minor, v.patch))
        .unwrap_or((0, 0, 0));
    let kb = parse_version_loose(b)
        .map(|v| (v.major, v.minor, v.patch))
        .unwrap_or((0, 0, 0));
    kb.cmp(&ka).then_with(|| b.cmp(a))
}

/// The same release under two spellings ("2.0.0.6" vs "2.0.0+6", or
/// exact text): equal as text or equal after parsing.
pub(crate) fn same_version(a: &str, b: &str) -> bool {
    a == b
        || parse_version_loose(a)
            .zip(parse_version_loose(b))
            .is_some_and(|(x, y)| x == y)
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
        /// Upstream spelling for user-facing text ("2.0.0.6"), not the
        /// parsed semver text ("2.0.0+6").
        display: String,
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
            display: display_spelling(&rec.version, &v),
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
            version: e.display.clone(),
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
                version: e.display.clone(),
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
    let mut best_stable: BTreeMap<u64, (semver::Version, String)> = BTreeMap::new();
    let mut newest_preview: Option<(semver::Version, String)> = None;
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
        let display = display_spelling(&rec.version, &v);
        if !is_preview_rec(rec, &v) {
            best_stable
                .entry(v.major)
                .and_modify(|b| {
                    if v > b.0 {
                        *b = (v.clone(), display.clone())
                    }
                })
                .or_insert((v.clone(), display));
        } else {
            newest_preview = Some(match newest_preview {
                Some((cur, cur_display)) if cur >= v => (cur, cur_display),
                _ => (v.clone(), display),
            });
        }
    }
    let mut out: Vec<(semver::Version, String, bool)> = Vec::new();
    for (_, (v, display)) in best_stable {
        out.push((v, display, false));
    }
    if let Some((pv, pdisplay)) = newest_preview
        && out.iter().all(|(v, _, _)| pv > *v)
    {
        out.push((pv, pdisplay, true));
    }
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out.dedup();
    out.into_iter()
        .map(|(_, display, pre)| (display, pre))
        .collect()
}
