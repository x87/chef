use std::io::Read as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use log::info;

use crate::packages::chef_home;
use crate::utils::fs::{sha256_file, write_atomic};

/// Generous ceiling for release assets (the 20 MB cap applies to the
/// catalog, 64 MB to the lock).
const ASSET_MAX_BYTES: u64 = 512 * 1024 * 1024;

/// Store root: `<chef_home>/store`.
pub fn store_root() -> PathBuf {
    chef_home().join("store")
}

fn store_version_dir(pkg: &str, version: &str) -> PathBuf {
    store_root().join(pkg).join(version)
}

fn complete_marker(pkg: &str, version: &str) -> PathBuf {
    store_version_dir(pkg, version).join(".complete")
}

fn cache_dir() -> PathBuf {
    chef_home().join("cache")
}

// ---------------------------------------------------------------------------
// Download
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Safe ZIP extraction
// ---------------------------------------------------------------------------

/// Validate an entry path: rejects absolute paths, Windows drive letters,
/// UNC prefixes, and any `..` component that would escape the extraction root.
pub fn sanitize_entry(name: &str) -> anyhow::Result<Option<String>> {
    if name.is_empty() {
        return Ok(None);
    }
    let lower = name.to_lowercase();
    if lower.starts_with('/')
        || lower.starts_with('\\')
        || lower.contains(":\\")
        || lower.contains(':') && lower.as_bytes().get(1) == Some(&b':')
        || lower.starts_with("\\\\")
    {
        bail!("archive contains absolute path: {name:?}");
    }
    let normalized = name.replace('\\', "/");
    // Resolve "." / ".." against a stack; ".." above the root is rejected.
    let mut stack: Vec<&str> = Vec::new();
    for comp in normalized.split('/') {
        match comp {
            "" | "." => continue,
            ".." => {
                if stack.pop().is_none() {
                    bail!("archive path escapes extraction root: {name:?}");
                }
            }
            _ => stack.push(comp),
        }
    }
    if stack.is_empty() {
        return Ok(None);
    }
    Ok(Some(stack.join("/")))
}

/// Extract a ZIP archive safely into `outdir`. Only ZIP is supported in V1.
pub fn extract_zip(archive: &Path, outdir: &Path) -> anyhow::Result<()> {
    let file = std::fs::File::open(archive)
        .with_context(|| format!("cannot open archive {}", archive.display()))?;
    let mut zip = zip::ZipArchive::new(file)
        .with_context(|| format!("{} is not a readable ZIP archive", archive.display()))?;
    std::fs::create_dir_all(outdir)?;

    const MAX_TOTAL_UNCOMPRESSED: u64 = 1024 * 1024 * 1024;
    let mut total: u64 = 0;

    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let rel = match sanitize_entry(entry.name()) {
            Ok(Some(rel)) => rel,
            Ok(None) => continue,
            Err(e) => return Err(e),
        };
        if entry.is_dir() {
            std::fs::create_dir_all(outdir.join(&rel))?;
            continue;
        }
        total = total.saturating_add(entry.size());
        if total > MAX_TOTAL_UNCOMPRESSED {
            bail!("archive expands beyond decompression limit");
        }
        let target = outdir.join(&rel);
        std::fs::create_dir_all(target.parent().unwrap_or_else(|| Path::new(".")))?;
        let mut out = std::fs::File::create(&target)?;
        std::io::copy(&mut entry, &mut out)?;
    }
    Ok(())
}

/// Ensure an extracted, verified payload exists in the store and return its
/// directory. The archive is selected by URL and verified against the lock
/// digest before extraction.
pub fn ensure_payload(
    key: &str,
    display: &str,
    version: &str,
    url: &str,
    sha256: &str,
    quiet: bool,
) -> anyhow::Result<PathBuf> {
    let vdir = store_version_dir(key, version);
    if complete_marker(key, version).exists() {
        return Ok(vdir);
    }
    // Existing dir without marker is treated as a cache miss: full re-extract.
    if vdir.exists() {
        std::fs::remove_dir_all(&vdir)?;
    }

    let name = url.rsplit('/').next().unwrap_or("archive").to_string();
    if !quiet {
        info!("downloading {display} {version}");
    }
    let archive = fetch_asset(url, sha256, key, version)?;
    if name.to_lowercase().ends_with(".zip") {
        extract_zip(&archive, &vdir)?;
    } else {
        // Raw single-file assets (e.g. a bare vorbisFile.dll release).
        std::fs::create_dir_all(&vdir)?;
        std::fs::copy(&archive, vdir.join(&name))?;
    }
    write_atomic(&complete_marker(key, version), version.as_bytes())?;
    Ok(vdir)
}
