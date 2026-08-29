//! `chef which` - report what is installed in a game directory.
//!
//! With no argument it prints a one-line summary per package, split into
//! packages chef installed and packages the user installed by hand. The
//! summary never lists files: a package either shows one release, or the
//! honest labels `multiple` / `unknown` / `not found`, always with a
//! pointer to the details view `chef which <pkg>`.
//!
//! Identification is by content only: a locked path counts as a release's
//! when the file on disk carries that release's recorded sha256. Only
//! paths recorded in `packages.lock` are ever read - the game tree is
//! never walked. User-editable `.ini` config files never decide the match.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::ChefError;
use crate::commands::add::split_pkg_spec;
use crate::game_dir::{self, StateFile};
use crate::match_names;
use crate::packages::{self, LockFile, PackagesFile};

/// One package release with the payload it ships: (version, [(deployed
/// path, sha256)]). Postinstall renames are applied to the deployed path,
/// so the presence check targets the file as it sits on disk.
pub(crate) type VersionFiles = Vec<(String, Vec<(String, String)>)>;

/// One row of the summary, for one package.
#[derive(Debug, Clone)]
pub(crate) struct PackageSummary {
    pub id: String,
    /// User-facing product name.
    pub name: String,
    /// true = recorded as installed by chef; false = user files only.
    pub managed: bool,
    /// "installed" | "multiple" | "unknown" | "not-found"
    pub status: &'static str,
    /// Version cell text: the found release, `multiple`, `unknown`, or
    /// `not found`.
    pub version: String,
    /// Found releases, newest first (full set; not shown in the summary).
    pub versions: Vec<String>,
    /// Human notes column.
    pub notes: String,
}

/// Newest version in a list by semver ordering (used to attribute a file
/// whose bytes match several releases to its newest one).
pub(crate) fn newest_match<'a>(vs: &[&'a str]) -> &'a str {
    let mut s: Vec<&str> = vs.to_vec();
    s.sort_by(|a, b| version_cmp_desc(a, b));
    s.into_iter().next().unwrap_or_default()
}

/// Attribute a file whose bytes match several releases: the installed
/// version wins when it is among the matches (chef knows what it
/// deployed), otherwise the newest. Downgrading 1.5.0 to 1.4.3 keeps
/// byte-identical plugins on disk; without the state preference they
/// would re-anchor the view to 1.5.0 and 1.4.3's missing 1.5.0-only
/// files would read as gaps.
pub(crate) fn attribute<'a>(installed: Option<&str>, matched: &[&'a str]) -> &'a str {
    if let Some(ins) = installed
        && let Some(v) = matched.iter().copied().find(|m| m == &ins)
    {
        return v;
    }
    newest_match(matched)
}

fn version_cmp_desc(a: &str, b: &str) -> std::cmp::Ordering {
    let ka = semver::Version::parse(a)
        .map(|v| (v.major, v.minor, v.patch))
        .unwrap_or((0, 0, 0));
    let kb = semver::Version::parse(b)
        .map(|v| (v.major, v.minor, v.patch))
        .unwrap_or((0, 0, 0));
    kb.cmp(&ka).then_with(|| b.cmp(a))
}

/// User-editable `.ini` config files never decide the outcome: users tune
/// them freely, so a mismatch there is benign and is ignored.
pub(crate) fn is_ignored_config(path: &str) -> bool {
    path.to_lowercase().ends_with(".ini")
}

/// A package reference that is safe to paste inside single quotes in a
/// `chef` command. By convention the first alias is the shortest usable
/// name (`sal`, `cleo5`, `ual`), so notes embed that; the display name is
/// used only when the package has no aliases, the id as a last resort.
pub(crate) fn command_ref(pkgs: &PackagesFile, id: &str) -> String {
    let quote_free = |s: &str| !s.contains('\'');
    let Some(pkg) = pkgs.pkg(id) else {
        return id.to_string();
    };
    if let Some(a) = pkg.aliases.iter().find(|a| quote_free(a)) {
        return a.clone();
    }
    if quote_free(&pkg.name) {
        return pkg.name.clone();
    }
    pkg.id.clone()
}

/// Every release of one package that covers `game`, newest first, with the
/// payload files it ships. Identical semvers (one catalog entry per game
/// group) collapse; a release is skipped when none of its digests are
/// locked.
pub(crate) fn version_files(
    pkgs: &PackagesFile,
    lock: &LockFile,
    id: &str,
    game: Option<&str>,
) -> VersionFiles {
    let Some(pkg) = pkgs.pkg(id) else {
        return Vec::new();
    };
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: VersionFiles = Vec::new();
    for rec in &pkg.versions {
        if let Some(g) = game
            && !packages::version_covers_game(rec, g)
        {
            continue;
        }
        // Skip records never added to the lock (nothing to verify against).
        if !rec.assets.iter().any(|a| lock.assets.contains_key(a)) {
            continue;
        }
        let Some(v) = packages::parse_version_loose(&rec.version) else {
            continue;
        };
        if !seen.insert(v.to_string()) {
            continue;
        }
        let mut files: Vec<(String, String)> = Vec::new();
        for url in &rec.assets {
            let Some(locked) = lock.assets.get(url) else {
                continue;
            };
            for f in &locked.files {
                let deployed = rec
                    .postinstall
                    .as_ref()
                    .and_then(|p| p.rename.get(&f.path))
                    .cloned()
                    .unwrap_or_else(|| f.path.clone());
                files.push((deployed, f.sha256.clone()));
            }
        }
        if !files.is_empty() {
            out.push((v.to_string(), files));
        }
    }
    out.sort_by(|a, b| version_cmp_desc(&a.0, &b.0));
    out
}

/// Lowercase backslash key for path equality (Windows is case-insensitive;
/// the lock stores forward slashes).
fn norm_key(s: &str) -> String {
    s.replace('/', "\\").to_lowercase()
}

/// Backslash display form for a lock path.
fn display_path(s: &str) -> String {
    s.replace('/', "\\")
}

/// Hash only the payload paths the catalog expects below the game root.
/// Presence in the summary is path-anchored, so this never hashes the
/// whole tree - a game folder can hold gigabytes of unrelated files.
fn expected_tree(
    pkgs: &PackagesFile,
    lock: &LockFile,
    game: Option<&str>,
    game_dir: &Path,
) -> BTreeMap<String, String> {
    let mut needed: BTreeMap<String, String> = BTreeMap::new(); // norm key -> deployed rel
    for id in pkgs.sorted_ids() {
        for (_, files) in version_files(pkgs, lock, id, game) {
            for (deployed, _) in &files {
                needed.insert(norm_key(deployed), deployed.clone());
            }
        }
    }
    let mut m = BTreeMap::new();
    for (key, deployed) in needed {
        if let Ok(sha) = crate::utils::fs::sha256_file(&game_dir.join(&deployed)) {
            m.insert(key, sha);
        }
    }
    m
}

/// Classify one package from the scanned tree.
///
/// `tree` maps lowercase backslash paths to digests. Presence is
/// path-anchored: a file counts for a release when it sits at the
/// release's locked path with the release's bytes. `.ini` config paths
/// are ignored - users edit them freely.
///
/// Returns `None` when the package has no evidence and was never installed
/// by chef (those packages never appear in the summary).
pub(crate) fn summarize_package(
    tree: &BTreeMap<String, String>,
    pkgs: &PackagesFile,
    lock: &LockFile,
    id: &str,
    game: Option<&str>,
    installed: Option<&str>,
    managed: bool,
) -> Option<PackageSummary> {
    let name = pkgs
        .pkg(id)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| id.to_string());
    let versions = version_files(pkgs, lock, id, game);
    if versions.is_empty() {
        return None;
    }

    // Every locked payload path of the package, with the (version, digest)
    // pairs that ship it.
    let mut paths: BTreeMap<String, (String, Vec<(String, String)>)> = BTreeMap::new();
    for (ver, files) in &versions {
        for (deployed, digest) in files {
            paths
                .entry(norm_key(deployed))
                .or_insert_with(|| (display_path(deployed), Vec::new()))
                .1
                .push((ver.clone(), digest.clone()));
        }
    }

    let mut found: BTreeSet<String> = BTreeSet::new();
    let mut unknown = false;
    let cmd = command_ref(pkgs, id);
    for (key, (display, expected)) in paths {
        if is_ignored_config(&display) {
            continue;
        }
        let Some(sha) = tree.get(&key) else {
            continue; // absent: it stands in for no release
        };
        let mut matched: Vec<&str> = expected
            .iter()
            .filter(|(_, digest)| digest == sha)
            .map(|(v, _)| v.as_str())
            .collect();
        matched.dedup();
        let newest = attribute(installed, &matched);
        if newest.is_empty() {
            // The file is there but matches no known release: a custom
            // build or an edited file. The version cannot be stated.
            unknown = true;
        } else {
            found.insert(newest.to_string());
        }
    }

    if unknown {
        let note = format!("run 'chef which {cmd}' for more details");
        let mut versions: Vec<String> = found.into_iter().collect();
        versions.sort_by(|a, b| version_cmp_desc(a, b));
        return Some(PackageSummary {
            id: id.to_string(),
            name,
            managed,
            status: "unknown",
            version: "unknown".to_string(),
            versions,
            notes: note,
        });
    }

    let mut found_vec: Vec<String> = found.into_iter().collect();
    found_vec.sort_by(|a, b| version_cmp_desc(a, b));

    if found_vec.is_empty() {
        // Nothing of any release is present. Only a chef-managed package
        // still shows, pointing at the restore path; unmanaged packages
        // stay invisible.
        let note = format!("run 'chef remove {cmd}' to restore the backup");
        return managed.then(|| PackageSummary {
            id: id.to_string(),
            name,
            managed,
            status: "not-found",
            version: "not found".to_string(),
            versions: Vec::new(),
            notes: note,
        });
    }

    if found_vec.len() > 1 {
        let note = format!("run 'chef which {cmd}' for more details");
        return Some(PackageSummary {
            id: id.to_string(),
            name,
            managed,
            status: "multiple",
            version: "multiple".to_string(),
            versions: found_vec,
            notes: note,
        });
    }

    Some(PackageSummary {
        id: id.to_string(),
        name,
        managed,
        status: "installed",
        version: packages::list_version(&found_vec[0]).to_string(),
        versions: found_vec,
        notes: String::new(),
    })
}

/// Rows for `chef which <name>` / `<name>@<version>`: every locked path of
/// the package that is on disk, labeled with the release its bytes match
/// (`unknown` for a custom build; the installed version wins over the
/// newest), plus the absent paths of the anchor release - the newest
/// release found - labeled `missing`. Absent paths that only older
/// releases ship are not reported: a newer release has dropped them, so
/// they stand in for no release and would only confuse. `only` restricts
/// the walk to one release's paths (its absent ones are still reported).
pub(crate) fn detail_rows(
    pkgs: &PackagesFile,
    lock: &LockFile,
    id: &str,
    game: Option<&str>,
    installed: Option<&str>,
    only: Option<&str>,
    game_dir: &Path,
) -> Vec<(String, String)> {
    // lower rel key -> (deployed path, [(version, expected digest)])
    let mut paths: BTreeMap<String, (String, Vec<(String, String)>)> = BTreeMap::new();
    for (ver, files) in version_files(pkgs, lock, id, game) {
        if let Some(only) = only
            && packages::parse_version_loose(&ver) != packages::parse_version_loose(only)
        {
            continue;
        }
        for (deployed, digest) in files {
            paths
                .entry(norm_key(&deployed))
                .or_insert_with(|| (display_path(&deployed), Vec::new()))
                .1
                .push((ver.clone(), digest));
        }
    }

    // Label every locked path; the anchor is the newest release that has
    // a file on disk. Present leftovers of older releases keep their
    // labels - they are real files.
    let mut rows: Vec<(String, String)> = Vec::new();
    let mut absent: Vec<(String, Vec<String>)> = Vec::new();
    let mut anchor: Option<String> = None;
    for (display, expected) in paths.into_values() {
        match crate::utils::fs::sha256_file(&game_dir.join(&display)) {
            Err(_) => absent.push((display, expected.into_iter().map(|(v, _)| v).collect())),
            Ok(sha) => {
                let mut matched: Vec<&str> = expected
                    .iter()
                    .filter(|(_, digest)| digest == &sha)
                    .map(|(v, _)| v.as_str())
                    .collect();
                matched.dedup();
                match attribute(installed, &matched) {
                    "" => rows.push((display, "unknown".to_string())),
                    v => {
                        if anchor
                            .as_deref()
                            .is_none_or(|a| version_cmp_desc(v, a).is_lt())
                        {
                            anchor = Some(v.to_string());
                        }
                        rows.push((display, packages::list_version(v).to_string()));
                    }
                }
            }
        }
    }

    // Absent paths count as missing only when the anchor release ships
    // them, or when the user explicitly asked about one release. Files a
    // newer release no longer uses are not sought.
    for (display, versions) in absent {
        let report = match only {
            Some(_) => true,
            None => anchor.as_ref().is_some_and(|a| versions.contains(a)),
        };
        if report {
            rows.push((display, "missing".to_string()));
        }
    }
    rows.sort();
    rows
}

/// Summary of every package for the detected game, split into rows chef
/// installed and rows the user installed by hand. Packages with no locked
/// release for this game, no evidence and no chef record are left out.
fn build_rows(
    by_path: &BTreeMap<String, String>,
    pkgs: &PackagesFile,
    lock: &LockFile,
    game: Option<&str>,
    state: &StateFile,
    key: &str,
) -> (Vec<PackageSummary>, Vec<PackageSummary>) {
    let mut chef: Vec<PackageSummary> = Vec::new();
    let mut user: Vec<PackageSummary> = Vec::new();
    for id in pkgs.sorted_ids() {
        if version_files(pkgs, lock, id, game).is_empty() {
            continue;
        }
        let installed = state.install_of(key, id).map(|i| i.version.clone());
        let managed = installed.is_some();
        if let Some(row) =
            summarize_package(by_path, pkgs, lock, id, game, installed.as_deref(), managed)
        {
            if row.managed {
                chef.push(row);
            } else {
                user.push(row);
            }
        }
    }
    (chef, user)
}

fn print_rows(header: &str, rows: &[PackageSummary]) {
    println!("{header}");
    if rows.is_empty() {
        println!("(none)");
        return;
    }
    let name_w = rows
        .iter()
        .map(|r| r.name.chars().count())
        .max()
        .unwrap_or(0)
        .min(26);
    let ver_w = rows
        .iter()
        .map(|r| r.version.chars().count())
        .max()
        .unwrap_or(0)
        .min(40);
    for r in rows {
        println!(
            "{:<name_w$}  {:<ver_w$}  {}",
            truncate(&r.name, 26),
            r.version,
            r.notes
        );
    }
}

/// 14235245 -> "14,235,245"
fn format_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;

    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        format!("{}...", s.chars().take(n - 1).collect::<String>())
    } else {
        s.to_string()
    }
}

fn print_header(game_dir: &Path, game_detected: &Option<game_dir::DetectedGame>) {
    println!("game dir: {}", game_dir.display());
    if let Some(g) = game_detected {
        match std::fs::metadata(game_dir.join(&g.exe)) {
            Ok(m) => println!(
                "game: {} ({}, {})",
                crate::game_dir::family_title(&g.family),
                g.exe,
                format_bytes(m.len())
            ),
            Err(_) => println!(
                "game: {} ({})",
                crate::game_dir::family_title(&g.family),
                g.exe
            ),
        }
    }
}

pub fn run(pkg: Option<&str>, dir: Option<PathBuf>, json: bool) -> crate::Result<()> {
    let game_dir = game_dir::resolve_game_dir(dir.as_deref()).map_err(ChefError::Other)?;
    let (pkgs, lock) = packages::load_metadata(false).map_err(ChefError::Other)?;

    let game_detected = game_dir::detect_game(&pkgs, &game_dir).map_err(ChefError::Other)?;
    let game = game_detected.as_ref().map(|g| g.family.clone());

    // Both modes below read only paths recorded in the lock: the summary
    // hashes expected payload paths, the details view the target package's
    // locked paths. The game tree is never walked.
    match pkg {
        None => {
            let state = StateFile::load().map_err(ChefError::Other)?;
            let key = game_dir::dir_hash_key(&game_dir);
            let by_path = expected_tree(&pkgs, &lock, game.as_deref(), &game_dir);
            let (chef, user) = build_rows(&by_path, &pkgs, &lock, game.as_deref(), &state, &key);

            if json {
                let installs = chef
                    .iter()
                    .chain(&user)
                    .map(|r| {
                        json!({
                            "id": r.id,
                            "name": r.name,
                            "managed": r.managed,
                            "section": if r.managed { "chef" } else { "user" },
                            "status": r.status,
                            "version": r.version,
                            "versions": r.versions,
                            "notes": r.notes,
                        })
                    })
                    .collect::<Vec<_>>();
                crate::emit_json(&json!({
                    "dir": game_dir.to_string_lossy(),
                    "game": game.as_ref().map(|g| crate::game_dir::family_title(g).to_string()),
                    "installs": installs,
                }));
                return Ok(());
            }

            print_header(&game_dir, &game_detected);
            println!();
            print_rows("PACKAGES INSTALLED BY CHEF", &chef);
            println!();
            print_rows("PACKAGES INSTALLED BY USER", &user);
            crate::commands::upgrade::update_notice();
            Ok(())
        }
        Some(arg) => {
            let (name, spec) = split_pkg_spec(arg);
            let id = match_names::resolve_id(&pkgs, name, game.as_deref())?;
            let only = match spec {
                Some(s) => Some(
                    packages::resolve_spec(&pkgs, &lock, &id, game.as_deref(), Some(s))
                        .map_err(ChefError::Other)?
                        .version,
                ),
                None => None,
            };
            let state = StateFile::load().map_err(ChefError::Other)?;
            let installed = state
                .install_of(&game_dir::dir_hash_key(&game_dir), &id)
                .map(|i| i.version.clone());
            let rows = detail_rows(
                &pkgs,
                &lock,
                &id,
                game.as_deref(),
                installed.as_deref(),
                only.as_deref(),
                &game_dir,
            );

            if json {
                crate::emit_json(&json!({
                    "id": id,
                    "name": pkgs.pkg(&id).map(|p| p.name.clone()).unwrap_or(id.clone()),
                    "dir": game_dir.to_string_lossy(),
                    "game": game.as_ref().map(|g| crate::game_dir::family_title(g).to_string()),
                    "files": rows.iter().map(|(p, v)| json!({
                        "path": p,
                        "version": v,
                    })).collect::<Vec<_>>(),
                }));
                return Ok(());
            }

            let name_title = pkgs
                .pkg(&id)
                .map(|p| p.name.clone())
                .unwrap_or_else(|| id.clone());
            if rows.is_empty() {
                println!("no files from {name_title} found in {}", game_dir.display());
            } else {
                let w = rows
                    .iter()
                    .map(|(p, _)| p.chars().count())
                    .max()
                    .unwrap_or(0);
                for (path, label) in &rows {
                    println!("{:<w$}  {}", path, label);
                }
            }
            crate::commands::upgrade::update_notice();
            Ok(())
        }
    }
}
