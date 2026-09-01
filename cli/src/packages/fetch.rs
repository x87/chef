//! Catalog intake: local override -> fresh fetch within TTL -> cached
//! mirror, with schema guards and size caps.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, bail};
use log::warn;

use crate::utils::fs::write_atomic;
use crate::utils::http::fetch_url;

use super::catalog::{LockFile, PackagesFile};
use super::home::{lock_mirror, packages_mirror};

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
