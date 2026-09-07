use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::bail;
use log::{info, warn};

use crate::ChefError;
use crate::game_dir::{self, DeployFile, DeployRequest};
use crate::packages::download::{complete_marker, store_version_dir};
use crate::packages::{self, LockFile, PackagesFile, ResolvedVersion, extract_zip, fetch_asset};
use crate::utils::fs::write_atomic;
use crate::utils::term;

/// True when `product`@`version` is installed in `game_dir` at exactly this
/// version with every managed file present and digest-intact.
fn already_installed(product: &ResolvedVersion, game_dir: &Path) -> bool {
    let Ok(state) = crate::game_dir::StateFile::load() else {
        return false;
    };
    let key = game_dir::dir_hash_key(game_dir);
    let Some(inst) = state.install_of(&key, &product.id) else {
        return false;
    };
    if inst.version != product.version {
        return false;
    }
    let same = inst.files.iter().all(|f| {
        crate::utils::fs::sha256_file(&game_dir.join(&f.path))
            .ok()
            .as_deref()
            == Some(f.sha256.as_str())
    });
    if same {
        #[cfg(test)]
        crate::emit::dbg_trace(format_args!(
            "already_installed TRUE id={} version={} game={}",
            product.id,
            product.version,
            game_dir.display()
        ));
    }
    same
}

/// One file operation of a dry run, shown to humans and in `--json`.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct PlanStep {
    /// Destination path relative to the game root.
    pub path: String,
    /// "add" | "replace" | "backup" | "remove" | "keep"
    pub op: &'static str,
    pub note: String,
}

/// Classify a dry run of `product` into per-file operations. `files` is the
/// extracted payload when it is cached (enabling byte-exact `keep`
/// detection); without it the plan falls back to existence and ownership.
pub(crate) fn plan_deploy(
    product: &ResolvedVersion,
    game_dir: &Path,
    files: &[DeployFile],
) -> anyhow::Result<Vec<PlanStep>> {
    use crate::game_dir::StateFile;

    let mut expected: BTreeMap<String, String> = BTreeMap::new(); // low path -> payload sha
    for f in files {
        expected.insert(f.dest_rel.to_lowercase(), f.sha256.clone());
    }

    // Every slot occupant and its managed paths (lowercase -> original path).
    let state = StateFile::load()?;
    let key = game_dir::dir_hash_key(game_dir);
    let mut managed: BTreeMap<String, (String, String)> = BTreeMap::new();
    for sid in &product.slot {
        if let Some(inst) = state.install_of(&key, sid) {
            for mf in &inst.files {
                managed
                    .entry(mf.path.to_lowercase())
                    .or_insert_with(|| (mf.path.clone(), mf.sha256.clone()));
            }
        }
    }

    let new_paths: Vec<String> = product
        .payload
        .iter()
        .map(|(d, _)| d.to_lowercase())
        .collect();
    let mut steps: Vec<PlanStep> = Vec::new();

    for (deployed, _) in &product.payload {
        let low = deployed.to_lowercase();
        let abs = game_dir.join(deployed);
        let current = crate::utils::fs::sha256_file(&abs).ok();
        let (op, note) = if !abs.exists() {
            ("add", String::new())
        } else if expected
            .get(&low)
            .is_some_and(|e| current.as_deref() == Some(e.as_str()))
        {
            ("keep", "already installed".to_string())
        } else if let Some((orig, recorded)) = managed.get(&low) {
            if current.as_deref() != Some(recorded.as_str()) {
                ("replace", format!("overwrites your modified {orig}"))
            } else {
                ("replace", "replaces the previous install".to_string())
            }
        } else {
            (
                "backup",
                "user file backed up, restored on 'chef remove'".to_string(),
            )
        };
        steps.push(PlanStep {
            path: deployed.clone(),
            op,
            note,
        });
    }

    // Files the new version no longer ships.
    for (low, (orig, recorded)) in &managed {
        if new_paths.iter().any(|p| p == low) {
            continue;
        }
        let current = crate::utils::fs::sha256_file(&game_dir.join(orig)).ok();
        if current.as_deref() == Some(recorded.as_str()) {
            steps.push(PlanStep {
                path: orig.clone(),
                op: "remove",
                note: format!("no longer shipped by {}", product.version),
            });
        } else {
            steps.push(PlanStep {
                path: orig.clone(),
                op: "keep",
                note: "no longer shipped; your modified copy stays".to_string(),
            });
        }
    }

    let rank = |op: &str| match op {
        "add" => 0,
        "replace" => 1,
        "backup" => 2,
        "remove" => 3,
        _ => 4,
    };
    steps.sort_by(|a, b| {
        rank(a.op)
            .cmp(&rank(b.op))
            .then_with(|| a.path.cmp(&b.path))
    });
    Ok(steps)
}

fn print_plan(product: &ResolvedVersion, steps: &[PlanStep]) {
    println!("{} {}", product.name, product.version);
    let w = steps
        .iter()
        .map(|s| s.path.chars().count())
        .max()
        .unwrap_or(0)
        .min(52);
    let mut last: &str = "";
    for s in steps {
        if s.op != last {
            println!("  {}:", s.op);
            last = s.op;
        }
        let path = if s.path.chars().count() > w {
            format!(
                "{}...",
                s.path.chars().take(w.saturating_sub(1)).collect::<String>()
            )
        } else {
            s.path.clone()
        };
        if s.note.is_empty() {
            println!("    {path}");
        } else {
            println!("    {:<w$}  {}", path, s.note);
        }
    }
}

pub(crate) fn install_one(
    product: &ResolvedVersion,
    game_dir: &Path,
    dry_run: bool,
) -> crate::Result<Vec<PlanStep>> {
    if dry_run {
        // Plan against the extracted payload when it is cached; never
        // download on a dry run.
        let files = if store_complete(product.store_key(), &product.version) {
            let vdir = ensure_payload(
                product.store_key(),
                &product.name,
                &product.version,
                &product.url,
                &product.asset_sha256,
                true,
            )
            .map_err(|e| ChefError::Other(e.context("preparing payload")))?;
            let specs: Vec<DeploySpec> = product
                .payload
                .iter()
                .map(|(deployed, entry)| DeploySpec {
                    deployed: deployed.clone(),
                    entry: entry.clone(),
                })
                .collect();
            collect_deployment(&vdir, &specs).map_err(ChefError::Other)?
        } else {
            Vec::new()
        };
        let steps = plan_deploy(product, game_dir, &files).map_err(ChefError::Other)?;
        print_plan(product, &steps);
        return Ok(steps);
    }

    let vdir = ensure_payload(
        product.store_key(),
        &product.name,
        &product.version,
        &product.url,
        &product.asset_sha256,
        false,
    )
    .map_err(|e| {
        ChefError::Other(e.context(format!(
            "preparing payload for {} {}",
            product.name, product.version
        )))
    })?;
    let specs: Vec<DeploySpec> = product
        .payload
        .iter()
        .map(|(deployed, entry)| DeploySpec {
            deployed: deployed.clone(),
            entry: entry.clone(),
        })
        .collect();
    let files: Vec<DeployFile> = collect_deployment(&vdir, &specs).map_err(ChefError::Other)?;

    let _lock = game_dir::Lock::acquire(game_dir).map_err(ChefError::Other)?;
    let outcome = game_dir::deploy(
        game_dir,
        DeployRequest {
            product,
            slot: &product.slot,
            version: &product.version,
            files: &files,
            dry_run,
        },
    )
    .map_err(ChefError::Other)?;
    drop(_lock);

    if outcome.replaced.is_some() {
        info!("replacing previous installation");
    }

    #[cfg(test)]
    {
        let st = crate::game_dir::StateFile::load();
        let recorded = st
            .as_ref()
            .map(|s| {
                s.install_of(&crate::game_dir::dir_hash_key(game_dir), &product.id)
                    .is_some()
            })
            .unwrap_or(false);
        let dirs = st
            .as_ref()
            .map(|s| s.dirs.keys().cloned().collect::<Vec<_>>().join(","))
            .unwrap_or_default();
        crate::emit::dbg_trace(format_args!(
            "install_one OK id={} version={} game={} home={} state_dirs=[{}] recorded={recorded}",
            product.id,
            product.version,
            game_dir.display(),
            crate::packages::chef_home().display(),
            dirs
        ));
    }
    Ok(Vec::new())
}

fn store_complete(key: &str, version: &str) -> bool {
    crate::packages::store_root()
        .join(key)
        .join(version)
        .join(".complete")
        .exists()
}

/// Ensure an extracted, verified payload exists in the store and return its
/// directory. The archive is selected by URL and verified against the lock
/// digest before extraction.
pub(crate) fn ensure_payload(
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

/// Canonical key for one dependency requirement: the sorted member ids, so
/// the same group declared twice (or in another order) is one requirement.
pub(crate) fn dependency_key(group: &packages::DependencyGroup) -> String {
    let mut ids = group.any.clone();
    ids.sort();
    ids.join(",")
}

/// Display titles for member ids (falls back to the id itself).
fn member_titles(pkgs: &PackagesFile, ids: &[String]) -> Vec<String> {
    ids.iter()
        .map(|id| {
            pkgs.pkg(id)
                .map(|p| p.name.clone())
                .unwrap_or_else(|| id.clone())
        })
        .collect()
}

/// Members of `group` offered to the user: deduped, in declaration order,
/// and - when a game is detected - restricted to ids with at least one
/// version record installable for that game (an ASI loader that only
/// covers gta-sa is never offered inside GTA III).
pub(crate) fn offered_members(
    pkgs: &PackagesFile,
    lock: &LockFile,
    game: Option<&str>,
    group: &packages::DependencyGroup,
) -> Vec<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    group
        .any
        .iter()
        .filter(|id| {
            if !seen.insert((*id).clone()) {
                return false;
            }
            match game {
                Some(g) => !packages::available_versions(pkgs, lock, id, Some(g)).is_empty(),
                None => true,
            }
        })
        .cloned()
        .collect()
}

/// Is the dependency group satisfied: any member planned in this run
/// (requested or already picked), installed by chef in this game dir, or
/// present on disk as a known payload file (e.g. an ASI loader the user
/// installed by hand)? `members` carries the full declared member list - a
/// member planned/installed under a sibling requirement satisfies it too.
pub(crate) fn dependency_satisfied(
    members: &[String],
    planned: &BTreeSet<String>,
    pkgs: &PackagesFile,
    lock: &LockFile,
    game: Option<&str>,
    game_dir: &Path,
) -> bool {
    if members.iter().any(|m| planned.contains(m)) {
        return true;
    }
    if let Ok(state) = crate::game_dir::StateFile::load() {
        let key = game_dir::dir_hash_key(game_dir);
        if members.iter().any(|m| state.install_of(&key, m).is_some()) {
            return true;
        }
    }
    let known = packages::payload_basenames(pkgs, lock, game);
    let Ok(entries) = std::fs::read_dir(game_dir) else {
        return false;
    };
    for e in entries.flatten() {
        if !e.path().is_file() {
            continue;
        }
        let name = e.file_name().to_string_lossy().to_lowercase();
        if let Some(ids) = known.get(&name)
            && ids.iter().any(|i| members.iter().any(|m| m == i))
        {
            return true;
        }
    }
    false
}

/// Install (or dry-run plan) every package in `specs` against `game_dir`.
/// `specs` are already-split `name@spec` arguments; name/version resolution,
/// dependency offers and the deployment itself all happen here.
pub fn run(
    specs: &[(&str, Option<&str>)],
    game_dir: &Path,
    dry_run: bool,
    json: bool,
) -> crate::Result<()> {
    let (pkgs, lock) = packages::load_metadata(false).map_err(ChefError::Other)?;

    // Detect the game once; drives which version records apply (games lists) - including UAL's per-game dll rename.
    let detected = game_dir::detect_game(&pkgs, game_dir).map_err(ChefError::Other)?;
    let game = detected.as_ref().map(|d| d.family.clone());

    // Build an ordered install plan from every argument.
    struct Planned {
        product: ResolvedVersion,
    }
    let mut plan: Vec<Planned> = Vec::new();
    for (name, spec) in specs {
        let id = packages::resolve_id(&pkgs, name, game.as_deref())?;

        let res = packages::resolve_spec(&pkgs, &lock, &id, game.as_deref(), *spec)
            .map_err(ChefError::Other)?;
        plan.push(Planned { product: res });
    }

    // Deduplicate repeated requests: the same module id listed twice (or
    // under several specs) is installed once - the first spec wins.
    let mut seen_ids: BTreeSet<String> = BTreeSet::new();
    plan.retain(|p| {
        if seen_ids.insert(p.product.id.clone()) {
            true
        } else {
            info!("{} is requested twice - installing once", p.product.name);
            false
        }
    });

    // Dependencies: every requested version may require other packages
    // (e.g. classic CLEO needs an ASI loader). Each `dependencies` entry
    // is an any-of group - one member suffices. A group is satisfied when
    // a member is planned in this run (requested or already picked),
    // already managed by chef, or otherwise present on disk. Missing ->
    // offer the group's installable members once; the picked package also
    // satisfies sibling groups sharing a member, so nothing is offered
    // twice. `planned_ids` grows as picks land, keeping every earlier
    // decision visible to later requirements.
    let mut planned_ids: BTreeSet<String> = plan.iter().map(|p| p.product.id.clone()).collect();
    let mut handled: Vec<String> = Vec::new();
    for i in 0..plan.len() {
        let p_title = plan[i].product.name.clone();
        let deps = plan[i].product.dependencies.clone();
        for group in deps {
            let key = dependency_key(&group);
            if handled.contains(&key)
                || dependency_satisfied(
                    &group.any,
                    &planned_ids,
                    &pkgs,
                    &lock,
                    game.as_deref(),
                    game_dir,
                )
            {
                continue;
            }
            handled.push(key);
            let members = offered_members(&pkgs, &lock, game.as_deref(), &group);
            if members.is_empty() {
                // No member is installable for this game - nothing to
                // offer; report the requirement the same way the warn
                // branch does.
                let titles = member_titles(&pkgs, &group.any).join(", ");
                if dry_run {
                    info!(
                        "note: {} needs one of: {titles} - none found (would offer to install)",
                        p_title
                    );
                } else {
                    warn!(
                        "{} needs one of: {titles} but none is installed - 'chef add {}' adds it",
                        p_title,
                        group.any.join("' or 'chef add ")
                    );
                }
                continue;
            }
            let titles = member_titles(&pkgs, &members);
            let need = format!("{} needs one of: {}", p_title, titles.join(", "));
            if dry_run {
                info!("note: {need} - none found (would offer to install)");
                continue;
            }
            if term::interactive()
                && let Some(chosen) = term::pick(&format!("{need} - set up one?"), &titles)
            {
                let dep_id = &members[chosen];
                if planned_ids.contains(dep_id) {
                    continue;
                }
                let dres = packages::resolve_spec(&pkgs, &lock, dep_id, game.as_deref(), None)
                    .map_err(ChefError::Other)?;
                planned_ids.insert(dep_id.clone());
                plan.push(Planned { product: dres });
            } else {
                warn!(
                    "{need} but none is installed - 'chef add {}' adds it",
                    members.join("' or 'chef add ")
                );
            }
        }
    }

    // Warn when the plan lines up two same-slot packages: they cannot
    // coexist, and the later install replaces the earlier one.
    for i in 0..plan.len() {
        for j in (i + 1)..plan.len() {
            let a = &plan[i].product;
            let b = &plan[j].product;
            if a.id == b.id {
                continue;
            }
            let sa = packages::existent_slot(&pkgs, &a.id);
            let sb = packages::existent_slot(&pkgs, &b.id);
            if sa.iter().any(|s| sb.iter().any(|t| t == s)) {
                warn!(
                    "{} and {} occupy the same slot - installing the later replaces the earlier",
                    a.name, b.name
                );
            }
        }
    }

    // Execute the plan in argument order, recording every outcome for
    // the JSON result document.
    #[derive(serde::Serialize)]
    struct Row {
        id: String,
        name: String,
        version: String,
        status: &'static str,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        plan: Vec<PlanStep>,
    }
    let mut rows: Vec<Row> = Vec::new();
    for p in &plan {
        // Exact-version no-op: same version already installed and all
        // managed files intact - say so and move on.
        if already_installed(&p.product, game_dir) {
            info!(
                "{} {} is already installed - nothing to do",
                p.product.name, p.product.version
            );
            rows.push(Row {
                id: p.product.id.clone(),
                name: p.product.name.clone(),
                version: p.product.version.clone(),
                status: "already",
                plan: Vec::new(),
            });
            continue;
        }
        if dry_run {
            let steps = install_one(&p.product, game_dir, true)?;
            rows.push(Row {
                id: p.product.id.clone(),
                name: p.product.name.clone(),
                version: p.product.version.clone(),
                status: "would-install",
                plan: steps,
            });
            continue;
        }
        info!(
            "installing {}{}...",
            p.product.name,
            packages::version_word(&p.product.version)
        );
        install_one(&p.product, game_dir, false)?;
        info!(
            "{}{} installed",
            p.product.name,
            packages::version_word(&p.product.version)
        );
        rows.push(Row {
            id: p.product.id.clone(),
            name: p.product.name.clone(),
            version: p.product.version.clone(),
            status: "installed",
            plan: Vec::new(),
        });
    }

    if json {
        crate::emit::emit_json(&serde_json::json!({ "add": rows }));
        return Ok(());
    }
    crate::handlers::upgrade::update_notice();
    Ok(())
}

// ---------------------------------------------------------------------------
// Deployment plan: payload specs + store archive -> staged DeployFiles.
// ---------------------------------------------------------------------------

/// One payload entry staged for deployment: the deployed relative path
/// and the archive entry it comes from (identical unless a postinstall
/// rename applies, e.g. UAL ships dinput8.dll that SA must see as
/// vorbisFile.dll).
#[derive(Debug, Clone)]
struct DeploySpec {
    /// Destination path relative to the game root (`/`-separated), after
    /// postinstall renames.
    pub deployed: String,
    /// Archive entry path to locate inside the store payload.
    pub entry: String,
}

/// Recursively locate a payload file by lowercase basename inside `store_dir`.
/// Shallowest match wins (archives sometimes nest payloads in folders).
fn locate_payload_file(store_dir: &Path, wanted: &str) -> Option<PathBuf> {
    let wanted = wanted.to_lowercase();
    let mut best: Option<PathBuf> = None;
    let mut stack = vec![store_dir.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name()?.to_string_lossy().to_lowercase() == wanted {
                let depth = p.components().count();
                if best
                    .as_ref()
                    .map(|b| b.components().count())
                    .unwrap_or(usize::MAX)
                    > depth
                {
                    best = Some(p);
                }
            }
        }
    }
    best
}

/// Build the full deployment plan for a package from its store payload:
/// resolve every lock file (excludes already applied) against the
/// extracted archive and compute digests. All payloads deploy to the game
/// root; the first spec is required, extras deploy when found.
fn collect_deployment(store_dir: &Path, specs: &[DeploySpec]) -> anyhow::Result<Vec<DeployFile>> {
    if specs.is_empty() {
        bail!("package declares no payload files");
    }

    let mut out = Vec::new();

    for (i, spec) in specs.iter().enumerate() {
        // Lock entries are archive-relative paths; the payload locator
        // matches by basename (archives nest payloads in folders).
        let entry_base = spec.entry.rsplit('/').next().unwrap_or(&spec.entry);
        let Some(src) = locate_payload_file(store_dir, entry_base) else {
            if i == 0 {
                bail!(
                    "required payload file {:?} not found in the release archive",
                    spec.entry
                );
            }
            continue;
        };

        let dest_rel = normalize_dest_name(&spec.deployed);
        let digest = crate::utils::fs::sha256_file(&src)?;
        out.push(DeployFile {
            dest_rel,
            src,
            sha256: digest,
        });
    }
    Ok(out)
}

/// Destination name: keep the canonical case as declared by the manifest.
fn normalize_dest_name(want: &str) -> String {
    want.replace('\\', "/")
}
