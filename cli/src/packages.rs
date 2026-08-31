use anyhow::{Context, bail};
use lazy_static::lazy_static;
use log::warn;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use crate::utils::fs::write_atomic;
use crate::utils::http::fetch_url;

/// The binary contacts only its own repo.
const DEFAULT_PACKAGES_URL: &str =
    "https://raw.githubusercontent.com/x87/chef/master/packages.json";
const DEFAULT_LOCK_URL: &str = "https://raw.githubusercontent.com/x87/chef/master/packages.lock";
const TTL: Duration = Duration::from_secs(24 * 60 * 60);
const PACKAGES_MAX_BYTES: u64 = 20 * 1024 * 1024;
/// The lock carries per-file digests; it can legitimately be larger.
const LOCK_MAX_BYTES: u64 = 64 * 1024 * 1024;
pub const SUPPORTED_PACKAGES_SCHEMA: u32 = 2;
pub const SUPPORTED_LOCK_SCHEMA: u32 = 2;

// ---------------------------------------------------------------------------
// Data home / well-known paths
// ---------------------------------------------------------------------------

lazy_static! {
    static ref HOME_OVERRIDE: Mutex<Option<PathBuf>> = Mutex::new(None);
}

/// Test seam: point chef's data home at a sandbox without any environment
/// variable or CLI surface. Held by the integration tests for the lifetime
/// of each `TestEnv`; cleared by `clear_home_override()`.
pub fn set_home_override(p: PathBuf) {
    *HOME_OVERRIDE.lock().unwrap() = Some(p);
}

/// Clear the home override set by `set_home_override`.
pub fn clear_home_override() {
    *HOME_OVERRIDE.lock().unwrap() = None;
}

fn home_override() -> Option<PathBuf> {
    HOME_OVERRIDE.lock().unwrap().clone()
}

/// Data home: the platform app-data directory plus "Chef"
/// (`%LOCALAPPDATA%\Chef` on Windows), overridable only by the test seam
/// `set_home_override`.
pub fn chef_home() -> PathBuf {
    if let Some(p) = home_override() {
        return p;
    }
    dirs::data_local_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local/share")
        })
        .join(APP_DIR_NAME)
}

/// Data-home folder name: lowercase `chef` on Unix (matches the installer's
/// `~/.local/share/chef` default), `Chef` on Windows (`%LOCALAPPDATA%\Chef`).
#[cfg(unix)]
const APP_DIR_NAME: &str = "chef";
#[cfg(not(unix))]
const APP_DIR_NAME: &str = "Chef";

fn packages_mirror() -> PathBuf {
    chef_home().join("packages.json")
}

fn lock_mirror() -> PathBuf {
    chef_home().join("packages.lock")
}

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

// ---------------------------------------------------------------------------
// Intake: local override -> fresh fetch within TTL -> cached mirror
// ---------------------------------------------------------------------------

fn check_schema<F: FnOnce() -> String>(actual: u32, supported: u32, what: F) -> anyhow::Result<()> {
    if actual > supported {
        bail!(
            "{} schema {} is newer than this chef (supports {}) - please upgrade chef",
            what(),
            actual,
            supported
        );
    }
    if actual < supported {
        bail!(
            "{} schema {} is outdated (this chef needs {}) - delete the cached copy and refresh",
            what(),
            actual,
            supported
        );
    }
    Ok(())
}

fn parse_packages(bytes: &[u8]) -> anyhow::Result<PackagesFile> {
    let pf: PackagesFile =
        serde_json::from_slice(bytes).context("packages.json is not valid JSON")?;
    check_schema(pf.schema, SUPPORTED_PACKAGES_SCHEMA, || {
        "package catalog".to_string()
    })?;
    Ok(pf)
}

fn parse_lock(bytes: &[u8]) -> anyhow::Result<LockFile> {
    let lf: LockFile = serde_json::from_slice(bytes).context("packages.lock is not valid JSON")?;
    check_schema(lf.schema, SUPPORTED_LOCK_SCHEMA, || {
        "package lock".to_string()
    })?;
    Ok(lf)
}

fn mirror_is_stale(mirror: &Path) -> bool {
    match std::fs::metadata(mirror).and_then(|m| m.modified()) {
        Ok(mtime) => SystemTime::now()
            .duration_since(mtime)
            .map(|age| age >= TTL)
            .unwrap_or(true),
        Err(_) => true,
    }
}

fn load_mirror(mirror: &Path, what: &str) -> anyhow::Result<Vec<u8>> {
    std::fs::read(mirror).with_context(|| format!("cannot read cached {what} {}", mirror.display()))
}

/// One fetchable metadata file: its default URL, local mirror and how
/// large it may legitimately be.
struct Metadata {
    default_url: &'static str,
    mirror: PathBuf,
    max_bytes: u64,
    what: &'static str,
}

/// Resolve one metadata file: fresh fetch within TTL -> cached mirror
/// fallback. `parse` turns the bytes into the typed file. Tests seed the
/// mirror (`<home>/packages.json` / `<home>/packages.lock`) directly, so a
/// fresh mirror serves offline without any override hook.
fn acquire<T>(
    meta: &Metadata,
    parse: fn(&[u8]) -> anyhow::Result<T>,
    force_refresh: bool,
) -> anyhow::Result<T> {
    let url = meta.default_url;
    if !force_refresh && !mirror_is_stale(&meta.mirror) {
        let bytes = load_mirror(&meta.mirror, meta.what)?;
        return parse(&bytes);
    }

    match fetch_url(url, meta.max_bytes, Duration::from_secs(60)) {
        Ok(bytes) => {
            let parsed = parse(&bytes)?;
            write_atomic(&meta.mirror, &bytes)?;
            Ok(parsed)
        }
        Err(e) => {
            if meta.mirror.exists() {
                let bytes = load_mirror(&meta.mirror, meta.what)?;
                let parsed = parse(&bytes)?;
                warn!(
                    "{} refresh failed ({e:#}); using cached mirror from {}",
                    meta.what,
                    meta.mirror.display()
                );
                return Ok(parsed);
            }
            Err(e).context(format!(
                "cannot fetch {} from {url} and no cached mirror exists at {}",
                meta.what,
                meta.mirror.display()
            ))
        }
    }
}

/// The package catalog. `force_refresh` bypasses the TTL.
pub fn get_packages(force_refresh: bool) -> anyhow::Result<PackagesFile> {
    acquire(
        &Metadata {
            default_url: DEFAULT_PACKAGES_URL,
            mirror: packages_mirror(),
            max_bytes: PACKAGES_MAX_BYTES,
            what: "package catalog",
        },
        parse_packages,
        force_refresh,
    )
}

/// The digest lock. `force_refresh` bypasses the TTL.
pub fn get_lock(force_refresh: bool) -> anyhow::Result<LockFile> {
    acquire(
        &Metadata {
            default_url: DEFAULT_LOCK_URL,
            mirror: lock_mirror(),
            max_bytes: LOCK_MAX_BYTES,
            what: "package lock",
        },
        parse_lock,
        force_refresh,
    )
}

/// Fetch both metadata files together (most commands need both).
pub fn load_metadata(force_refresh: bool) -> anyhow::Result<(PackagesFile, LockFile)> {
    let pkgs = get_packages(force_refresh)?;
    let lock = get_lock(force_refresh)?;
    // Surface obviously-stale locks (missing assets) without failing the
    // whole command; resolution still errors with a precise message.
    for pkg in &pkgs.packages {
        for v in &pkg.versions {
            for a in &v.assets {
                if !lock.assets.contains_key(a) {
                    warn!(
                        "no digest for {} ({} {}) - run 'cargo run -p gen_hashes' to regenerate packages.lock",
                        a, pkg.id, v.version
                    );
                }
            }
        }
    }
    Ok((pkgs, lock))
}

// ---------------------------------------------------------------------------
// Version + asset resolution
// ---------------------------------------------------------------------------

/// Current platform tag ("os-arch"), used to pick among arch-variant
/// assets when a version lists several.
pub fn current_platform() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "aarch64") => "windows-aarch64",
        _ => "windows-x86_64",
    }
}

/// Pick the asset URL for this platform among a version's candidates:
/// prefer one whose name hints the current os/arch; fall back to the first.
pub fn select_asset_url(assets: &[String]) -> anyhow::Result<String> {
    if assets.is_empty() {
        bail!("version declares no assets");
    }
    if assets.len() == 1 {
        return Ok(assets[0].clone());
    }
    let platform = current_platform();
    for a in assets {
        if asset_hints_platform(a, platform) {
            return Ok(a.clone());
        }
    }
    Ok(assets[0].clone())
}

/// Loose name-marker test: only returns true for assets that carry a marker,
/// so unmarked candidates never outrank the default.
fn asset_hints_platform(url: &str, platform: &str) -> bool {
    let name = url.rsplit('/').next().unwrap_or(url).to_lowercase();
    let (os, arch) = platform.split_once('-').unwrap_or((platform, ""));
    let os_ok = if name.contains("win32") || name.contains("windows") {
        os == "windows"
    } else {
        return false; // no marker -> not a hint
    };
    if arch.is_empty() {
        return os_ok;
    }
    let arch_ok = if name.contains("arm64") || name.contains("aarch64") {
        arch == "aarch64"
    } else if name.contains("x64") || name.contains("amd64") || name.contains("x86-64") {
        arch == "x86_64"
    } else if name.contains("x86") || name.contains("i386") || name.contains("win32") {
        arch == "x86_64" || arch == "x86"
    } else {
        return false;
    };
    os_ok && arch_ok
}

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

// ---------------------------------------------------------------------------
// Payload identification (used by `which` for unmanaged files)
// ---------------------------------------------------------------------------

/// basename -> (package id, version) for every payload file in the lock.
/// Lets chef name an unknown on-disk file by its sha256 alone, and map
/// root payload filenames back to the ids that ship them.
pub fn payload_index<'a>(
    pkgs: &'a PackagesFile,
    lock: &'a LockFile,
) -> Vec<(String, String, String, String)> {
    // (sha256, id, version, path)
    let mut out: Vec<(String, String, String, String)> = Vec::new();
    for pkg in &pkgs.packages {
        for rec in &pkg.versions {
            for url in &rec.assets {
                let Some(locked) = lock.assets.get(url) else {
                    continue;
                };
                for f in &locked.files {
                    out.push((
                        f.sha256.clone(),
                        pkg.id.clone(),
                        rec.version.clone(),
                        f.path.clone(),
                    ));
                }
            }
        }
    }
    out
}

/// (lowercase basename) -> ids that ship a file of that name, optionally
/// restricted to versions that cover the detected game. Drives the root
/// scan in `which` and the dependency-presence heuristic.
pub fn payload_basenames(
    pkgs: &PackagesFile,
    lock: &LockFile,
    game: Option<&str>,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut m: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for pkg in &pkgs.packages {
        for rec in &pkg.versions {
            if let Some(g) = game
                && !version_covers_game(rec, g)
            {
                continue;
            }
            for url in &rec.assets {
                let Some(locked) = lock.assets.get(url) else {
                    continue;
                };
                for f in &locked.files {
                    let base = f
                        .path
                        .rsplit('/')
                        .next()
                        .unwrap_or(f.path.as_str())
                        .to_lowercase();
                    if base.is_empty() {
                        continue;
                    }
                    m.entry(base).or_default().insert(pkg.id.clone());
                }
            }
        }
    }
    m
}

/// Every catalog match for a payload digest: (id, version) pairs in index
/// order. A single match is discriminating evidence for that exact
/// version; several matches mean the file is byte-identical across those
/// releases and cannot tell them apart on its own.
pub fn identify_digests<'a>(
    index: &'a [(String, String, String, String)],
    sha256: &str,
) -> Vec<(&'a str, &'a str)> {
    index
        .iter()
        .filter(|(s, _, _, _)| s.as_str() == sha256)
        .map(|(_, id, version, _)| (id.as_str(), version.as_str()))
        .collect()
}
