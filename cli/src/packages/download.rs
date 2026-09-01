//! Download and digest verification into the store/cache layout.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

use super::home::chef_home;
use crate::utils::fs::sha256_file;

/// Size limit for one release asset (the compressed archive)
const ASSET_MAX_BYTES: u64 = 512 * 1024 * 1024;

/// Store root: `<chef_home>/store`.
pub fn store_root() -> PathBuf {
    chef_home().join("store")
}

pub(crate) fn store_version_dir(pkg: &str, version: &str) -> PathBuf {
    store_root().join(pkg).join(version)
}

pub(crate) fn complete_marker(pkg: &str, version: &str) -> PathBuf {
    store_version_dir(pkg, version).join(".complete")
}

fn cache_dir() -> PathBuf {
    chef_home().join("cache")
}

/// Fetch `url` to `dest` (no resume in V1; partial downloads go to `.part`
/// and are renamed only after the caller verifies the digest).
pub fn download(url: &str, dest: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dest.parent().unwrap_or_else(|| Path::new(".")))?;
    let part = dest.with_extension(format!(
        "{}part",
        dest.extension()
            .and_then(|e| e.to_str())
            .map(|e| format!("{e}."))
            .unwrap_or_default()
    ));
    // Discard any stale .part from a previous interrupted run.
    let _ = std::fs::remove_file(&part);

    if let Some(path) = file_url_path(url) {
        // Local payload support (offline testing / mirrors).
        let path2 = path.clone();
        std::fs::copy(&path2, &part).with_context(|| format!("cannot copy local asset {path}"))?;
    } else {
        let resp = crate::utils::http::http_agent(std::time::Duration::from_secs(60))
            .get(url)
            .call()
            .map_err(|e| anyhow::anyhow!("download failed: {e}"))?;
        let mut reader = resp.into_reader().take(ASSET_MAX_BYTES.saturating_add(1));
        let mut out = std::fs::File::create(&part)?;
        std::io::copy(&mut reader, &mut out)?;
        let size = out.metadata()?.len();
        drop(out);
        if size > ASSET_MAX_BYTES {
            let _ = std::fs::remove_file(&part);
            bail!("downloaded asset exceeds {ASSET_MAX_BYTES} byte limit");
        }
    }
    std::fs::rename(&part, dest)?;
    Ok(())
}

fn file_url_path(url: &str) -> Option<String> {
    let rest = url.strip_prefix("file://")?;
    // file:///C:/... -> strip leading slash for Windows drive paths.
    #[cfg(windows)]
    let rest = rest.strip_prefix('/').unwrap_or(rest);
    Some(rest.to_string())
}

/// Download to cache and verify against the lock digest. The user agent
/// and platform checks are identical to the catalog fetch path.
pub fn fetch_asset(url: &str, sha256: &str, pkg: &str, version: &str) -> anyhow::Result<PathBuf> {
    let name = url.rsplit('/').next().unwrap_or("asset").to_string();
    let dest = cache_dir().join(format!("{pkg}-{version}-{name}"));
    if dest.exists() {
        let got = sha256_file(&dest)?;
        if got == sha256 {
            return Ok(dest);
        }
        // Corrupt cache entry: re-download.
        let _ = std::fs::remove_file(&dest);
    }
    download(url, &dest).with_context(|| format!("downloading {name}"))?;
    let got = sha256_file(&dest)?;
    if got != sha256 {
        let _ = std::fs::remove_file(&dest);
        bail!(
            "checksum mismatch for {}: expected sha256 {}, got {}",
            name,
            sha256,
            got
        );
    }
    Ok(dest)
}
