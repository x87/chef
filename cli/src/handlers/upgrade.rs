use std::io::Read as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use log::{info, warn};
use serde::{Deserialize, Serialize};

use crate::ChefError;
use crate::packages::chef_home;
use crate::utils::fs::write_atomic;
use crate::utils::http;

const REPO: &str = "x87/chef";
const CHECK_INTERVAL_DAYS: u64 = 7;

/// Check for a newer chef release and, unless `check` only, download and
/// replace the running binary.
pub fn run(check: bool, json: bool) -> crate::Result<()> {
    let release = api_latest()
        .with_context(|| format!("fetching latest release of {REPO}"))
        .map_err(ChefError::Other)?;

    let tag = release
        .get("tag_name")
        .and_then(|t| t.as_str())
        .unwrap_or("?")
        .trim_start_matches('v')
        .to_string();

    let latest =
        semver::Version::parse(&tag).map_err(|e| ChefError::Other(anyhow::anyhow!("{e}")))?;
    let cur = current_version();

    if check {
        let available = latest > cur;
        if json {
            crate::emit::emit_json(&serde_json::json!({
                "updated": false,
                "current": cur.to_string(),
                "latest": latest.to_string(),
                "updateAvailable": available,
            }));
        } else if available {
            info!("new version v{latest} available (current: v{cur}) - run 'chef upgrade'");
        } else {
            info!("chef is up to date (v{cur})");
        }
        return Ok(());
    }

    if latest <= cur {
        if json {
            crate::emit::emit_json(&serde_json::json!({
                "updated": false,
                "current": cur.to_string(),
                "latest": latest.to_string(),
            }));
        } else {
            info!("chef is up to date (v{cur})");
        }
        return Ok(());
    }

    perform_upgrade(&release).map_err(ChefError::Other)?;
    if json {
        crate::emit::emit_json(&serde_json::json!({
            "updated": true,
            "from": cur.to_string(),
            "to": latest.to_string(),
        }));
    }
    Ok(())
}

fn api_latest() -> anyhow::Result<serde_json::Value> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let resp = http::http_agent(std::time::Duration::from_secs(60))
        .get(&url)
        .call()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    use std::io::Read;

    let mut buf = Vec::new();

    resp.into_reader().read_to_end(&mut buf)?;
    serde_json::from_slice(&buf).context("latest release response is not valid JSON")
}

fn current_version() -> semver::Version {
    semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap()
}

fn parse_tag(v: &serde_json::Value) -> Option<semver::Version> {
    let tag = v.get("tag_name")?.as_str()?;
    semver::Version::parse(tag.trim_start_matches('v')).ok()
}

// ---------------------------------------------------------------------------
// Update notice (non-blocking, at most once every 7 days)
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Default)]
struct CheckState {
    #[serde(default)]
    last_check_epoch: u64,
    #[serde(default)]
    latest_seen: String,
}

fn check_state_path() -> PathBuf {
    chef_home().join("update_check.json")
}

/// Called after successful `use`, `list`, `which`. Best-effort, never fails.
pub fn update_notice() {
    let _ = run_notice();
}

fn run_notice() -> anyhow::Result<()> {
    let mut st: CheckState = std::fs::read(check_state_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    if now.saturating_sub(st.last_check_epoch) < CHECK_INTERVAL_DAYS * 86_400 {
        return Ok(());
    }
    st.last_check_epoch = now;

    // Notice is non-blocking: fetch failures are silently ignored.
    if let Ok(v) = api_latest()
        && let Some(latest) = parse_tag(&v)
    {
        st.latest_seen = latest.to_string();
        if latest > current_version() {
            info!("new version v{latest} available - run 'chef upgrade'");
        }
    }
    let _ = write_atomic(&check_state_path(), &serde_json::to_vec(&st)?);
    Ok(())
}

// ---------------------------------------------------------------------------
// Binary self-update
// ---------------------------------------------------------------------------

/// Release asset name for this build, e.g. `chef-x86_64-pc-windows-msvc.zip`.
fn asset_stem() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "chef-x86_64-pc-windows-msvc",
        ("windows", "aarch64") => "chef-aarch64-pc-windows-msvc",
        _ => panic!("unsupported platform for release builds"),
    }
}

#[derive(Serialize, Deserialize, Default)]
struct TofuCache {
    #[serde(default)]
    digests: std::collections::BTreeMap<String, String>,
}

fn tofu_path() -> PathBuf {
    chef_home().join("tofu.json")
}

fn fetch_bytes(url: &str) -> anyhow::Result<Vec<u8>> {
    let resp = http::http_agent(std::time::Duration::from_secs(120))
        .get(url)
        .call()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut buf = Vec::new();

    resp.into_reader().read_to_end(&mut buf)?;
    Ok(buf)
}

fn expected_digest(asset_name: &str, asset_url: &str) -> anyhow::Result<String> {
    // 1. sidecar digest published in the same release
    match fetch_bytes(&format!("{asset_url}.sha256")) {
        Ok(sidecar) => {
            let text = String::from_utf8_lossy(&sidecar);
            let first = text.split_whitespace().next().unwrap_or("").to_lowercase();
            if first.len() == 64 && first.chars().all(|c| c.is_ascii_hexdigit()) {
                return Ok(first);
            }
            bail!("sidecar digest for {asset_name} is malformed");
        }
        Err(_) => {
            // 2. TOFU fallback - only for chef release assets, never packages.
            let mut cache: TofuCache = std::fs::read(tofu_path())
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
            if let Some(known) = cache.digests.get(asset_name) {
                return Ok(known.clone());
            }

            let tmpdir = tempfile::tempdir()?;
            let tmp = tmpdir.path().join(asset_name);

            crate::packages::download(asset_url, &tmp)
                .with_context(|| format!("downloading {asset_name}"))?;

            let digest = crate::utils::fs::sha256_file(&tmp)?;

            warn!("no .sha256 sidecar for {asset_name} - trusting first-seen digest (TOFU)");

            cache.digests.insert(asset_name.to_string(), digest.clone());
            write_atomic(&tofu_path(), &serde_json::to_vec_pretty(&cache)?)?;
            Ok(digest)
        }
    }
}

fn extract_binary(archive: &Path, dest_dir: &Path) -> anyhow::Result<PathBuf> {
    let name = archive.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let binary_name = if cfg!(windows) { "chef.exe" } else { "chef" };

    if name.ends_with(".zip") {
        crate::packages::extract_zip(archive, dest_dir)?;
        let candidate = find_file(dest_dir, binary_name)?
            .ok_or_else(|| anyhow::anyhow!("archive does not contain {binary_name}"))?;
        Ok(candidate)
    } else {
        // .tar.gz
        let f = std::fs::File::open(archive)?;
        let gz = flate2::read::GzDecoder::new(f);
        let mut tar = tar::Archive::new(gz);
        tar.unpack(dest_dir)?;
        find_file(dest_dir, binary_name)?
            .ok_or_else(|| anyhow::anyhow!("archive does not contain {binary_name}"))
    }
}

fn find_file(root: &Path, name: &str) -> anyhow::Result<Option<PathBuf>> {
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir)?.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().map(|f| f == name).unwrap_or(false) {
                return Ok(Some(p));
            }
        }
    }
    Ok(None)
}

fn perform_upgrade(release: &serde_json::Value) -> anyhow::Result<()> {
    let stem = asset_stem();
    let assets = release
        .get("assets")
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default();
    let asset = assets
        .iter()
        .find(|a| {
            a.get("name")
                .and_then(|n| n.as_str())
                .map(|n| n.starts_with(stem))
                .unwrap_or(false)
        })
        .ok_or_else(|| anyhow::anyhow!("release has no asset matching {stem}.*"))?;
    let name = asset
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap()
        .to_string();
    let url = asset
        .get("browser_download_url")
        .and_then(|u| u.as_str())
        .ok_or_else(|| anyhow::anyhow!("asset {name} lacks a download URL"))?
        .to_string();

    info!("downloading {name}...");

    let digest = expected_digest(&name, &url)?;
    let work = tempfile::tempdir()?;
    let archive = work.path().join(&name);
    crate::packages::download(&url, &archive)?;
    let got = crate::utils::fs::sha256_file(&archive)?;

    if got != digest {
        bail!("checksum mismatch for {name}: expected {digest}, got {got}");
    }

    let extracted = extract_binary(&archive, &work.path().join("x"))?;

    // Replace the running binary.
    let me = std::env::current_exe().context("cannot locate running binary")?;
    let me_old = me.with_extension("exe.old");
    let new_tmp = me.with_extension("new");
    std::fs::copy(&extracted, &new_tmp)?;

    if me.exists() {
        let _ = std::fs::remove_file(&me_old);
        std::fs::rename(&me, &me_old)
            .with_context(|| format!("renaming {} to {}", me.display(), me_old.display()))?;
    }
    std::fs::rename(&new_tmp, &me)?;

    info!(
        "upgraded chef -> {}",
        release
            .get("tag_name")
            .and_then(|t| t.as_str())
            .unwrap_or("?")
    );
    info!(
        "the old binary was kept as {}; it is removed on next successful run",
        me_old.display()
    );
    Ok(())
}
