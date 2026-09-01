//! Safe archive extraction (ZIP in V1), with traversal and size guards.

use std::path::Path;

use anyhow::{Context, bail};

/// Size limit for one uncompressed asset (1Gb)
const MAX_TOTAL_UNCOMPRESSED: u64 = 1024 * 1024 * 1024;

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
