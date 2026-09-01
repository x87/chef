use anyhow::Context;
use sha2::{Digest, Sha256};
use std::fs::{File, create_dir_all};
use std::io::Read as _;
use std::path::Path;

/// Write `bytes` to `path` atomically: temp file in the same directory,
/// then rename into place. Creates parent directories.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    create_dir_all(dir)?;

    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    std::io::Write::write_all(&mut tmp, bytes)?;

    tmp.persist(path)
        .map_err(|e| anyhow::anyhow!("failed to persist {}: {e}", path.display()))?;
    Ok(())
}

/// SHA-256 hex digest of a file's contents.
pub fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let mut f = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}
