//! Platform-aware release asset selection (Win32 vs x64 hints).

use anyhow::bail;

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
