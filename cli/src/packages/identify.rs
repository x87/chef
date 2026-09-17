//! Payload identification from the lock (used by `which` for unmanaged
//! files and by dependency-presence checks).

use std::collections::{BTreeMap, BTreeSet};

use super::catalog::{LockFile, PackagesFile, version_covers_game};

// ---------------------------------------------------------------------------
// Payload identification (used by `which` for unmanaged files)
// ---------------------------------------------------------------------------

/// One payload entry as indexed from the lock: (sha256, package id,
/// release version, archive path). The digest does not depend on the
/// deployed name, so it names the owner of a file whatever a postinstall
/// rename called it.
pub type PayloadEntry = (String, String, String, String);

/// One entry per payload file in the lock (see [`PayloadEntry`]).
/// Lets chef name an unknown on-disk file by its sha256 alone, and map
/// root payload filenames back to the ids that ship them.
pub fn payload_index(pkgs: &PackagesFile, lock: &LockFile) -> Vec<PayloadEntry> {
    let mut out: Vec<PayloadEntry> = Vec::new();
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
pub fn identify_digests<'a>(index: &'a [PayloadEntry], sha256: &str) -> Vec<(&'a str, &'a str)> {
    index
        .iter()
        .filter(|(s, _, _, _)| s.as_str() == sha256)
        .map(|(_, id, version, _)| (id.as_str(), version.as_str()))
        .collect()
}

/// The id of a package that ships these exact bytes, other than `exclude`.
/// Two packages can claim one path (SAL and UAL both deploy
/// `vorbisFile.dll` in GTA SA); the digest says whose payload the file
/// really is.
pub fn digest_owner_other_than<'a>(
    index: &'a [PayloadEntry],
    sha256: &str,
    exclude: &str,
) -> Option<&'a str> {
    identify_digests(index, sha256)
        .into_iter()
        .find(|(id, _)| *id != exclude)
        .map(|(id, _)| id)
}
