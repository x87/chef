use anyhow::{Context, bail};
use log::warn;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::packages::{PackagesFile, ResolvedVersion, chef_home};
use crate::utils::fs::write_atomic;
use crate::utils::walk::{copy_tree, paths_equal, prune_empty_tree, restore_tree};

pub const SUPPORTED_STATE_SCHEMA: u32 = 2;

// ---------------------------------------------------------------------------
// Directory resolution
// ---------------------------------------------------------------------------

/// `--dir` -> cwd; canonicalized before use.
pub fn resolve_game_dir(flag: Option<&Path>) -> anyhow::Result<PathBuf> {
    let chosen: PathBuf = flag
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    // Canonicalize; create the dir only if it plausibly exists as a target.
    // `dunce` strips the `\\?\` prefix std adds on Windows so state keys
    // and output stay readable.
    let canon = if chosen.exists() {
        dunce::canonicalize(&chosen)?
    } else {
        bail!("game directory {} does not exist", chosen.display())
    };
    safety_guard(&canon)?;
    #[cfg(test)]
    crate::dbg_trace(format_args!(
        "resolve_game_dir in={} -> out={} key={}",
        chosen.display(),
        canon.display(),
        dir_hash(&canon)
    ));
    Ok(canon)
}

/// Safe guards: never treat the data home, the user's
/// home or the filesystem root as a game directory.
fn safety_guard(dir: &Path) -> anyhow::Result<()> {
    let bad = [
        ("the data home", Some(chef_home())),
        ("the user home", dirs::home_dir()),
        ("the filesystem root", Some(PathBuf::from("/"))),
    ];
    for (label, p) in bad.iter() {
        if let Some(p) = p
            && paths_equal(dir, p)
        {
            bail!(
                "refusing to operate on {label} ({}) as a game directory",
                p.display()
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Game detection
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DetectedGame {
    pub exe: String,
    /// Game family ("sa" / "iii" / "vc") from the catalog exe map.
    pub family: String,
}

/// Human-readable name for a game family (the catalog's game ids).
pub fn family_title(family: &str) -> &'static str {
    match family {
        "gta-sa" => "GTA San Andreas",
        "gta-3" => "GTA III",
        "gta-vc" => "GTA Vice City",
        _ => "unknown game",
    }
}

/// Detect the game from executable names in the target directory (root only)
/// using the catalog's exe -> game-id map. Multiple recognized exes mapping
/// to different games -> ambiguous error. Exe names match case-insensitively
/// on all platforms.
pub fn detect_game(pkgs: &PackagesFile, dir: &Path) -> anyhow::Result<Option<DetectedGame>> {
    let mut found: BTreeMap<String, String> = BTreeMap::new(); // game -> exe
    for e in std::fs::read_dir(dir)?.flatten() {
        let name = e.file_name().to_string_lossy().to_lowercase();
        if let Some(game) = pkgs.games.get(&name) {
            found.entry(game.clone()).or_insert(name);
        }
    }
    if found.is_empty() {
        return Ok(None);
    }
    if found.len() > 1 {
        let list = found
            .iter()
            .map(|(f, e)| format!("{e} -> {}", family_title(f)))
            .collect::<Vec<_>>()
            .join(", ");
        bail!(
            "ambiguous game detection in {}: multiple game executables found ({list})",
            dir.display()
        );
    }
    let (family, exe) = found.into_iter().next().unwrap();
    Ok(Some(DetectedGame { exe, family }))
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedFile {
    /// Relative path inside the game directory (`/`-separated).
    pub path: String,
    pub source: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Install {
    pub package: String,
    pub version: String,
    pub files: Vec<ManagedFile>,
    #[serde(default)]
    pub owned_dirs: Vec<String>,
    #[serde(default)]
    pub backup: Option<String>,
    /// User files that were displaced (backed up) so this install could
    /// take their paths; restored on remove. Snapshots live in this
    /// install's backup folder.
    #[serde(default)]
    pub displaced: Vec<String>,
    pub at: u64,
}

impl Install {
    pub fn managed_paths(&self) -> Vec<String> {
        self.files.iter().map(|f| f.path.clone()).collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GameDirState {
    /// Keyed by the persistent package id - at most one entry per id.
    pub installs: BTreeMap<String, Install>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateFile {
    pub schema: u32,
    #[serde(default)]
    pub dirs: BTreeMap<String, GameDirState>,
}

pub fn state_path() -> PathBuf {
    chef_home().join("state.json")
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl StateFile {
    pub fn load() -> anyhow::Result<StateFile> {
        let path = state_path();

        #[cfg(test)]
        crate::dbg_trace(format_args!(
            "StateFile::load path={} exists={}",
            path.display(),
            path.exists()
        ));

        if !path.exists() {
            return Ok(StateFile {
                schema: SUPPORTED_STATE_SCHEMA,
                dirs: BTreeMap::new(),
            });
        }

        let bytes = std::fs::read(&path)?;
        let st: StateFile = serde_json::from_slice(&bytes).map_err(|e| {
            anyhow::anyhow!(
                "chef state at {} is corrupt ({e}); inspect the backups folder in the chef data directory before deleting it",
                path.display()
            )
        })?;

        if st.schema > SUPPORTED_STATE_SCHEMA {
            bail!(
                "state schema {} is newer than supported {} - please upgrade chef",
                st.schema,
                SUPPORTED_STATE_SCHEMA
            );
        }
        #[cfg(test)]
        crate::dbg_trace(format_args!(
            "StateFile::load -> dirs={} keys=[{}]",
            st.dirs.len(),
            st.dirs.keys().cloned().collect::<Vec<_>>().join(",")
        ));
        Ok(st)
    }

    pub fn save(&self) -> anyhow::Result<()> {
        #[cfg(test)]
        crate::dbg_trace(format_args!(
            "StateFile::save path={} dirs={} keys=[{}] installs=[{}]",
            state_path().display(),
            self.dirs.len(),
            self.dirs.keys().cloned().collect::<Vec<_>>().join(","),
            self.dirs
                .values()
                .flat_map(|d| d.installs.keys())
                .cloned()
                .collect::<Vec<_>>()
                .join(",")
        ));
        write_atomic(&state_path(), &serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }

    pub fn dir_state(&self, dir_key: &str) -> &GameDirState {
        use std::sync::LazyLock;
        static EMPTY: LazyLock<GameDirState> = LazyLock::new(GameDirState::default);
        self.dirs.get(dir_key).unwrap_or(&EMPTY)
    }

    pub fn install_of(&self, dir_key: &str, id: &str) -> Option<&Install> {
        self.dirs.get(dir_key)?.installs.get(id)
    }

    /// All installs recorded for one game directory.
    pub fn installs_in(&self, dir_key: &str) -> Vec<&Install> {
        self.dir_state(dir_key).installs.values().collect()
    }
}

// ---------------------------------------------------------------------------
// Advisory per-game-dir lock
// ---------------------------------------------------------------------------

const LOCK_STALE_SECS: u64 = 30;

pub struct Lock {
    path: PathBuf,
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn dir_hash(dir: &Path) -> String {
    // Canonicalize so every spelling of the same directory (raw path, 8.3
    // short names, `..` components, trailing separators, case) maps to one
    // state key - matching what `resolve_game_dir` feeds the commands. CI
    // runners set %TEMP% to a short name (e.g. C:\Users\RUNNER~1), so the
    // test harness's raw path and the CLI's canonical path used to diverge.
    let canon = dunce::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let mut h = crc32fast::Hasher::new();
    h.update(canon.to_string_lossy().to_lowercase().as_bytes());
    format!("{:08x}", h.finalize())
}

impl Lock {
    pub fn acquire(game_dir: &Path) -> anyhow::Result<Lock> {
        let dir = chef_home().join("locks");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.lock", dir_hash(game_dir)));

        match std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
        {
            Ok(mut f) => {
                writeln!(f, "{}", std::process::id())?;
                Ok(Lock { path })
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Stale-lock takeover after 30 s.
                let stale = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                let age = stale
                    .map(|t| t.elapsed().map(|d| d.as_secs()).unwrap_or(u64::MAX))
                    .unwrap_or(u64::MAX);
                if age > LOCK_STALE_SECS {
                    warn!("taking over stale lock {} (age {age}s)", path.display());
                    std::fs::remove_file(&path)?;
                    Lock::acquire(game_dir)
                } else {
                    bail!(
                        "another chef process is working on this game directory (lock: {})",
                        path.display()
                    );
                }
            }
            Err(e) => Err(e).with_context(|| format!("cannot create lock {}", path.display())),
        }
    }
}

// ---------------------------------------------------------------------------
// Backups
// ---------------------------------------------------------------------------

pub fn backup_root_for(game_dir: &Path) -> PathBuf {
    chef_home().join("backups").join(dir_hash(game_dir))
}

fn new_backup_dir(game_dir: &Path) -> anyhow::Result<PathBuf> {
    let ts = unix_now().to_string();
    let root = backup_root_for(game_dir);
    // The timestamp is second-resolution: successive deploys within the
    // same second must not share a backup folder (generations would mix
    // and the displaced-file carry-over would copy a path onto itself).
    let mut dir = root.join(&ts);
    let mut n = 0;

    while dir.exists() {
        n += 1;
        dir = root.join(format!("{ts}-{n}"));
    }

    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

enum DigestVerdict {
    Match,
    Modified,
}

fn compare_digest(path: &Path, expected: &str) -> DigestVerdict {
    match crate::utils::fs::sha256_file(path) {
        Ok(got) if got == expected => DigestVerdict::Match,
        _ => DigestVerdict::Modified,
    }
}

// ---------------------------------------------------------------------------
// On-disk presence of a recorded install
// ---------------------------------------------------------------------------

/// Classification of a recorded install's files on disk. `present` and
/// `missing` are complementary; a missing file is `moved` when its digest
/// matches a file found elsewhere in the game root.
#[derive(Debug, Default)]
pub struct InstallCheck {
    /// Relative paths present at their recorded location.
    pub present: Vec<String>,
    /// Recorded relative paths that are absent from the game dir.
    pub missing: Vec<String>,
    /// (recorded path, found absolute path) for each missing file whose
    /// digest appeared elsewhere in the game root.
    pub moved: Vec<(String, PathBuf)>,
}

impl InstallCheck {
    pub fn all_present(&self) -> bool {
        self.missing.is_empty()
    }

    /// Nothing of the install remains in the game dir (and nothing was
    /// found moved) - the recording is stale.
    pub fn fully_gone(&self) -> bool {
        self.present.is_empty() && self.moved.is_empty()
    }
}

/// Classify a recorded install against the game dir: which recorded paths
/// are present, which are missing, and whether any missing file was moved
/// (digest match elsewhere in the root). Root-only, matching chef v1's
/// discovery scope.
pub fn check_install(game_dir: &Path, inst: &Install) -> InstallCheck {
    let mut out = InstallCheck::default();

    for mf in &inst.files {
        if game_dir.join(&mf.path).exists() {
            out.present.push(mf.path.clone());
        } else {
            out.missing.push(mf.path.clone());
        }
    }

    if out.missing.is_empty() {
        return out;
    }

    // Index root files by digest once (first match wins, mirroring the
    // older per-file scans) and look the missing digests up in it.
    let expected: BTreeMap<&str, &str> = inst
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.sha256.as_str()))
        .collect();
    let mut by_digest: BTreeMap<String, PathBuf> = BTreeMap::new();

    if let Ok(entries) = std::fs::read_dir(game_dir) {
        for e in entries.flatten() {
            let p = e.path();
            if !p.is_file() {
                continue;
            }
            if let Some(d) = crate::utils::fs::sha256_file(&p).ok()
                && !by_digest.contains_key(&d)
            {
                by_digest.insert(d, p);
            }
        }
    }

    for rel in &out.missing {
        if let Some(exp) = expected.get(rel.as_str())
            && let Some(found) = by_digest.get(*exp)
        {
            out.moved.push((rel.clone(), found.clone()));
        }
    }
    out
}

/// Live-refresh pass: drop recorded installs whose files are all gone and
/// not found (moved) elsewhere in the root, so manual deletes stop showing
/// as installed. Returns true when anything was pruned.
pub fn prune_stale_state(game_dir: &Path, state: &mut StateFile) -> bool {
    let key = dir_hash_key(game_dir);
    let Some(dir_state) = state.dirs.get(&key) else {
        return false;
    };
    let to_prune: Vec<String> = dir_state
        .installs
        .values()
        .filter(|inst| check_install(game_dir, inst).fully_gone())
        .map(|inst| inst.package.clone())
        .collect();

    if to_prune.is_empty() {
        return false;
    }

    let dir_state = state.dirs.get_mut(&key).expect("entry exists");

    for id in &to_prune {
        dir_state.installs.remove(id);
    }

    if dir_state.installs.is_empty() {
        state.dirs.remove(&key);
    }
    true
}

// ---------------------------------------------------------------------------
// Staged deployment transaction
// ---------------------------------------------------------------------------

/// One file staged for deployment.
#[derive(Debug, Clone)]
pub struct DeployFile {
    /// Destination path relative to the game root (always `/`-separated).
    pub dest_rel: String,
    /// Source path in the store.
    pub src: PathBuf,
    pub sha256: String,
}

pub struct DeployRequest<'a> {
    pub product: &'a ResolvedVersion,
    /// Ids occupying the same slot (the product's id plus its `replaces`
    /// partners); any existing install among them is evicted first.
    pub slot: &'a [String],
    pub version: &'a str,
    pub files: &'a [DeployFile],
    pub dry_run: bool,
}

pub struct DeployOutcome {
    pub replaced: Option<Install>,
    pub preserved_modified: Vec<String>,
    /// User files backed up so the deploy could proceed; restored by
    /// 'chef remove'.
    pub displaced: Vec<String>,
}

/// Execute the staged transaction:
/// stage -> backup occupant -> remove occupant -> move staged into place ->
/// write state atomically. On failure after backup: restore snapshots, delete
/// staged files, leave previous state untouched, exit 1 with a report.
///
/// Caller must hold the game-dir lock.
pub fn deploy(game_dir: &Path, req: DeployRequest<'_>) -> anyhow::Result<DeployOutcome> {
    let mut state = StateFile::load()?;
    let key = dir_hash_key(game_dir);

    let id = req.product.id.as_str();

    // Collision policy: files not managed by chef are backed up and their
    // paths taken; 'chef remove' restores them. Compare case-insensitively -
    // game directories live on case-insensitive filesystems and manifests
    // may differ in case from recorded paths. Managed paths are the union
    // across the whole slot (every install this deploy replaces).
    let existing_paths: Vec<String> = req
        .slot
        .iter()
        .filter_map(|sid| state.install_of(&key, sid))
        .flat_map(|i| i.managed_paths().into_iter().map(|p| p.to_lowercase()))
        .collect();
    let mut collisions = Vec::new();

    for f in req.files {
        let dest = game_dir.join(&f.dest_rel);
        if dest.exists()
            && !existing_paths
                .iter()
                .any(|p| *p == f.dest_rel.to_lowercase())
        {
            collisions.push(f.dest_rel.clone());
        }
    }

    collisions.sort();

    if req.dry_run {
        return Ok(DeployOutcome {
            replaced: None,
            preserved_modified: vec![],
            displaced: vec![],
        });
    }

    // 1. Stage new payload copies.
    let staging = tempfile::tempdir()?;

    for f in req.files {
        let tmp_target = staging.path().join(&f.dest_rel);
        std::fs::create_dir_all(tmp_target.parent().unwrap_or_else(|| Path::new(".")))?;
        std::fs::copy(&f.src, &tmp_target)?;
    }

    let previous = req
        .slot
        .iter()
        .filter_map(|sid| state.install_of(&key, sid).cloned())
        .next();

    // 2. Snapshot the current occupant, skipping user-modified files (sec.5.4).
    let mut preserved: Vec<String> = Vec::new();
    let mut backed_up: Vec<(String, PathBuf)> = Vec::new();
    let backup_dir = if previous.is_some() || !collisions.is_empty() {
        Some(new_backup_dir(game_dir)?)
    } else {
        None
    };

    if let Some(prev) = &previous {
        for mf in &prev.files {
            let abs = game_dir.join(&mf.path);
            if !abs.exists() {
                continue;
            }
            if matches!(compare_digest(&abs, &mf.sha256), DigestVerdict::Match) {
                let snap = backup_dir.as_ref().unwrap().join(&mf.path);
                std::fs::create_dir_all(snap.parent().unwrap_or_else(|| Path::new(".")))?;
                std::fs::copy(&abs, &snap).with_context(|| {
                    format!("snapshotting {} -> {}", abs.display(), snap.display())
                })?;
                backed_up.push((mf.path.clone(), snap));
            } else {
                preserved.push(mf.path.clone());
            }
        }
    }

    // Snapshot the user files we are about to displace.
    for rel in &collisions {
        let bdir_ref = backup_dir.as_ref().unwrap();
        let abs = game_dir.join(rel);
        let snap = bdir_ref.join(rel);
        std::fs::create_dir_all(snap.parent().unwrap_or_else(|| Path::new(".")))?;
        std::fs::copy(&abs, &snap).with_context(|| format!("backing up {rel}"))?;
        backed_up.push((rel.clone(), snap));
    }

    // Carry over snapshots of user files displaced by earlier installs of
    // this package, so a single backup folder serves this generation.
    let mut displaced: Vec<String> = collisions.clone();

    if let Some(prev) = &previous
        && !prev.displaced.is_empty()
    {
        let old_root = prev
            .backup
            .as_ref()
            .map(|b| backup_root_for(game_dir).join(b));
        for rel in &prev.displaced {
            if displaced.iter().any(|d| d.eq_ignore_ascii_case(rel)) {
                continue;
            }
            let Some(old_root) = &old_root else {
                continue;
            };
            let from = old_root.join(rel);
            if from.exists() {
                let to = backup_dir.as_ref().unwrap().join(rel);
                std::fs::create_dir_all(to.parent().unwrap_or_else(|| Path::new(".")))?;
                std::fs::copy(&from, &to)?;
                displaced.push(rel.clone());
            }
        }
    }

    // Record the backup folder name so `remove` can restore it (sec.5.4).
    let backup_name = backup_dir
        .as_ref()
        .and_then(|p| p.file_name().map(|f| f.to_string_lossy().to_string()));

    let owned_dirs = owned_dirs_of(req.files);

    // Rollback bookkeeping: snapshots of occupant files + newly deployed files.
    let new_install = Install {
        package: req.product.id.clone(),
        version: req.version.to_string(),
        files: req
            .files
            .iter()
            .map(|f| ManagedFile {
                path: f.dest_rel.clone(),
                source: f
                    .src
                    .strip_prefix(crate::store::store_root())
                    .map(|r| format!("store/{}", r.to_string_lossy().replace('\\', "/")))
                    .unwrap_or_else(|_| f.src.display().to_string()),
                sha256: f.sha256.clone(),
            })
            .collect(),
        owned_dirs: owned_dirs.clone(),
        backup: backup_name,
        displaced: displaced.clone(),
        at: unix_now(),
    };

    // 3-5. Remove occupant -> move staged into place -> write state atomically.
    // A failure anywhere in here triggers the rollback below.
    let result = (|| -> anyhow::Result<()> {
        if let Some(prev) = &previous {
            remove_install_files(game_dir, prev, &preserved)?;
        }
        copy_tree(staging.path(), game_dir)?;
        let dir_state = state.dirs.entry(key.clone()).or_default();
        // Evict every slot occupant, then record the new product.
        for sid in req.slot {
            dir_state.installs.remove(sid);
        }
        dir_state.installs.insert(id.to_string(), new_install);
        state.save()?;
        Ok(())
    })();

    if let Err(e) = result {
        #[cfg(test)]
        crate::dbg_trace(format_args!("deploy FAILED id={id}: {e:#}"));
        // Rollback: restore snapshots over pre-existing files, delete newly
        // deployed files, leave previous state untouched.
        let mut report = vec![format!("deployment failed: {e:#}")];

        for (rel, snap) in &backed_up {
            let abs = game_dir.join(rel);
            let _ = std::fs::create_dir_all(abs.parent().unwrap_or_else(|| Path::new(".")));
            if std::fs::copy(snap, &abs).is_ok() {
                report.push(format!("restored snapshot: {rel}"));
            } else {
                report.push(format!("FAILED to restore snapshot: {rel}"));
            }
        }

        for f in req.files {
            let abs = game_dir.join(&f.dest_rel);
            if abs.exists() {
                let _ = std::fs::remove_file(&abs);
                report.push(format!("deleted staged file: {}", f.dest_rel));
            }
        }

        anyhow::bail!(
            "rolled back - previous state intact\n{}",
            report.join("\n  ")
        );
    }

    #[cfg(test)]
    crate::dbg_trace(format_args!(
        "deploy OK id={id} version={} game={} home={} key={key} state_dirs={}",
        req.version,
        game_dir.display(),
        chef_home().display(),
        state.dirs.len()
    ));

    // 6. Release happens via Drop.
    for p in &preserved {
        warn!("preserved user-modified file: {p}");
    }

    Ok(DeployOutcome {
        replaced: previous,
        preserved_modified: preserved,
        displaced,
    })
}

/// Top-level directories used by the staged files (owned by the package
/// and pruned when empty on replace/remove). Derived from the payload
/// paths - the catalog declares nothing extra.
fn owned_dirs_of(files: &[DeployFile]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for f in files {
        let Some(first) = f.dest_rel.split('/').next() else {
            continue;
        };

        if !first.is_empty()
            && f.dest_rel.contains('/')
            && !out.iter().any(|d| d.eq_ignore_ascii_case(first))
        {
            out.push(first.to_string());
        }
    }
    out
}

/// Remove a managed installation with modified-file preservation and backup
/// restore (sec.12). Caller must hold the lock.
pub fn remove_install(
    game_dir: &Path,
    id: &str,
    expect_version: Option<&str>,
) -> anyhow::Result<(Install, Vec<String>, Vec<String>)> {
    let mut state = StateFile::load()?;
    let key = dir_hash_key(game_dir);
    let inst = state
        .dirs
        .get(&key)
        .and_then(|d| d.installs.get(id))
        .cloned()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no installation with id '{id}' found in {}",
                game_dir.display()
            )
        })?;

    if let Some(v) = expect_version {
        let v_norm = v.strip_prefix('v').unwrap_or(v).trim_end_matches('.');
        let matches = inst.version == v_norm
            || inst.version == v
            || inst.version.starts_with(&format!("{v_norm}."));
        if !matches {
            bail!(
                "installed version of {} is {}, not {v}",
                inst.package,
                inst.version
            );
        }
    }

    let mut preserved = Vec::new();

    for mf in &inst.files {
        let abs = game_dir.join(&mf.path);
        if !abs.exists() {
            continue;
        }
        if matches!(compare_digest(&abs, &mf.sha256), DigestVerdict::Match) {
            let _ = std::fs::remove_file(&abs);
        } else {
            preserved.push(mf.path.clone());
        }
    }

    // Restore backup where applicable - never over a user-modified file.
    // Report every file that actually came back, exactly once.
    let mut restored: Vec<String> = Vec::new();
    if let Some(bdir) = &inst.backup {
        let bpath = backup_root_for(game_dir).join(bdir);
        if bpath.exists() {
            restored.extend(restore_tree(&bpath, game_dir, &preserved));
        }
        // Snapshots of displaced user files not already covered above
        // (e.g. when the backup dir was pruned).
        for rel in &inst.displaced {
            if restored.contains(rel) || preserved.contains(rel) {
                continue;
            }
            if let Some(snap) = bpath.join(rel).exists().then(|| bpath.join(rel)) {
                let abs = game_dir.join(rel);
                if !abs.exists() {
                    let _ = std::fs::create_dir_all(abs.parent().unwrap_or_else(|| Path::new(".")));
                    if std::fs::copy(&snap, &abs).is_ok() {
                        restored.push(rel.clone());
                    }
                }
            }
        }
    }

    // Prune owned directories (only when empty).
    prune_owned_dirs(game_dir, &inst);

    state
        .dirs
        .get_mut(&key)
        .expect("entry exists")
        .installs
        .remove(id);
    state.save()?;

    Ok((inst, preserved, restored))
}

fn remove_install_files(
    game_dir: &Path,
    inst: &Install,
    preserved: &[String],
) -> anyhow::Result<()> {
    for mf in &inst.files {
        if preserved.contains(&mf.path) {
            continue;
        }
        let abs = game_dir.join(&mf.path);
        let _ = std::fs::remove_file(&abs);
    }
    prune_owned_dirs(game_dir, inst);
    Ok(())
}

fn prune_owned_dirs(game_dir: &Path, inst: &Install) {
    for od in &inst.owned_dirs {
        let dir = game_dir.join(od.replace('/', std::path::MAIN_SEPARATOR_STR));
        prune_empty_tree(&dir, game_dir);
    }
}

pub fn dir_hash_key(game_dir: &Path) -> String {
    dir_hash(game_dir)
}
