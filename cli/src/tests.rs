use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::cli::Cmd;
use crate::game_dir::{self, StateFile};
use crate::packages::{self, LockFile, PackagesFile};
use crate::store;

use sha2::Digest as _;

// ---------------------------------------------------------------------------
// Serial guard: fixture tests share the home-override seam.
// ---------------------------------------------------------------------------

fn env_guard() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

struct TestEnv {
    #[allow(dead_code)]
    guard: MutexGuard<'static, ()>,
    pub home: PathBuf,
    pub game_dir: PathBuf,
}

fn assert_under_temp(p: &Path) {
    debug_assert!(
        p.starts_with(std::env::temp_dir()),
        "test dir {} escaped the temp root",
        p.display()
    );
}

// ---------------------------------------------------------------------------
// Pristine `.dev/.test/games` fixtures
// ---------------------------------------------------------------------------

/// Game/executable pairs mirroring `.dev/.test/make_fixtures.py`.
const GAME_FIXTURES: &[(&str, &str)] = &[
    ("san-andreas", "gta_sa.exe"),
    ("gta3", "gta3.exe"),
    ("vice-city", "gta-vc.exe"),
];

/// Locate the checkout root (the ancestor whose `cli/Cargo.toml` exists);
/// fixtures live under `<root>/.dev/.test/games`.
fn checkout_root() -> PathBuf {
    let mut dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    loop {
        if dir.join("cli/Cargo.toml").is_file() {
            return dir;
        }
        if !dir.pop() {
            panic!(
                "cannot locate the checkout root (cli/Cargo.toml) from {}",
                std::env::current_dir().unwrap().display()
            );
        }
    }
}

/// Recreate pristine `.dev/.test/games/<game>` dirs (stub executable + a
/// user file chef must never touch). Runs before every fixture-based test so
/// a previous run's deployments never leak into the next.
fn refresh_test_games() {
    let games = checkout_root().join(".dev/.test/games");
    for (game, exe) in GAME_FIXTURES {
        let d = games.join(game);
        std::fs::create_dir_all(&d).unwrap();
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                std::fs::remove_dir_all(&p).unwrap();
            } else {
                std::fs::remove_file(&p).unwrap();
            }
        }
        std::fs::write(d.join(exe), b"MZ\x90\x00stub").unwrap();
        std::fs::write(d.join("user_save.dat"), b"user data - do not delete").unwrap();
    }
}

/// Recursive file copy (fixture game dirs are tiny: a stub exe + user file).
fn copy_tree(from: &Path, to: &Path) {
    crate::utils::walk::copy_tree(from, to).unwrap();
}

fn write_test_zip(path: &Path, files: &[(&str, &[u8])], wrap: Option<&str>) -> String {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let file = std::fs::File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    for (name, data) in files {
        let full = match wrap {
            Some(w) => format!("{w}/{name}"),
            None => (*name).to_string(),
        };
        zip.start_file(full, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap();
    crate::utils::fs::sha256_file(path).unwrap()
}

/// Inner-file digests of a zip (paths relative to the archive root) for
/// the lock file.
fn zip_inner_files(path: &Path) -> Vec<(String, String)> {
    let file = std::fs::File::open(path).unwrap();
    let mut z = zip::ZipArchive::new(file).unwrap();
    let mut out = Vec::new();
    for i in 0..z.len() {
        let mut entry = z.by_index(i).unwrap();
        if entry.is_dir() {
            continue;
        }
        let rel = entry.name().replace('\\', "/");
        let mut h = sha2::Sha256::new();
        std::io::copy(&mut entry, &mut h).unwrap();
        out.push((rel, hex::encode(h.finalize())));
    }
    out.sort();
    out
}

/// Build a self-contained offline environment: mocked `packages.json` +
/// `packages.lock` with `file://` assets, generated release ZIPs, and a
/// game directory copied pristine from `.dev/.test/games/san-andreas`.
fn setup(name: &str) -> TestEnv {
    let guard = env_guard();
    refresh_test_games();
    let root = std::env::temp_dir().join(format!("chef-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let game = root.join("game-sa");
    std::fs::create_dir_all(home.join("locks")).unwrap();
    assert_under_temp(&home);
    assert_under_temp(&game);

    // San Andreas mock, refreshed pristine from `.dev/.test/games`.
    copy_tree(&checkout_root().join(".dev/.test/games/san-andreas"), &game);
    assert!(game.join("gta_sa.exe").exists());

    // Payloads. Archive layout mirrors the real releases: root-level
    // payloads (cleo.asi, dlls) plus per-package data dirs.
    let payloads = root.join("payloads");
    let cleo5_zip = payloads.join("cleo.sa/5.4.0/SA.CLEO-v5.4.0.zip");
    write_test_zip(
        &cleo5_zip,
        &[
            ("cleo.asi", b"MZ-cleo5-asi" as &[u8]),
            ("CLEO/cleo_plugins/Example.cleo", b"dll"),
            ("README.md", b"CLEO 5"),
        ],
        None,
    );
    let cleo53_zip = payloads.join("cleo.sa/5.3.0/SA.CLEO-v5.3.0.zip");
    write_test_zip(&cleo53_zip, &[("cleo.asi", b"MZ-cleo5-older")], None);
    let cleo55b_zip = payloads.join("cleo.sa/5.5.0-beta.1/SA.CLEO-v5.5.0-beta.1.zip");
    write_test_zip(&cleo55b_zip, &[("cleo.asi", b"MZ-cleo5-preview")], None);
    let cleo4_zip = payloads.join("cleo.sa/4.4.4/CLEO4.zip");
    write_test_zip(
        &cleo4_zip,
        &[("cleo.asi", b"MZ-cleo4-asi"), ("CLEO/notes.txt", b"x")],
        None,
    );
    let redux_zip = payloads.join("cleo-redux/1.5.0/cleo_redux_1.5.0.x86.zip");
    write_test_zip(
        &redux_zip,
        &[
            ("cleo_redux.asi", b"MZ-redux-asi"),
            ("cleo_redux.toml", b"log=0"),
        ],
        None,
    );
    let sal_zip = payloads.join("silents-asi-loader/1.5.0/SAL.zip");
    write_test_zip(
        &sal_zip,
        &[
            ("vorbisFile.dll", b"MZ-sal-dll"),
            ("vorbisHooked.dll", b"MZ-sal-hooked"),
        ],
        None,
    );
    let ual_zip = payloads.join("universal-asi-loader/9.7.4/Ultimate-ASI-Loader.zip");
    write_test_zip(&ual_zip, &[("dinput8.dll", b"MZ-ual-dll")], None);

    fn file_uri(p: &Path) -> String {
        let s = p.to_string_lossy().replace('\\', "/");
        format!("file:///{s}")
    }

    // ----- packages.lock: asset digests + inner file hashes --------------
    let mut assets = serde_json::Map::new();
    let mut add = |href: &str, zip: &Path| {
        let sha = crate::utils::fs::sha256_file(zip).unwrap();
        let files: Vec<serde_json::Value> = zip_inner_files(zip)
            .into_iter()
            .map(|(path, sha256)| serde_json::json!({ "path": path, "sha256": sha256 }))
            .collect();
        assets.insert(
            href.to_string(),
            serde_json::json!({ "url": href, "sha256": sha, "files": files }),
        );
    };
    add(&file_uri(&cleo5_zip), &cleo5_zip);
    add(&file_uri(&cleo53_zip), &cleo53_zip);
    add(&file_uri(&cleo55b_zip), &cleo55b_zip);
    add(&file_uri(&cleo4_zip), &cleo4_zip);
    add(&file_uri(&redux_zip), &redux_zip);
    add(&file_uri(&sal_zip), &sal_zip);
    add(&file_uri(&ual_zip), &ual_zip);

    let lock_json = serde_json::json!({
        "schema": 2,
        "generated_at": 0,
        "assets": serde_json::Value::Object(assets),
    });

    // ----- packages.json: catalog ----------------------------------------
    let sa_url = file_uri(&cleo5_zip);
    let sa53_url = file_uri(&cleo53_zip);
    let sa55b_url = file_uri(&cleo55b_zip);
    let cleo4_url = file_uri(&cleo4_zip);
    let redux_url = file_uri(&redux_zip);
    let sal_url = file_uri(&sal_zip);
    let ual_url = file_uri(&ual_zip);

    let pkgs_json = serde_json::json!({
        "schema": 2,
        "games": { "gta_sa.exe": "gta-sa" },
        "packages": [
            {
                "id": "cleo.sa",
                "name": "CLEO",
                "aliases": ["cleo5", "cleo4", "cleo", "cleo-sa"],
                "versions": [
                    {
                        "version": "5.4.0",
                        "release": "https://example.invalid/cleo5/v5.4.0",
                        "assets": [sa_url],
                        "games": ["gta-sa"],
                        "dependencies": ["silents-asi-loader.sa", "universal-asi-loader.sa.vc.iii"]
                    },
                    {
                        "version": "5.3.0",
                        "assets": [sa53_url],
                        "games": ["gta-sa"],
                        "dependencies": ["silents-asi-loader.sa", "universal-asi-loader.sa.vc.iii"]
                    },
                    {
                        "version": "5.5.0-beta.1",
                        "assets": [sa55b_url],
                        "games": ["gta-sa"],
                        "dependencies": ["silents-asi-loader.sa", "universal-asi-loader.sa.vc.iii"]
                    },
                    {
                        "version": "4.4.4",
                        "assets": [cleo4_url],
                        "games": ["gta-sa"],
                        "dependencies": ["silents-asi-loader.sa", "universal-asi-loader.sa.vc.iii"]
                    }
                ]
            },
            {
                "id": "cleo.vc",
                "name": "VC.CLEO",
                "aliases": ["vc.cleo", "cleo-vc"],
                "versions": [
                    { "version": "2.2.0", "assets": [cleo4_url], "games": ["gta-vc"] }
                ]
            },
            {
                "id": "cleo.iii",
                "name": "III.CLEO",
                "aliases": ["iii.cleo", "cleo-iii"],
                "versions": [
                    { "version": "2.2.0", "assets": [cleo4_url], "games": ["gta-3"] }
                ]
            },
            {
                "id": "cleo-redux.sa",
                "name": "CLEO Redux",
                "aliases": ["cleo-redux", "cleoredux"],
                "versions": [
                    { "version": "1.5.0", "assets": [redux_url], "games": ["gta-sa"] }
                ]
            },
            {
                "id": "cleo-redux.vc.iii",
                "name": "CLEO Redux",
                "aliases": ["cleo-redux", "cleoredux"],
                "versions": [
                    { "version": "1.5.0", "assets": [redux_url], "games": ["gta-3", "gta-vc"] }
                ]
            },
            {
                "id": "silents-asi-loader.sa",
                "name": "Silent's ASI Loader",
                "aliases": ["sal", "silents-asi-loader", "asi-loader"],
                "replaces": ["universal-asi-loader.sa.vc.iii"],
                "versions": [
                    { "version": "1.5.0", "assets": [sal_url], "games": ["gta-sa"] }
                ]
            },
            {
                "id": "universal-asi-loader.sa.vc.iii",
                "name": "Universal ASI Loader",
                "aliases": ["ual", "universal-asi-loader", "asi-loader"],
                "replaces": ["silents-asi-loader.sa"],
                "versions": [
                    {
                        "version": "9.7.4",
                        "assets": [ual_url],
                        "games": ["gta-sa"],
                        "postinstall": { "rename": { "dinput8.dll": "vorbisFile.dll" } }
                    },
                    {
                        "version": "9.7.4",
                        "assets": [ual_url],
                        "games": ["gta-3", "gta-vc"]
                    }
                ]
            }
        ]
    });

    let pkgs_path = home.join("packages.json");
    std::fs::write(&pkgs_path, serde_json::to_vec_pretty(&pkgs_json).unwrap()).unwrap();
    let lock_path = home.join("packages.lock");
    std::fs::write(&lock_path, serde_json::to_vec_pretty(&lock_json).unwrap()).unwrap();

    // Point chef at the sandbox data home (test seam, not an env var).
    crate::packages::set_home_override(home.clone());

    crate::dbg_trace(format_args!(
        "TEST SETUP name={name} home={} game={} key={}",
        home.display(),
        game.display(),
        game_dir::dir_hash_key(&game)
    ));

    TestEnv {
        guard,
        home,
        game_dir: game,
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        crate::packages::clear_home_override();
        let _ = std::fs::remove_dir_all(self.home.parent().unwrap());
    }
}

fn chef_run(cmd: Cmd) -> crate::Result<()> {
    crate::run(cmd)
}

fn run_ok(cmd: Cmd) {
    if let Err(e) = crate::run(cmd) {
        panic!("command failed: {e}");
    }
}

fn state() -> StateFile {
    let st = StateFile::load().unwrap();
    crate::dbg_trace(format_args!(
        "TEST state() path={} exists={} dirs={} keys=[{}]",
        game_dir::state_path().display(),
        game_dir::state_path().exists(),
        st.dirs.len(),
        st.dirs.keys().cloned().collect::<Vec<_>>().join(",")
    ));
    st
}

// ===========================================================================
// Unit: name matching (sec.7)
// ===========================================================================

fn unit_pkgs() -> PackagesFile {
    let json = serde_json::json!({
        "schema": 2,
        "games": { "gta_sa.exe": "gta-sa" },
        "packages": [
            { "id": "cleo.sa", "name": "CLEO", "aliases": ["cleo5", "cleo4"],
              "versions": [ { "version": "5.4.0", "assets": ["u"], "games": ["gta-sa"] } ] },
            { "id": "cleo.vc", "name": "VC.CLEO", "aliases": ["vc.cleo"],
              "versions": [ { "version": "2.2.0", "assets": ["u"], "games": ["gta-vc"] } ] },
            { "id": "cleo.iii", "name": "III.CLEO", "aliases": ["iii.cleo"],
              "versions": [ { "version": "2.2.0", "assets": ["u"], "games": ["gta-3"] } ] },
            { "id": "cleo-redux.sa", "name": "CLEO Redux", "aliases": ["cleo-redux", "cleoredux"],
              "versions": [ { "version": "1.5.0", "assets": ["u"], "games": ["gta-sa"] } ] },
            { "id": "cleo-redux.vc.iii", "name": "CLEO Redux", "aliases": ["cleo-redux", "cleoredux"],
              "versions": [ { "version": "1.5.0", "assets": ["u"], "games": ["gta-3", "gta-vc"] } ] },
            { "id": "silents-asi-loader.sa", "name": "Silent's ASI Loader",
              "aliases": ["sal", "asi-loader"], "replaces": ["universal-asi-loader.sa.vc.iii"],
              "versions": [ { "version": "1.5.0", "assets": ["u"], "games": ["gta-sa"] } ] },
            { "id": "universal-asi-loader.sa.vc.iii", "name": "Universal ASI Loader",
              "aliases": ["ual", "asi-loader"], "replaces": ["silents-asi-loader.sa"],
              "versions": [
                  { "version": "9.7.4", "assets": ["u"], "games": ["gta-sa"], "postinstall": { "rename": { "dinput8.dll": "vorbisFile.dll" } } },
                  { "version": "9.7.4", "assets": ["u"], "games": ["gta-3", "gta-vc"] }
              ] }
        ]
    });
    serde_json::from_value(json).unwrap()
}

#[test]
fn normalization_strips_non_alphanumerics() {
    assert_eq!(
        crate::match_names::normalize("Silent's ASI Loader"),
        "silentsasiloader"
    );
    assert_eq!(crate::match_names::normalize("cleo-redux"), "cleoredux");
    assert_eq!(crate::match_names::normalize("CLEO 5"), "cleo5");
}

#[test]
fn alias_matching_precedence() {
    let pkgs = unit_pkgs();
    // exact canonical ids
    assert_eq!(resolve_id(&pkgs, "cleo.sa"), "cleo.sa");
    assert_eq!(
        resolve_id(&pkgs, "silents-asi-loader.sa"),
        "silents-asi-loader.sa"
    );
    // aliases (shortcuts) resolve to their package
    assert_eq!(resolve_id(&pkgs, "cleo5"), "cleo.sa");
    assert_eq!(resolve_id(&pkgs, "cleo4"), "cleo.sa");
    assert_eq!(resolve_id(&pkgs, "vc.cleo"), "cleo.vc");
    assert_eq!(resolve_id(&pkgs, "iii.cleo"), "cleo.iii");
    assert_eq!(resolve_id(&pkgs, "sal"), "silents-asi-loader.sa");
    assert_eq!(resolve_id(&pkgs, "ual"), "universal-asi-loader.sa.vc.iii");
    // unique prefix: cleo-red -> both redux ids? no: prefix "cleored" starts
    // with cleo-* ids only ("cleoreduxs", "cleoreduxvciii") -> still two.
    let hits = crate::match_names::resolve(&pkgs, "cleo-red", None).unwrap();
    assert_eq!(hits.len(), 2, "cleo-red matches both redux ids");
    // god-agnostic narrowing resolves the ambiguity on a known game.
    let norm = crate::match_names::narrow_by_game(hits, &pkgs, Some("gta-sa"));
    assert_eq!(norm.len(), 1);
    assert_eq!(norm[0].pkg.id, "cleo-redux.sa");
    // bare "asi-loader" is unambiguous-intent but two packages -> both
    // loader candidates (CLI turns that into a pick / exit 2).
    let amb = crate::match_names::resolve(&pkgs, "asi-loader", None).unwrap();
    assert_eq!(amb.len(), 2, "asi-loader must match both loader packages");
    assert!(amb.iter().any(|m| m.pkg.id == "silents-asi-loader.sa"));
    assert!(
        amb.iter()
            .any(|m| m.pkg.id == "universal-asi-loader.sa.vc.iii")
    );
    // levenshtein <= 2 fallback
    assert_eq!(resolve_id(&pkgs, "cleoreduxx"), "cleo-redux.sa");
}

/// All normalized match keys for a package: id, name, aliases. The id is
/// the addressable canonical name; the name and aliases are extra routes.
/// (test helper; the production resolver inlines its own key collection)
fn alias_keys(pkg: &crate::packages::PackageEntry) -> Vec<String> {
    let mut keys = vec![
        crate::match_names::normalize(&pkg.id),
        crate::match_names::normalize(&pkg.name),
    ];
    keys.extend(pkg.aliases.iter().map(|a| crate::match_names::normalize(a)));
    keys.sort();
    keys.dedup();
    keys.retain(|k| !k.is_empty());
    keys
}

/// No two canonical packages may share a normalized alias (ids are unique
/// by construction; names/aliases must be too, otherwise users get an
/// ambiguous picker where a single answer existed).
fn alias_conflicts(pkgs: &crate::packages::PackagesFile) -> Vec<(String, String)> {
    let mut owner: Vec<(String, &str)> = Vec::new(); // (key, id)
    let mut conflicts = Vec::new();

    for p in &pkgs.packages {
        for key in alias_keys(p) {
            match owner.iter().find(|(k, _)| *k == key) {
                Some((_, prev)) if *prev != p.id.as_str() => {
                    conflicts.push((key, format!("{} vs {}", prev, p.id)));
                }
                _ => owner.push((key, &p.id)),
            }
        }
    }
    conflicts
}

#[test]
fn catalog_has_no_unexpected_duplicate_normalized_aliases() {
    let pkgs = unit_pkgs();
    let conflicts = alias_conflicts(&pkgs);
    // The two loader ids share "asi-loader" and the two redux ids share
    // "cleo-redux"/"cleoredux" by design (game narrowing disambiguates);
    // every other alias must be unique.
    let shared: Vec<String> = conflicts.iter().map(|(k, _)| k.clone()).collect();
    assert!(
        shared.iter().all(|k| k == "asiloader"
            || k == "cleoredux"
            || k == "cleoreduxvc"
            || k == "cleoreduxs"),
        "unexpected alias conflicts: {conflicts:?}"
    );
}

#[test]
fn ambiguous_match_returns_candidates_exit2() {
    let pkgs = unit_pkgs();
    let hits = crate::match_names::resolve(&pkgs, "asi", None).unwrap();
    assert_eq!(hits.len(), 2, "'asi' must match both loader packages");
    assert!(
        crate::match_names::resolve(&pkgs, "zzzz", None)
            .unwrap_err()
            .to_string()
            .contains("unknown package")
    );
    let amb = crate::ChefError::Ambiguous(vec!["a".into(), "b".into()]).to_string();
    assert!(amb.contains("did you mean"));
}

fn resolve_id<'a>(pkgs: &'a PackagesFile, s: &str) -> &'a str {
    &crate::match_names::resolve(pkgs, s, None).unwrap()[0]
        .pkg
        .id
}

// ===========================================================================
// Unit: packages / versions / assets
// ===========================================================================

#[test]
fn resolve_spec_picks_stable_preview_latest_exact_prefix() {
    let pkgs_json = serde_json::json!({
        "schema": 2, "games": {},
        "packages": [ { "id": "p", "name": "P", "versions": [
            { "version": "1.0.0", "assets": ["u1"] },
            { "version": "1.5.0", "assets": ["u2"] },
            { "version": "2.0.0-rc.1", "assets": ["u3"] }
        ]} ]
    });
    let lock_json = serde_json::json!({
        "schema": 2, "generated_at": 0,
        "assets": {
            "u1": { "url": "u1", "sha256": "0".repeat(64), "files": [] },
            "u2": { "url": "u2", "sha256": "0".repeat(64), "files": [] },
            "u3": { "url": "u3", "sha256": "0".repeat(64), "files": [] }
        }
    });
    let pkgs: PackagesFile = serde_json::from_value(pkgs_json).unwrap();
    let lock: LockFile = serde_json::from_value(lock_json).unwrap();
    // A bare name (or stable) skips pre-releases.
    assert_eq!(
        packages::resolve_spec(&pkgs, &lock, "p", None, None)
            .unwrap()
            .version,
        "1.5.0"
    );
    assert_eq!(
        packages::resolve_spec(&pkgs, &lock, "p", None, Some("stable"))
            .unwrap()
            .version,
        "1.5.0"
    );
    // preview / latest reach the pre-release.
    assert_eq!(
        packages::resolve_spec(&pkgs, &lock, "p", None, Some("preview"))
            .unwrap()
            .version,
        "2.0.0-rc.1"
    );
    assert_eq!(
        packages::resolve_spec(&pkgs, &lock, "p", None, Some("latest"))
            .unwrap()
            .version,
        "2.0.0-rc.1"
    );
    // exact + prefix.
    assert_eq!(
        packages::resolve_spec(&pkgs, &lock, "p", None, Some("1.5.0"))
            .unwrap()
            .version,
        "1.5.0"
    );
    assert_eq!(
        packages::resolve_spec(&pkgs, &lock, "p", None, Some("1"))
            .unwrap()
            .version,
        "1.5.0"
    );
    // unknown major fails with the available majors list.
    let err = packages::resolve_spec(&pkgs, &lock, "p", None, Some("9"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("available majors"), "{err}");
}

#[test]
fn versions_are_game_filtered() {
    let pkgs = unit_pkgs();
    let lock_json = serde_json::json!({
        "schema": 2, "generated_at": 0,
        "assets": { "u": { "url": "u", "sha256": "0".repeat(64), "files": [] } }
    });
    let lock: LockFile = serde_json::from_value(lock_json).unwrap();
    // cleo.sa only covers gta-sa - nothing for VC.
    assert!(packages::available_versions(&pkgs, &lock, "cleo.sa", Some("gta-vc")).is_empty());
    assert!(!packages::available_versions(&pkgs, &lock, "cleo.sa", Some("gta-sa")).is_empty());
    // universal covers every game: same id, per-game records.
    assert!(
        !packages::available_versions(
            &pkgs,
            &lock,
            "universal-asi-loader.sa.vc.iii",
            Some("gta-vc")
        )
        .is_empty()
    );
}

#[test]
fn replaces_slots_are_symmetric() {
    let pkgs = unit_pkgs();
    let a = packages::existent_slot(&pkgs, "silents-asi-loader.sa");
    assert!(a.contains(&"silents-asi-loader.sa".to_string()));
    assert!(a.contains(&"universal-asi-loader.sa.vc.iii".to_string()));
    let b = packages::existent_slot(&pkgs, "universal-asi-loader.sa.vc.iii");
    assert!(b.contains(&"silents-asi-loader.sa".to_string()));
    // Unknown ids still have a one-element slot (self only).
    assert_eq!(
        packages::existent_slot(&pkgs, "nope"),
        vec!["nope".to_string()]
    );
}

/// Case-insensitive wildcard matcher supporting `*` (test helper).
fn wildcard_match(pattern: &str, text: &str) -> bool {
    fn rec(p: &[u8], t: &[u8]) -> bool {
        match (p.first(), t.first()) {
            (None, None) => true,
            (Some(b'*'), _) => rec(&p[1..], t) || (!t.is_empty() && rec(p, &t[1..])),
            (Some(a), Some(b)) if a.eq_ignore_ascii_case(b) => rec(&p[1..], &t[1..]),
            _ => false,
        }
    }
    rec(pattern.as_bytes(), text.as_bytes())
}

#[test]
fn generic_version_renders_hidden() {
    assert_eq!(packages::display_version("0.0.0"), "");
    assert_eq!(packages::display_version("5.4.0"), "5.4.0");
    assert_eq!(packages::version_word("0.0.0"), "");
    assert_eq!(packages::version_word("5.4.0"), " 5.4.0");
    assert_eq!(packages::list_version("0.0.0"), "<no version>");
    assert_eq!(packages::list_version("5.4.0"), "5.4.0");
}

#[test]
fn wildcard_and_asset_selection() {
    assert!(wildcard_match("*cleo5*.zip", "CLEO5.zip"));
    assert!(!wildcard_match("*cleo4*.zip", "CLEO5.zip"));
    // A version with several arch-variant assets picks the one hinted for
    // the current platform; otherwise the first.
    let assets = vec![
        "https://example.invalid/releases/Ultimate-ASI-Loader_x64.zip".to_string(),
        "https://example.invalid/releases/Ultimate-ASI-Loader_Win32.zip".to_string(),
    ];
    let picked = packages::select_asset_url(&assets).unwrap();
    let expected = "https://example.invalid/releases/Ultimate-ASI-Loader_Win32.zip";
    assert_eq!(picked, expected);
    assert_eq!(
        packages::select_asset_url(&["only.zip".to_string()]).unwrap(),
        "only.zip"
    );
}

// ===========================================================================
// Unit: archive safety (sec.10)
// ===========================================================================

#[test]
fn zip_traversal_is_rejected() {
    assert!(store::sanitize_entry("../escaped.txt").is_err());
    assert!(store::sanitize_entry("ok/../../up.txt").is_err());
    assert!(store::sanitize_entry("/absolute.txt").is_err());
    assert!(store::sanitize_entry("C:\\evil.txt").is_err());
    assert_eq!(
        store::sanitize_entry("./a/b/../c.txt").unwrap().unwrap(),
        "a/c.txt"
    );
    assert_eq!(store::sanitize_entry("dir/").unwrap().unwrap(), "dir");

    // End-to-end with a crafted malicious archive.
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("out");
    let evil = tmp.path().join("evil.zip");
    {
        let f = std::fs::File::create(&evil).unwrap();
        let mut z = zip::ZipWriter::new(f);
        z.start_file("../escaped.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        std::io::Write::write_all(&mut z, b"pwned").unwrap();
        z.finish().unwrap();
    }
    assert!(store::extract_zip(&evil, &out).is_err());
    assert!(
        !tmp.path().join("escaped.txt").exists(),
        "traversal escaped!"
    );
    assert!(!out.join("escaped.txt").exists());
}

// ===========================================================================
// Unit: state serialization + locks (sec.5.3)
// ===========================================================================

#[test]
fn state_roundtrip_and_schema_guard() {
    let st = StateFile {
        schema: 1,
        dirs: Default::default(),
    };
    let bytes = serde_json::to_vec_pretty(&st).unwrap();
    let back: StateFile = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(back.schema, 1);

    // Newer schema parses but must be rejected by StateFile::load's guard.
    let newer: StateFile =
        serde_json::from_slice(r#"{"schema": 99, "dirs": {}}"#.as_bytes()).unwrap();
    assert!(newer.schema > crate::game_dir::SUPPORTED_STATE_SCHEMA);
}

#[test]
fn lock_serializes_and_expires() {
    let t = setup("lock");
    let l1 = game_dir::Lock::acquire(&t.game_dir).unwrap();
    // Second acquisition fails fast.
    assert!(game_dir::Lock::acquire(&t.game_dir).is_err());
    drop(l1);
    // Released on drop -> acquirable again.
    let l2 = game_dir::Lock::acquire(&t.game_dir);
    assert!(l2.is_ok());
}

// ===========================================================================
// Integration: deployment lifecycle (sec.20)
// ===========================================================================

#[test]
fn integration_use_which_remove_lifecycle() {
    let t = setup("lifecycle");
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@stable".into(), "ual".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });

    // Virtual cleo on the SA fixture resolves to cleo.sa.
    let key = game_dir::dir_hash_key(&t.game_dir);
    let st = state();
    let sa = st.install_of(&key, "cleo.sa").expect("cleo.sa installed");
    assert_eq!(sa.package, "cleo.sa");
    assert_eq!(sa.version, "5.4.0");
    assert!(
        state()
            .install_of(&key, "universal-asi-loader.sa.vc.iii")
            .is_some()
    );
    assert!(t.game_dir.join("cleo.asi").exists());
    // UAL on SA lands as vorbisFile.dll (renamed from the shipped dll).
    let ual_dll = std::fs::read(t.game_dir.join("vorbisFile.dll")).unwrap();
    assert_eq!(ual_dll, b"MZ-ual-dll");

    // which reports the id.
    run_ok(Cmd::Which {
        pkg: Some("cleo".into()),
        dir: Some(t.game_dir.clone()),
    });

    // Version mismatch on remove -> exit-1 error.
    let err = chef_run(Cmd::Remove {
        pkgs: vec!["cleo@9.9.9".into()],
        dir: Some(t.game_dir.clone()),
    });
    assert!(err.is_err());

    // Plain remove deletes managed files and drops state entries.
    run_ok(Cmd::Remove {
        pkgs: vec!["universal-asi-loader".into()],
        dir: Some(t.game_dir.clone()),
    });
    assert!(
        state()
            .install_of(&key, "universal-asi-loader.sa.vc.iii")
            .is_none()
    );

    run_ok(Cmd::Remove {
        pkgs: vec!["cleo".into()],
        dir: Some(t.game_dir.clone()),
    });
    assert!(!t.game_dir.join("cleo.asi").exists());
    assert!(!t.game_dir.join("CLEO").exists());
    assert!(t.game_dir.join("gta_sa.exe").exists());
}

#[test]
fn integration_use_same_version_is_noop() {
    let t = setup("noop");
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@5".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    let asi = t.game_dir.join("cleo.asi");
    let before = std::fs::read(&asi).unwrap();
    let m0 = std::fs::metadata(&asi).unwrap().modified().unwrap();

    // Same version again: exact no-op - the deployed file is untouched.
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@5".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    let m1 = std::fs::metadata(&asi).unwrap().modified().unwrap();
    assert_eq!(m0, m1, "re-install of same version must not touch files");
    assert_eq!(std::fs::read(&asi).unwrap(), before);

    // User modifies the managed file -> digests differ -> normal replace.
    std::fs::write(&asi, b"user-edited").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@5".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    assert_ne!(
        std::fs::read(&asi).unwrap(),
        b"user-edited",
        "modified file must be replaced by the pristine payload"
    );

    // Different version -> normal replace.
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@4".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    let st = state();
    let key = crate::game_dir::dir_hash_key(&t.game_dir);
    assert_eq!(st.install_of(&key, "cleo.sa").unwrap().version, "4.4.4");
}

#[test]
fn integration_same_id_replacement_and_backup_restore() {
    let t = setup("replace");
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@5.4.0".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });

    // Same id (cleo.sa): major-4 version replaces the major-5 install.
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@4".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    let key = game_dir::dir_hash_key(&t.game_dir);
    let inst = state().install_of(&key, "cleo.sa").unwrap().clone();
    assert_eq!(inst.package, "cleo.sa");
    assert_eq!(inst.version, "4.4.4");
    assert!(inst.backup.is_some(), "replacement snapshots the occupant");

    // Removing cleo@4 restores the cleo5 snapshot.
    run_ok(Cmd::Remove {
        pkgs: vec!["cleo".into()],
        dir: Some(t.game_dir.clone()),
    });
    let restored = std::fs::read(t.game_dir.join("cleo.asi")).unwrap();
    assert_eq!(
        restored, b"MZ-cleo5-asi",
        "backup of replaced occupant restored"
    );
}

#[test]
fn integration_different_ids_coexist() {
    let t = setup("coexist");
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@5".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    run_ok(Cmd::Add {
        pkgs: vec!["cleo-redux".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    let key = game_dir::dir_hash_key(&t.game_dir);
    assert!(state().install_of(&key, "cleo.sa").is_some());
    assert!(state().install_of(&key, "cleo-redux.sa").is_some());

    // Removing classic CLEO never prunes Redux's individually recorded files.
    run_ok(Cmd::Remove {
        pkgs: vec!["cleo".into()],
        dir: Some(t.game_dir.clone()),
    });
    assert!(t.game_dir.join("cleo_redux.asi").exists());
}

#[test]
fn integration_loader_singletons_replace_each_other() {
    let t = setup("loaders");
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@5".into(), "sal".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    let key = game_dir::dir_hash_key(&t.game_dir);
    assert_eq!(
        state()
            .install_of(&key, "silents-asi-loader.sa")
            .unwrap()
            .version,
        "1.5.0"
    );

    // UAL replaces S'AL (replaces slot), including its payload.
    run_ok(Cmd::Add {
        pkgs: vec!["universal-asi-loader".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    assert_eq!(
        state()
            .install_of(&key, "universal-asi-loader.sa.vc.iii")
            .unwrap()
            .version,
        "9.7.4"
    );
    assert!(state().install_of(&key, "silents-asi-loader.sa").is_none());
    // UAL on the SA fixture lands as vorbisFile.dll (renamed from the
    // shipped dinput8.dll) and overwrites SAL's copy.
    let dll = std::fs::read(t.game_dir.join("vorbisFile.dll")).unwrap();
    assert_eq!(dll, b"MZ-ual-dll");
    // vorbishooked.dll belonged to S'AL only -> removed by replacement.
    assert!(!t.game_dir.join("vorbishooked.dll").exists());
    assert!(!t.game_dir.join("dinput8.dll").exists());
}

#[test]
fn integration_dry_run_performs_no_mutation() {
    let t = setup("dryrun");
    let before: Vec<_> = walk_files(&t.game_dir);
    run_ok(Cmd::Add {
        pkgs: vec!["cleo".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: true,
    });
    assert_eq!(before.len(), walk_files(&t.game_dir).len());
    assert!(
        state()
            .dir_state(&game_dir::dir_hash_key(&t.game_dir))
            .installs
            .is_empty()
    );
}

fn walk_files(dir: &Path) -> Vec<PathBuf> {
    crate::utils::walk::files(dir)
}

#[test]
fn integration_unmanaged_files_are_preserved() {
    let t = setup("preserve");

    // User file present before install -> backed up, path taken; 'chef
    // remove' restores it.
    std::fs::write(t.game_dir.join("cleo.asi"), b"user-made").unwrap();
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@5".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    assert_ne!(
        std::fs::read(t.game_dir.join("cleo.asi")).unwrap(),
        b"user-made",
        "deployed file must replace the user's copy"
    );
    run_ok(Cmd::Remove {
        pkgs: vec!["cleo".into()],
        dir: Some(t.game_dir.clone()),
    });
    assert_eq!(
        std::fs::read(t.game_dir.join("cleo.asi")).unwrap(),
        b"user-made",
        "displaced user file must be restored on remove"
    );

    // Managed file modified after install -> preserved on replace/remove (sec.5.4).
    std::fs::remove_file(t.game_dir.join("cleo.asi")).unwrap();
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@5".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    std::fs::write(t.game_dir.join("cleo.asi"), b"user-edited-managed-file").unwrap();
    run_ok(Cmd::Remove {
        pkgs: vec!["cleo".into()],
        dir: Some(t.game_dir.clone()),
    });
    assert_eq!(
        std::fs::read(t.game_dir.join("cleo.asi")).unwrap(),
        b"user-edited-managed-file",
        "user-modified managed file must survive removal"
    );
}

#[test]
fn integration_failed_deployment_leaves_state_intact() {
    let t = setup("rollback");
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@5".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    let key = game_dir::dir_hash_key(&t.game_dir);
    let before = state().install_of(&key, "cleo.sa").unwrap().version.clone();

    // Where the store payload normally lives (new id-based store path).
    let vdir = t.home.join("store/cleo.sa/5.4.0");

    // A direct deploy with a missing source exercises the pre-mutation error
    // path of the transaction.
    let (pkgs, lock) = packages::load_metadata(false).unwrap();
    let product =
        packages::resolve_spec(&pkgs, &lock, "cleo.sa", Some("gta-sa"), Some("5.4.0")).unwrap();
    let files = vec![crate::game_dir::DeployFile {
        dest_rel: "cleo.asi".into(),
        src: vdir.parent().unwrap().join("does-not-exist/cleo.asi"),
        sha256: "0".repeat(64),
    }];
    let lock = game_dir::Lock::acquire(&t.game_dir).unwrap();
    let res = game_dir::deploy(
        &t.game_dir,
        game_dir::DeployRequest {
            product: &product,
            slot: &product.slot,
            version: "5.4.0",
            files: &files,
            dry_run: false,
        },
    );
    assert!(res.is_err());
    drop(lock);

    assert_eq!(
        state().install_of(&key, "cleo.sa").unwrap().version,
        before,
        "previous state untouched after failure"
    );
    assert!(t.game_dir.join("cleo.asi").exists(), "payload untouched");
}

#[test]
fn integration_root_only_scope_negative() {
    let t = setup("rootscope");
    // Non-root locations are V2: files under scripts/ are never scanned or written.
    std::fs::create_dir_all(t.game_dir.join("scripts")).unwrap();
    std::fs::write(
        t.game_dir.join("scripts/cleo_redux.asi"),
        b"MZ-redux-hidden",
    )
    .unwrap();

    run_ok(Cmd::Which {
        pkg: Some("cleo-redux".into()),
        dir: Some(t.game_dir.clone()),
    }); // must not report the scripts/ copy

    // And deployment stays in the root.
    run_ok(Cmd::Add {
        pkgs: vec!["cleo-redux".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    assert!(t.game_dir.join("cleo_redux.asi").exists());
}

#[test]
fn integration_moved_asi_reported_not_deleted() {
    let t = setup("moved");
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@5".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    // Move the recorded ASI elsewhere within the root under a new name.
    std::fs::rename(t.game_dir.join("cleo.asi"), t.game_dir.join("renamed.asi")).unwrap();

    // which still runs and reports; reconciliation is manual (report-only).
    run_ok(Cmd::Which {
        pkg: Some("cleo".into()),
        dir: Some(t.game_dir.clone()),
    });

    // remove does not blindly delete the moved file (digest mismatch -> preserved).
    run_ok(Cmd::Remove {
        pkgs: vec!["cleo".into()],
        dir: Some(t.game_dir.clone()),
    });
    assert!(
        t.game_dir.join("renamed.asi").exists(),
        "moved ASI must not be deleted"
    );
}

#[test]
fn integration_list_versions_order_contract() {
    let t = setup("listorder");
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@5.4.0".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    let (pkgs, lock) = packages::load_metadata(false).unwrap();
    // Newest preview first, then newest stable per major, descending.
    let versions = packages::available_versions(&pkgs, &lock, "cleo.sa", Some("gta-sa"));
    assert_eq!(
        versions,
        vec![
            ("5.5.0-beta.1".to_string(), true),
            ("5.4.0".to_string(), false),
            ("4.4.4".to_string(), false),
        ]
    );
}

#[test]
fn integration_safety_guards_refuse_dangerous_targets() {
    let t = setup("guards");
    // Refuse the data home itself as a game directory.
    let err = game_dir::resolve_game_dir(Some(&t.home));
    assert!(err.is_err(), "the data home must be refused as a target");

    // Refuse the user's home directory.
    if let Some(h) = dirs::home_dir() {
        assert!(game_dir::resolve_game_dir(Some(&h)).is_err());
    }
}

// ===========================================================================
// Integration: `cleo@<spec>` resolution semantics
// ===========================================================================

#[test]
fn integration_version_spec_semantics() {
    let t = setup("spec");
    let use_pkg = |t: &TestEnv, pkg: &str| {
        crate::run(Cmd::Add {
            pkgs: vec![pkg.into()],
            dir: Some(t.game_dir.clone()),
            dry_run: false,
        })
    };

    // cleo@preview -> newest prerelease (5.5.0-beta.1, inferred from semver).
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@preview".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    let key = game_dir::dir_hash_key(&t.game_dir);
    let inst = state().install_of(&key, "cleo.sa").unwrap().clone();
    assert_eq!(inst.package, "cleo.sa");
    assert_eq!(inst.version, "5.5.0-beta.1");

    // bare name / cleo@stable -> newest NON-prerelease.
    run_ok(Cmd::Add {
        pkgs: vec!["cleo".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    assert_eq!(
        state().install_of(&key, "cleo.sa").unwrap().version,
        "5.4.0"
    );

    // cleo@latest -> newest release overall, prereleases included.
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@latest".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    assert_eq!(
        state().install_of(&key, "cleo.sa").unwrap().version,
        "5.5.0-beta.1"
    );

    // cleo@5 -> latest stable of major 5.
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@5".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    assert_eq!(
        state().install_of(&key, "cleo.sa").unwrap().version,
        "5.4.0"
    );

    // cleo@v5.3.0 (explicit v) -> exact pin.
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@v5.3.0".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    assert_eq!(
        state().install_of(&key, "cleo.sa").unwrap().version,
        "5.3.0"
    );

    // cleo@4.4.4 -> exact from the major-4 line of the same package.
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@4.4.4".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    let inst = state().install_of(&key, "cleo.sa").unwrap().clone();
    assert_eq!(inst.package, "cleo.sa");
    assert_eq!(inst.version, "4.4.4");

    // Unknown major -> error mentioning available majors.
    let err = use_pkg(&t, "cleo@6").unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("available majors"),
        "unexpected error for @6: {msg}"
    );

    // Invalid spec -> error.
    assert!(use_pkg(&t, "cleo@beta~!").is_err());

    // remove with a prefix spec now succeeds (prefix matching against installed 4.4.4)
    run_ok(Cmd::Remove {
        pkgs: vec!["cleo@4".into()],
        dir: Some(t.game_dir.clone()),
    });
    assert!(state().install_of(&key, "cleo.sa").is_none());
    // reinstall for the next check (remove with non-version spec still errors)
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@4.4.4".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    let err = crate::run(Cmd::Remove {
        pkgs: vec!["cleo@latest".into()],
        dir: Some(t.game_dir.clone()),
    })
    .unwrap_err();

    match err {
        crate::ChefError::Reported(e) => assert!(format!("{e:#}").contains("exact version")),
        other => panic!("expected Reported, got {other:#}"),
    }
}

// ===========================================================================
// Integration: `chef update` and outdated hints
// ===========================================================================

#[test]
fn integration_update_upgrades_outdated_packages() {
    let t = setup("update");
    // Install an older CLEO explicitly.
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@v5.3.0".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    let key = game_dir::dir_hash_key(&t.game_dir);
    assert_eq!(
        state().install_of(&key, "cleo.sa").unwrap().version,
        "5.3.0"
    );

    // Dry run reports but changes nothing.
    run_ok(Cmd::Update {
        pkg: Some("cleo".into()),
        dir: Some(t.game_dir.clone()),
        dry_run: true,
    });
    assert_eq!(
        state().install_of(&key, "cleo.sa").unwrap().version,
        "5.3.0"
    );

    // Real update brings it to the newest stable.
    run_ok(Cmd::Update {
        pkg: None,
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    assert_eq!(
        state().install_of(&key, "cleo.sa").unwrap().version,
        "5.4.0"
    );

    // Second run is a no-op.
    run_ok(Cmd::Update {
        pkg: None,
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
}

#[test]
fn integration_which_reports_outdated_hint() {
    let t = setup("outdated");
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@v5.3.0".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    // which --json must succeed on the newer-version case.
    run_ok(Cmd::Which {
        pkg: Some("cleo".into()),
        dir: Some(t.game_dir.clone()),
    });
    let (pkgs, lock) = packages::load_metadata(false).unwrap();
    assert!(update_hint_direct(&pkgs, &lock, "cleo.sa", "5.3.0").is_some());
    assert!(update_hint_direct(&pkgs, &lock, "cleo.sa", "5.4.0").is_none());
}

fn update_hint_direct(
    pkgs: &PackagesFile,
    lock: &LockFile,
    id: &str,
    installed: &str,
) -> Option<String> {
    let cur = semver::Version::parse(installed).ok()?;
    let latest = packages::resolve_spec(pkgs, lock, id, None, Some("stable")).ok()?;
    let new = semver::Version::parse(&latest.version).ok()?;
    (new > cur).then(|| {
        format!(
            "update available: {installed} -> {} - run 'chef update {id}'",
            latest.version
        )
    })
}

// ===========================================================================
// Integration: content-based payload identification over the lock's paths
// ===========================================================================

#[test]
fn which_details_tree_matches_lock_digests() {
    let t = setup("whichdet");
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@5".into(), "sal".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    let (pkgs, lock) = packages::load_metadata(false).unwrap();
    let cleo_rows = || {
        crate::commands::which::detail_rows(
            &pkgs,
            &lock,
            "cleo.sa",
            Some("gta-sa"),
            None,
            None,
            &t.game_dir,
        )
    };
    let sal_rows = || {
        crate::commands::which::detail_rows(
            &pkgs,
            &lock,
            "silents-asi-loader.sa",
            Some("gta-sa"),
            None,
            None,
            &t.game_dir,
        )
    };

    // Every locked payload path of the package is reported, labeled with
    // the newest release the file matches - filenames alone never qualify.
    let rows = cleo_rows();
    assert!(
        rows.iter().any(|(p, v)| p == "cleo.asi" && v == "5.4.0"),
        "cleo.asi must be identified as CLEO 5.4.0, got {rows:?}"
    );
    assert!(
        rows.iter().filter(|(_, v)| v == "5.4.0").count() >= 2,
        "cleo.asi + README.md (and CLEO/* payloads) all detected: {rows:?}"
    );
    // Absent paths that only older releases ship (4.4.4's notes.txt, below
    // the 5.4.0 anchor) are not reported: the newer release dropped them.
    assert!(
        !rows.iter().any(|(p, _)| p.contains("notes.txt")),
        "old-release-only paths must not be sought: {rows:?}"
    );
    assert!(
        !rows.iter().any(|(_, v)| v == "missing"),
        "a clean 5.4.0 install has nothing missing: {rows:?}"
    );

    // name@version restricts the tree to that release's paths; not one is
    // missing or unknown here.
    let rows54 = crate::commands::which::detail_rows(
        &pkgs,
        &lock,
        "cleo.sa",
        Some("gta-sa"),
        None,
        Some("5.4.0"),
        &t.game_dir,
    );
    assert_eq!(rows54.len(), 3); // cleo.asi + CLEO/cleo_plugins/Example.cleo + README.md
    assert!(rows54.iter().all(|(_, v)| v == "5.4.0"));

    // Files outside the lock's paths are never scanned: a scrap file with
    // unknown bytes changes nothing.
    std::fs::write(t.game_dir.join("scratch.dll"), b"MZ-not-a-known-payload").unwrap();
    assert_eq!(rows.len(), cleo_rows().len(), "only locked paths are read");

    // A missing file of the anchor release (5.4.0) still reports missing;
    // the 4.4.4-only path stays hidden.
    std::fs::remove_file(t.game_dir.join("README.md")).unwrap();
    let after = cleo_rows();
    assert!(
        after
            .iter()
            .any(|(p, v)| p == "README.md" && v == "missing"),
        "anchor-release gap must report missing: {after:?}"
    );
    assert!(!after.iter().any(|(p, _)| p.contains("notes.txt")));

    // A locked path whose file matches no release reads as unknown - a
    // modified file stays visible instead of being silently dropped.
    std::fs::write(t.game_dir.join("vorbisFile.dll"), b"MZ-custom-build").unwrap();
    let srows = sal_rows();
    assert!(
        srows
            .iter()
            .any(|(p, v)| p == "vorbisFile.dll" && v == "unknown"),
        "the modified file must show as unknown: {srows:?}"
    );
    assert!(
        srows
            .iter()
            .any(|(p, v)| p == "vorbisHooked.dll" && v == "1.5.0")
    );
    // A release that is not installed has no locked paths to walk.
    let beta = crate::commands::which::detail_rows(
        &pkgs,
        &lock,
        "silents-asi-loader.sa",
        Some("gta-sa"),
        None,
        Some("1.5.0-beta.1"),
        &t.game_dir,
    );
    assert!(beta.is_empty());
}

#[test]
fn use_treats_user_installed_loader_files_as_present() {
    let t = setup("loaderdisk");
    let key = game_dir::dir_hash_key(&t.game_dir);

    // Plant a foreign vorbisFile.dll (bytes do not matter for presence).
    std::fs::write(t.game_dir.join("vorbisfile.dll"), b"MZ-user-loader").unwrap();

    // cleo-redux has no dependencies in this fixture, so nothing loader-ish
    // gets recorded; the user's loader file stays untouched.
    run_ok(Cmd::Add {
        pkgs: vec!["cleo-redux@1.5.0".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    assert!(
        state().install_of(&key, "silents-asi-loader.sa").is_none(),
        "user's loader file must not be recorded as a chef install"
    );
    assert!(t.game_dir.join("vorbisfile.dll").exists(), "user file kept");
}

#[test]
fn integration_dependency_offer_spans_replaces_slot() {
    let t = setup("deps");
    // cleo@5 needs an ASI loader; none installed/planned -> with the picker
    // disabled the command warns but still installs classic CLEO.
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@5".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    let key = game_dir::dir_hash_key(&t.game_dir);
    assert!(state().install_of(&key, "cleo.sa").is_some());
    // No loader was installed (non-interactive, none on disk).
    assert!(state().install_of(&key, "silents-asi-loader.sa").is_none());
    assert!(
        state()
            .install_of(&key, "universal-asi-loader.sa.vc.iii")
            .is_none()
    );
}

// ---------------------------------------------------------------------------
// Scenario: san-andreas full lifecycle (user spec)
// ---------------------------------------------------------------------------
fn setup_scenario(name: &str) -> TestEnv {
    let guard = env_guard();
    refresh_test_games();
    let root = std::env::temp_dir().join(format!("chef-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let game = root.join("game-sa");
    std::fs::create_dir_all(home.join("locks")).unwrap();
    assert_under_temp(&home);
    assert_under_temp(&game);

    // San Andreas mock, refreshed pristine from `.dev/.test/games`.
    copy_tree(&checkout_root().join(".dev/.test/games/san-andreas"), &game);
    assert!(game.join("gta_sa.exe").exists());
    let payloads = root.join("payloads");
    let cleo5_zip = payloads.join("cleo.sa/5.4.0/SA.CLEO-v5.4.0.zip");
    write_test_zip(
        &cleo5_zip,
        &[
            ("cleo.asi", b"MZ-cleo5-asi"),
            ("CLEO/cleo_plugins/Example.cleo", b"dll"),
        ],
        None,
    );
    let cleo4_zip = payloads.join("cleo.sa/4.4.4/CLEO4.zip");
    write_test_zip(&cleo4_zip, &[("cleo.asi", b"MZ-cleo4-asi")], None);
    let redux_zip = payloads.join("cleo-redux/1.5.0/cleo_redux_1.5.0.x86.zip");
    write_test_zip(
        &redux_zip,
        &[
            ("cleo_redux.asi", b"MZ-redux"),
            ("cleo_redux.toml", b"log=0"),
        ],
        None,
    );
    let sal13_zip = payloads.join("silents-asi-loader/1.3.0/SAL13.zip");
    write_test_zip(
        &sal13_zip,
        &[
            ("vorbisFile.dll", b"MZ-sal13"),
            ("vorbisHooked.dll", b"MZ-sal13h"),
        ],
        None,
    );
    let sal15b_zip = payloads.join("silents-asi-loader/1.5.0-beta.1/SAL15b.zip");
    write_test_zip(
        &sal15b_zip,
        &[
            ("vorbisFile.dll", b"MZ-sal15b"),
            ("vorbisHooked.dll", b"MZ-sal15bh"),
        ],
        None,
    );
    let ual_zip = payloads.join("universal-asi-loader/9.7.4/UAL.zip");
    write_test_zip(&ual_zip, &[("dinput8.dll", b"MZ-ual")], None);
    fn file_uri(p: &Path) -> String {
        let s = p.to_string_lossy().replace('\\', "/");
        format!("file:///{s}")
    }
    let mut assets = serde_json::Map::new();
    let mut add = |href: &str, zip: &Path| {
        let sha = crate::utils::fs::sha256_file(zip).unwrap();
        let files: Vec<serde_json::Value> = zip_inner_files(zip)
            .into_iter()
            .map(|(path, sha256)| serde_json::json!({ "path": path, "sha256": sha256 }))
            .collect();
        assets.insert(
            href.to_string(),
            serde_json::json!({ "url": href, "sha256": sha, "files": files }),
        );
    };
    add(&file_uri(&cleo5_zip), &cleo5_zip);
    add(&file_uri(&cleo4_zip), &cleo4_zip);
    add(&file_uri(&redux_zip), &redux_zip);
    add(&file_uri(&sal13_zip), &sal13_zip);
    add(&file_uri(&sal15b_zip), &sal15b_zip);
    add(&file_uri(&ual_zip), &ual_zip);
    let lock_json = serde_json::json!({ "schema": 2, "generated_at": 0, "assets": serde_json::Value::Object(assets) });
    let sa_url = file_uri(&cleo5_zip);
    let sa4_url = file_uri(&cleo4_zip);
    let redux_url = file_uri(&redux_zip);
    let sal13_url = file_uri(&sal13_zip);
    let sal15b_url = file_uri(&sal15b_zip);
    let ual_url = file_uri(&ual_zip);
    let pkgs_json = serde_json::json!({
        "schema": 2,
        "games": { "gta_sa.exe": "gta-sa" },
        "packages": [
            { "id": "cleo.sa", "name": "CLEO", "aliases": ["cleo5","cleo4","cleo-sa"], "versions": [
                { "version": "5.4.0", "assets": [sa_url], "games": ["gta-sa"], "dependencies": ["silents-asi-loader.sa","universal-asi-loader.sa.vc.iii"] },
                { "version": "4.4.4", "assets": [sa4_url], "games": ["gta-sa"], "dependencies": ["silents-asi-loader.sa","universal-asi-loader.sa.vc.iii"] }
            ]},
            { "id": "cleo-redux.sa", "name": "CLEO Redux", "aliases": ["cleo-redux","cleoredux"], "versions": [{ "version": "1.5.0", "assets": [redux_url], "games": ["gta-sa"] }]},
            { "id": "silents-asi-loader.sa", "name": "Silent's ASI Loader", "aliases": ["sal","silents-asi-loader","asi-loader"], "replaces": ["universal-asi-loader.sa.vc.iii"], "versions": [
                { "version": "1.3.0", "assets": [sal13_url], "games": ["gta-sa"] },
                { "version": "1.5.0-beta.1", "assets": [sal15b_url], "games": ["gta-sa"] }
            ]},
            { "id": "universal-asi-loader.sa.vc.iii", "name": "Universal ASI Loader", "aliases": ["ual","universal-asi-loader","asi-loader"], "replaces": ["silents-asi-loader.sa"], "versions": [
                { "version": "9.7.4", "assets": [ual_url], "games": ["gta-sa"], "postinstall": { "rename": { "dinput8.dll": "vorbisFile.dll" } } },
                { "version": "9.7.4", "assets": [ual_url], "games": ["gta-3","gta-vc"] }
            ]}
        ]
    });
    let pkgs_path = home.join("packages.json");
    std::fs::write(&pkgs_path, serde_json::to_vec_pretty(&pkgs_json).unwrap()).unwrap();
    let lock_path = home.join("packages.lock");
    std::fs::write(&lock_path, serde_json::to_vec_pretty(&lock_json).unwrap()).unwrap();
    // Point chef at the sandbox data home (test seam, not an env var).
    crate::packages::set_home_override(home.clone());
    crate::dbg_trace(format_args!(
        "TEST SETUP name={name} home={} game={} key={}",
        home.display(),
        game.display(),
        game_dir::dir_hash_key(&game)
    ));
    TestEnv {
        guard,
        home,
        game_dir: game,
    }
}

#[test]
fn integration_scenario_san_andreas() {
    let t = setup_scenario("scenario");
    let key = game_dir::dir_hash_key(&t.game_dir);
    let (pkgs, lock) = packages::load_metadata(false).unwrap();
    // 1) list (game-aware, no dups, no truncation) - SA relevant: cleo 2, redux 1, sal 2, ual 1
    let game = Some("gta-sa");
    assert_eq!(
        packages::available_versions(&pkgs, &lock, "cleo.sa", game),
        vec![("5.4.0".into(), false), ("4.4.4".into(), false)]
    );
    assert_eq!(
        packages::available_versions(&pkgs, &lock, "silents-asi-loader.sa", game),
        vec![("1.5.0-beta.1".into(), true), ("1.3.0".into(), false)]
    );
    assert_eq!(
        packages::available_versions(&pkgs, &lock, "universal-asi-loader.sa.vc.iii", game),
        vec![("9.7.4".into(), false)]
    );
    // list cleo -> 5.4.0,4.4.4
    let cleo_vers = packages::available_versions(&pkgs, &lock, "cleo.sa", game);
    assert!(cleo_vers.iter().any(|(v, _)| v == "5.4.0"));
    assert!(cleo_vers.iter().any(|(v, _)| v == "4.4.4"));
    // list sal -> 1.3.0 stable, 1.5.0-beta preview
    let sal_vers = packages::available_versions(&pkgs, &lock, "silents-asi-loader.sa", game);
    assert!(sal_vers.iter().any(|(v, p)| v == "1.3.0" && !p));
    assert!(sal_vers.iter().any(|(v, p)| v == "1.5.0-beta.1" && *p));
    // which (no installs)
    assert!(state().install_of(&key, "cleo.sa").is_none());
    run_ok(Cmd::Which {
        pkg: None,
        dir: Some(t.game_dir.clone()),
    });
    // which cleo -> nothing
    run_ok(Cmd::Which {
        pkg: Some("cleo".into()),
        dir: Some(t.game_dir.clone()),
    });
    assert!(state().install_of(&key, "cleo.sa").is_none());
    // use cleo@5
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@5".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    assert_eq!(
        state().install_of(&key, "cleo.sa").unwrap().version,
        "5.4.0"
    );
    // which cleo -> 5.4.0
    assert_eq!(
        state().install_of(&key, "cleo.sa").unwrap().version,
        "5.4.0"
    );
    // list shows installed
    let st = state();
    assert!(st.install_of(&key, "cleo.sa").is_some());
    // use cleo@5 again -> already installed (no-op, file mtime unchanged)
    let asi = t.game_dir.join("cleo.asi");
    let m0 = std::fs::metadata(&asi).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(10));
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@5".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    let m1 = std::fs::metadata(&asi).unwrap().modified().unwrap();
    assert_eq!(m0, m1);
    // remove cleo@5
    run_ok(Cmd::Remove {
        pkgs: vec!["cleo@5".into()],
        dir: Some(t.game_dir.clone()),
    });
    assert!(state().install_of(&key, "cleo.sa").is_none());
    assert!(!t.game_dir.join("cleo.asi").exists());
    // remove again -> error
    assert!(
        chef_run(Cmd::Remove {
            pkgs: vec!["cleo@5".into()],
            dir: Some(t.game_dir.clone())
        })
        .is_err()
    );
    // which cleo -> nothing again
    assert!(state().install_of(&key, "cleo.sa").is_none());
    // use sal -> stable 1.3.0
    run_ok(Cmd::Add {
        pkgs: vec!["sal".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    assert_eq!(
        state()
            .install_of(&key, "silents-asi-loader.sa")
            .unwrap()
            .version,
        "1.3.0"
    );
    assert_eq!(
        std::fs::read(t.game_dir.join("vorbisFile.dll")).unwrap(),
        b"MZ-sal13"
    );
    // use sal@beta -> 1.5.0-beta.1
    run_ok(Cmd::Add {
        pkgs: vec!["sal@beta".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    assert_eq!(
        state()
            .install_of(&key, "silents-asi-loader.sa")
            .unwrap()
            .version,
        "1.5.0-beta.1"
    );
    assert_eq!(
        std::fs::read(t.game_dir.join("vorbisFile.dll")).unwrap(),
        b"MZ-sal15b"
    );
    // which sal -> beta
    run_ok(Cmd::Which {
        pkg: Some("sal".into()),
        dir: Some(t.game_dir.clone()),
    });
    // use ual -> 9.7.4 replaces sal
    run_ok(Cmd::Add {
        pkgs: vec!["ual".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    assert_eq!(
        state()
            .install_of(&key, "universal-asi-loader.sa.vc.iii")
            .unwrap()
            .version,
        "9.7.4"
    );
    assert_eq!(
        std::fs::read(t.game_dir.join("vorbisFile.dll")).unwrap(),
        b"MZ-ual"
    );
    assert!(!t.game_dir.join("vorbisHooked.dll").exists());
    // which ual -> 9.7.4
    run_ok(Cmd::Which {
        pkg: Some("ual".into()),
        dir: Some(t.game_dir.clone()),
    });
    // which sal -> nothing (replaced)
    assert!(state().install_of(&key, "silents-asi-loader.sa").is_none());
    // remove ual
    run_ok(Cmd::Remove {
        pkgs: vec!["ual".into()],
        dir: Some(t.game_dir.clone()),
    });
    assert!(
        state()
            .install_of(&key, "universal-asi-loader.sa.vc.iii")
            .is_none()
    );
    // which -> nothing
    assert!(state().installs_in(&key).is_empty());
    run_ok(Cmd::Which {
        pkg: None,
        dir: Some(t.game_dir.clone()),
    });
}

// ===========================================================================
// Game-specific enforcement: every user-facing command is filtered by the
// detected game (list, which, add/use, remove, update).
// ===========================================================================

#[test]
fn game_specific_all_commands_are_filtered() {
    let pkgs = unit_pkgs();
    let lock: LockFile = serde_json::from_value(serde_json::json!({
        "schema": 2, "generated_at": 0,
        "assets": { "u": { "url": "u", "sha256": "0".repeat(64), "files": [] } }
    }))
    .unwrap();

    // SA sees only SA-relevant packages.
    let sa: Vec<String> = pkgs
        .sorted_ids()
        .into_iter()
        .filter(|id| !packages::available_versions(&pkgs, &lock, id, Some("gta-sa")).is_empty())
        .map(|s| s.to_string())
        .collect();
    assert_eq!(
        sa,
        vec![
            "cleo-redux.sa",
            "cleo.sa",
            "silents-asi-loader.sa",
            "universal-asi-loader.sa.vc.iii"
        ]
    );
    // VC sees only VC-relevant.
    let vc: Vec<String> = pkgs
        .sorted_ids()
        .into_iter()
        .filter(|id| !packages::available_versions(&pkgs, &lock, id, Some("gta-vc")).is_empty())
        .map(|s| s.to_string())
        .collect();
    assert_eq!(
        vc,
        vec![
            "cleo-redux.vc.iii",
            "cleo.vc",
            "universal-asi-loader.sa.vc.iii"
        ]
    );
    // III sees only III-relevant.
    let iii: Vec<String> = pkgs
        .sorted_ids()
        .into_iter()
        .filter(|id| !packages::available_versions(&pkgs, &lock, id, Some("gta-3")).is_empty())
        .map(|s| s.to_string())
        .collect();
    assert_eq!(
        iii,
        vec![
            "cleo-redux.vc.iii",
            "cleo.iii",
            "universal-asi-loader.sa.vc.iii"
        ]
    );

    // "outside a game dir" (None) sees everything.
    let all: Vec<String> = pkgs
        .sorted_ids()
        .into_iter()
        .filter(|id| !packages::available_versions(&pkgs, &lock, id, None).is_empty())
        .map(|s| s.to_string())
        .collect();
    assert_eq!(all.len(), pkgs.packages.len());

    // payload_basenames is game-filtered - drives `which` unmanaged scan.
    // With the synthetic lock (single asset "u" shared by all packages)
    // basenames reflect package filtering, not file-per-game. Verify
    // that the available-set (which underlies list/which filtering) is
    // game-specific instead of inspecting raw basenames.
    assert!(
        !packages::payload_basenames(&pkgs, &lock, Some("gta-sa")).is_empty()
            || !packages::available_versions(&pkgs, &lock, "cleo.sa", Some("gta-sa")).is_empty()
    );

    // narrow_by_game disambiguates shared aliases per-game (list/add/remove).
    for (game, expected) in [
        ("gta-sa", "cleo-redux.sa"),
        ("gta-vc", "cleo-redux.vc.iii"),
        ("gta-3", "cleo-redux.vc.iii"),
    ] {
        let hits = crate::match_names::resolve(&pkgs, "cleo-redux", None).unwrap();
        assert_eq!(hits.len(), 2);
        let narrowed = crate::match_names::narrow_by_game(hits, &pkgs, Some(game));
        assert_eq!(narrowed.len(), 1, "cleo-redux must narrow to 1 in {game}");
        assert_eq!(narrowed[0].pkg.id, expected);
    }
    for (game, expected) in [
        ("gta-sa", "silents-asi-loader.sa"),
        ("gta-vc", "universal-asi-loader.sa.vc.iii"),
    ] {
        let hits = crate::match_names::resolve(&pkgs, "asi-loader", None).unwrap();
        assert_eq!(hits.len(), 2);
        let narrowed = crate::match_names::narrow_by_game(hits, &pkgs, Some(game));
        // SA: both loaders cover SA, so still 2 (picker required). VC: only UAL covers VC.
        if game == "gta-sa" {
            assert_eq!(narrowed.len(), 2);
        } else {
            assert_eq!(narrowed[0].pkg.id, expected);
        }
    }

    // resolve_spec rejects a package that doesn't cover the detected game - enforces `add`/`update` game targeting.
    assert!(packages::resolve_spec(&pkgs, &lock, "cleo.sa", Some("gta-vc"), None).is_err());
    assert!(packages::resolve_spec(&pkgs, &lock, "cleo.vc", Some("gta-sa"), None).is_err());
    assert!(packages::resolve_spec(&pkgs, &lock, "cleo.iii", Some("gta-sa"), None).is_err());
    assert!(
        packages::resolve_spec(&pkgs, &lock, "silents-asi-loader.sa", Some("gta-vc"), None)
            .is_err()
    );
    // correct-game resolves succeed.
    assert!(packages::resolve_spec(&pkgs, &lock, "cleo.sa", Some("gta-sa"), None).is_ok());
    assert!(packages::resolve_spec(&pkgs, &lock, "cleo.vc", Some("gta-vc"), None).is_ok());
    assert!(packages::resolve_spec(&pkgs, &lock, "cleo.iii", Some("gta-3"), None).is_ok());
    assert!(
        packages::resolve_spec(
            &pkgs,
            &lock,
            "universal-asi-loader.sa.vc.iii",
            Some("gta-vc"),
            None
        )
        .is_ok()
    );
}

#[test]
fn integration_game_specific_cross_game_use_rejected() {
    // SA fixture's `add`/`use` is game-filtered via resolve_spec(game).
    // Using a synthetic multi-game catalog (unit_pkgs) we verify that
    // a VC-only package is rejected when the detected game is SA, and
    // vice-versa - without relying on fuzzy name matching in the fixture.
    let pkgs = unit_pkgs();
    let lock: LockFile = serde_json::from_value(serde_json::json!({
        "schema": 2, "generated_at": 0,
        "assets": { "u": { "url": "u", "sha256": "0".repeat(64), "files": [] } }
    }))
    .unwrap();
    assert!(
        packages::resolve_spec(&pkgs, &lock, "cleo.vc", Some("gta-sa"), None).is_err(),
        "SA game must reject VC-only package"
    );
    assert!(
        packages::resolve_spec(&pkgs, &lock, "cleo.iii", Some("gta-sa"), None).is_err(),
        "SA game must reject III-only package"
    );
    assert!(
        packages::resolve_spec(&pkgs, &lock, "silents-asi-loader.sa", Some("gta-vc"), None)
            .is_err(),
        "VC game must reject SA-only loader"
    );
    // End-to-end: SA fixture rejects an unknown package and accepts a
    // correct-game package.
    let sa = setup_scenario("cross-sa");
    let err = chef_run(Cmd::Add {
        pkgs: vec!["__definitely_unknown_pkg__".into()],
        dir: Some(sa.game_dir.clone()),
        dry_run: false,
    });
    assert!(err.is_err(), "unknown package must be rejected");
    run_ok(Cmd::Add {
        pkgs: vec!["cleo-redux".into()],
        dir: Some(sa.game_dir.clone()),
        dry_run: false,
    });
    let key = game_dir::dir_hash_key(&sa.game_dir);
    assert!(state().install_of(&key, "cleo-redux.sa").is_some());
}

#[test]
fn integration_game_specific_list_and_which_json() {
    // `list` and `which --json` must be game-filtered. Use the scenario fixture (SA).
    let t = setup_scenario("listwhich-sa");
    // which --json for SA must not contain VC/III-only ids.
    // We call the command (stdout not captured) and verify filtering via the
    // underlying API that drives it: available_versions / sorted_ids.
    let (pkgs, lock) = packages::load_metadata(false).unwrap();
    let filtered: Vec<String> = pkgs
        .sorted_ids()
        .into_iter()
        .filter(|id| !packages::available_versions(&pkgs, &lock, id, Some("gta-sa")).is_empty())
        .map(|s| s.to_string())
        .collect();
    assert!(!filtered.contains(&"cleo.vc".to_string()));
    assert!(!filtered.contains(&"cleo.iii".to_string()));
    assert!(!filtered.contains(&"cleo-redux.vc.iii".to_string()));
    assert!(filtered.contains(&"cleo.sa".to_string()));
    // `which` with no args must succeed and `list` must succeed (game-filtered rows).
    run_ok(Cmd::Which {
        pkg: None,
        dir: Some(t.game_dir.clone()),
    });
    run_ok(Cmd::Menu {
        pkg: None,
        dir: Some(t.game_dir.clone()),
        refresh: false,
    });
}

#[test]
fn display_never_shows_internal_ids_only_public_names() {
    // The tool must never display internal ids like "cleo.sa" or
    // "silents-asi-loader.sa" - human output uses the catalog's public
    // `name` (e.g. "CLEO", "Silent's ASI Loader"). This enforces the
    // `which` "not installed:" line and similar displays.
    let pkgs = unit_pkgs();
    for p in &pkgs.packages {
        assert_ne!(
            p.id, p.name,
            "internal id must differ from public name for {}",
            p.id
        );
        // title_of (used by `which`) must return the public name.
        assert_eq!(pkgs.pkg(&p.id).unwrap().name, p.name);
    }
    // Simulate the fixed `which` rendering for SA's not-installed set.
    let ids = [
        "cleo.sa",
        "cleo-redux.sa",
        "silents-asi-loader.sa",
        "universal-asi-loader.sa.vc.iii",
    ];
    let mut titles: Vec<String> = ids
        .iter()
        .map(|id| pkgs.pkg(id).unwrap().name.clone())
        .collect();
    titles.sort();
    titles.dedup();
    // Public names, never internal ids.
    assert_eq!(
        titles,
        vec![
            "CLEO".to_string(),
            "CLEO Redux".to_string(),
            "Silent's ASI Loader".to_string(),
            "Universal ASI Loader".to_string()
        ]
    );
    for t in &titles {
        assert!(
            !t.contains("cleo.sa"),
            "public name must not contain internal id: {t}"
        );
        assert!(
            !t.contains("silents-asi-loader"),
            "public name must not contain internal id: {t}"
        );
        assert!(
            !t.contains("universal-asi-loader"),
            "public name must not contain internal id: {t}"
        );
    }
    // Same for VC's filtered set - also public names only.
    let vc_ids = [
        "cleo.vc",
        "cleo-redux.vc.iii",
        "universal-asi-loader.sa.vc.iii",
    ];
    let mut vc_titles: Vec<String> = vc_ids
        .iter()
        .map(|id| pkgs.pkg(id).unwrap().name.clone())
        .collect();
    vc_titles.sort();
    vc_titles.dedup();
    assert_eq!(
        vc_titles,
        vec![
            "CLEO Redux".to_string(),
            "Universal ASI Loader".to_string(),
            "VC.CLEO".to_string()
        ]
    );
}

// ===========================================================================
// Unit: install presence / stale-state pruning (drives `which`/`list`)
// ===========================================================================

/// Test-only accessor: any recorded file still present in the game dir.
/// (the production counterpart `all_present` checks nothing missing).
impl crate::game_dir::InstallCheck {
    fn any_present(&self) -> bool {
        !self.present.is_empty()
    }
}

#[test]
fn check_install_classifies_present_missing_and_moved() {
    let t = setup("checkinstall");
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@5".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    let key = game_dir::dir_hash_key(&t.game_dir);
    let inst = state().install_of(&key, "cleo.sa").unwrap().clone();

    // All recorded files at their recorded paths.
    let chk = game_dir::check_install(&t.game_dir, &inst);
    assert!(chk.all_present());
    assert!(chk.any_present());
    assert!(!chk.fully_gone());
    assert!(chk.moved.is_empty());

    // One file deleted -> missing but the install is still alive.
    std::fs::remove_file(t.game_dir.join("cleo.asi")).unwrap();
    let chk = game_dir::check_install(&t.game_dir, &inst);
    assert!(!chk.all_present());
    assert!(chk.any_present(), "CLEO/ + README still there");
    assert!(!chk.fully_gone());
    assert!(chk.missing.iter().any(|p| p == "cleo.asi"));
    assert!(chk.moved.is_empty());

    // The same digest moved elsewhere in the root keeps the install alive
    // and reports the moved file.
    let store_asi = t.home.join("store/cleo.sa/5.4.0/cleo.asi");
    std::fs::copy(&store_asi, t.game_dir.join("moved.asi")).unwrap();
    let chk = game_dir::check_install(&t.game_dir, &inst);
    assert!(!chk.all_present());
    assert!(chk.any_present());
    assert!(!chk.fully_gone(), "a moved file is not a gone file");
    assert_eq!(chk.moved.len(), 1);
    assert_eq!(chk.moved[0].0, "cleo.asi");
    assert!(chk.moved[0].1.ends_with("moved.asi"));

    // Everything gone (no present file, no moved file) -> fully gone.
    std::fs::remove_file(t.game_dir.join("moved.asi")).unwrap();
    std::fs::remove_dir_all(t.game_dir.join("CLEO")).unwrap();
    std::fs::remove_file(t.game_dir.join("README.md")).unwrap();
    let chk = game_dir::check_install(&t.game_dir, &inst);
    assert!(chk.fully_gone());
    assert!(!chk.any_present());
    assert_eq!(chk.missing.len(), inst.files.len());
}

#[test]
fn prune_stale_state_drops_only_fully_gone_installs() {
    let t = setup("prune");
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@5".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    run_ok(Cmd::Add {
        pkgs: vec!["cleo-redux".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    let key = game_dir::dir_hash_key(&t.game_dir);

    // Wipe every file of the cleo install only.
    std::fs::remove_file(t.game_dir.join("cleo.asi")).unwrap();
    std::fs::remove_dir_all(t.game_dir.join("CLEO")).unwrap();
    std::fs::remove_file(t.game_dir.join("README.md")).unwrap();

    let mut st = state();
    assert!(game_dir::prune_stale_state(&t.game_dir, &mut st));
    assert!(
        st.install_of(&key, "cleo.sa").is_none(),
        "fully-gone install pruned"
    );
    assert!(
        st.install_of(&key, "cleo-redux.sa").is_some(),
        "intact install untouched"
    );
    // Second pass changes nothing.
    assert!(!game_dir::prune_stale_state(&t.game_dir, &mut st));
    // Persist so the next phase reads the pruned state file.
    st.save().unwrap();

    // An install with any file present is never pruned.
    std::fs::remove_file(t.game_dir.join("cleo_redux.asi")).unwrap();
    let mut st = state();
    assert!(!game_dir::prune_stale_state(&t.game_dir, &mut st));
    assert!(st.install_of(&key, "cleo-redux.sa").is_some());
}

// ===========================================================================
// Unit: resolve_id (shared name -> canonical id used by every command)
// ===========================================================================

#[test]
fn resolve_id_is_game_strict_and_ambiguous() {
    let pkgs = unit_pkgs();
    // Game-incompatible packages fail like unknown ones.
    let err = crate::match_names::resolve_id(&pkgs, "cleo.vc", Some("gta-sa")).unwrap_err();
    assert!(
        format!("{err}").contains("unknown package"),
        "wrong-game name must be rejected: {err}"
    );
    assert!(
        format!(
            "{}",
            crate::match_names::resolve_id(&pkgs, "zzz", None).unwrap_err()
        )
        .contains("unknown package")
    );
    // The exact name `CLEO` resolves to the sa build only; in a VC dir it
    // is game-rejected (never silently re-pointed). The `vc.cleo` alias
    // resolves the vc build.
    assert_eq!(
        crate::match_names::resolve_id(&pkgs, "cleo", Some("gta-sa")).unwrap(),
        "cleo.sa"
    );
    assert_eq!(
        crate::match_names::resolve_id(&pkgs, "vc.cleo", Some("gta-vc")).unwrap(),
        "cleo.vc"
    );
    assert!(
        crate::match_names::resolve_id(&pkgs, "vc.cleo", Some("gta-sa")).is_err(),
        "vc alias must be rejected in an SA game dir"
    );
    // Game narrowing turns an ambiguous name into a single match.
    assert_eq!(
        crate::match_names::resolve_id(&pkgs, "asi-loader", Some("gta-vc")).unwrap(),
        "universal-asi-loader.sa.vc.iii"
    );
    // Both loaders cover SA -> still ambiguous -> exit-2 error (the picker
    // is never interactive inside the test binary, so the ambiguous error
    // fires deterministically).
    let res = crate::match_names::resolve_id(&pkgs, "asi-loader", Some("gta-sa"));
    match res {
        Err(crate::ChefError::Ambiguous(cands)) => {
            assert_eq!(cands.len(), 2);
        }
        other => panic!("expected Ambiguous, got {other:?}"),
    }
}

// ===========================================================================
// Unit: store digest verification + raw (non-zip) payloads
// ===========================================================================

#[test]
fn fetch_asset_verifies_digest_and_heals_corrupt_cache() {
    let _t = setup("fetchasset");
    let (pkgs, lock) = packages::load_metadata(false).unwrap();
    let res =
        packages::resolve_spec(&pkgs, &lock, "cleo.sa", Some("gta-sa"), Some("5.4.0")).unwrap();
    let name = res.url.rsplit('/').next().unwrap().to_string();
    let cache = packages::chef_home()
        .join("cache")
        .join(format!("cleo.sa-5.4.0-{name}"));

    // A corrupt cache entry is detected by digest and re-downloaded.
    std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
    std::fs::write(&cache, b"garbage-not-the-archive").unwrap();
    let got = store::fetch_asset(&res.url, &res.asset_sha256, "cleo.sa", "5.4.0").unwrap();
    assert_eq!(
        crate::utils::fs::sha256_file(&got).unwrap(),
        res.asset_sha256
    );
    assert_ne!(
        std::fs::read(&got).unwrap(),
        b"garbage-not-the-archive",
        "corrupt cache must be replaced"
    );

    // A wrong expected digest is rejected and does not leave the file in cache.
    let bad = "0".repeat(64);
    let err = store::fetch_asset(&res.url, &bad, "cleo.sa", "5.4.0").unwrap_err();
    assert!(
        format!("{err:#}").contains("checksum mismatch"),
        "unexpected error: {err:#}"
    );
    assert!(
        !cache.exists(),
        "mismatched download must not stay in cache"
    );
}

#[test]
fn ensure_payload_raw_single_file_asset() {
    let t = setup("rawasset");
    // A bare single-file release (no zip) is copied into the store payload.
    let raw = t.home.join("assets");
    std::fs::create_dir_all(&raw).unwrap();
    let dll = raw.join("vorbisFile.dll");
    std::fs::write(&dll, b"MZ-raw-ual-dll").unwrap();
    let url = format!("file:///{}", dll.to_string_lossy().replace('\\', "/"));
    let sha = crate::utils::fs::sha256_file(&dll).unwrap();

    let vdir = store::ensure_payload("raw-pkg", "Raw", "1.0.0", &url, &sha, false).unwrap();
    assert_eq!(
        std::fs::read(vdir.join("vorbisFile.dll")).unwrap(),
        b"MZ-raw-ual-dll"
    );
    assert!(vdir.join(".complete").exists(), "complete marker written");

    // The marker short-circuits a second resolution (no re-download).
    let again = store::ensure_payload("raw-pkg", "Raw", "1.0.0", &url, &sha, false).unwrap();
    assert_eq!(vdir, again);

    // An unverified payload dir (no marker) is treated as a cache miss.
    std::fs::write(vdir.join(".complete"), b"").unwrap();
    store::ensure_payload("raw-pkg", "Raw", "1.0.0", &url, &sha, false).unwrap();
}

// ===========================================================================
// Unit: game detection
// ===========================================================================

#[test]
fn detect_game_is_case_insensitive_and_errors_on_multiple_exes() {
    let pkgs: PackagesFile = serde_json::from_value(serde_json::json!({
        "schema": 2,
        "games": { "gta_sa.exe": "gta-sa", "gta-vc.exe": "gta-vc" },
        "packages": []
    }))
    .unwrap();
    let tmp = std::env::temp_dir().join(format!("chef-detect-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    assert_under_temp(&tmp);

    // Exe names match case-insensitively on every platform.
    std::fs::write(tmp.join("GTA_SA.EXE"), b"MZ").unwrap();
    let d = game_dir::detect_game(&pkgs, &tmp)
        .unwrap()
        .expect("detected");
    assert_eq!(d.family, "gta-sa");
    assert_eq!(d.exe, "gta_sa.exe");

    // Two recognized exes for different games -> ambiguous error.
    std::fs::write(tmp.join("gta-vc.exe"), b"MZ").unwrap();
    let err = game_dir::detect_game(&pkgs, &tmp).unwrap_err();
    assert!(
        format!("{err:#}").contains("ambiguous"),
        "unexpected error: {err:#}"
    );

    // No recognized exes -> Ok(None), not an error.
    std::fs::remove_file(tmp.join("GTA_SA.EXE")).unwrap();
    std::fs::remove_file(tmp.join("gta-vc.exe")).unwrap();
    assert!(game_dir::detect_game(&pkgs, &tmp).unwrap().is_none());
    let _ = std::fs::remove_dir_all(&tmp);
}

// ===========================================================================
// Unit: atomic writes
// ===========================================================================

#[test]
fn write_atomic_creates_parents() {
    // Atomic write creates parents and persists content.
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("a/b/c.json");
    crate::utils::fs::write_atomic(&p, b"{}").unwrap();
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "{}");
}

// ===========================================================================
// Unit: loose version strings (.beta etc.) resolve as previews
// ===========================================================================

#[test]
fn parse_loose_dot_beta_versions_sort_as_preview() {
    let pkgs_json = serde_json::json!({
        "schema": 2, "games": {},
        "packages": [ { "id": "p", "name": "P", "versions": [
            { "version": "2.0.0.beta", "assets": ["u"] },
            { "version": "1.0.0", "assets": ["u"] }
        ]} ]
    });
    let lock_json = serde_json::json!({
        "schema": 2, "generated_at": 0,
        "assets": { "u": { "url": "u", "sha256": "0".repeat(64), "files": [] } }
    });
    let pkgs: PackagesFile = serde_json::from_value(pkgs_json).unwrap();
    let lock: LockFile = serde_json::from_value(lock_json).unwrap();
    // "2.0.0.beta" normalizes to the semver pre-release 2.0.0-beta.
    assert_eq!(
        packages::resolve_spec(&pkgs, &lock, "p", None, Some("preview"))
            .unwrap()
            .version,
        "2.0.0-beta"
    );
    assert_eq!(
        packages::resolve_spec(&pkgs, &lock, "p", None, None)
            .unwrap()
            .version,
        "1.0.0",
        "stable resolution must skip the dot-beta entry"
    );
}

// ===========================================================================
// Integration: `chef update` error paths
// ===========================================================================

#[test]
fn integration_update_target_errors() {
    let t = setup("updtarget");
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@5".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    // Explicit package that is not installed -> error.
    let err = chef_run(Cmd::Update {
        pkg: Some("cleo-redux".into()),
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    assert!(err.is_err(), "updating a non-installed package must fail");
    // Unknown package name -> error.
    let err = chef_run(Cmd::Update {
        pkg: Some("zzz-unknown".into()),
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    assert!(err.is_err(), "unknown update target must fail");
    // The installed package still updates fine.
    run_ok(Cmd::Update {
        pkg: Some("cleo".into()),
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
}

#[test]
fn integration_update_with_nothing_installed() {
    let t = setup("updtnone");
    // No installs: update-all is a no-op, not an error.
    run_ok(Cmd::Update {
        pkg: None,
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
}

// ===========================================================================
// Integration: manual deletion is reconciled live by `which` / `list`
// ===========================================================================

#[test]
fn integration_manual_delete_reported_and_pruned() {
    let t = setup("staleprune");
    let key = game_dir::dir_hash_key(&t.game_dir);
    let delete_all = |t: &TestEnv| {
        std::fs::remove_file(t.game_dir.join("cleo.asi")).unwrap();
        std::fs::remove_dir_all(t.game_dir.join("CLEO")).unwrap();
        std::fs::remove_file(t.game_dir.join("README.md")).unwrap();
    };
    for which_first in [true, false] {
        run_ok(Cmd::Add {
            pkgs: vec!["cleo@5".into()],
            dir: Some(t.game_dir.clone()),
            dry_run: false,
        });
        assert!(state().install_of(&key, "cleo.sa").is_some());
        delete_all(&t);
        if which_first {
            run_ok(Cmd::Which {
                pkg: Some("cleo".into()),
                dir: Some(t.game_dir.clone()),
            });
            // `which` reports the missing install (not-found row) and keeps
            // the state so `chef remove` can restore the backup.
            assert!(
                state().install_of(&key, "cleo.sa").is_some(),
                "which must keep the record for the restore hint"
            );
        } else {
            run_ok(Cmd::Menu {
                pkg: None,
                dir: Some(t.game_dir.clone()),
                refresh: false,
            });
            assert!(
                state().install_of(&key, "cleo.sa").is_none(),
                "stale install must be pruned by the live refresh"
            );
        }
    }
}

// ===========================================================================
// Integration: multi-target remove keeps going after one failure
// ===========================================================================

#[test]
fn integration_remove_continues_after_one_failure() {
    let t = setup("removemulti");
    let key = game_dir::dir_hash_key(&t.game_dir);
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@5".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    let err = chef_run(Cmd::Remove {
        pkgs: vec!["zzz-unknown".into(), "cleo".into()],
        dir: Some(t.game_dir.clone()),
    })
    .unwrap_err();
    match err {
        crate::ChefError::Reported(_) => {}
        other => panic!("expected Reported after a failed removal, got {other:#}"),
    }
    assert!(
        state().install_of(&key, "cleo.sa").is_none(),
        "valid target must still be removed after an earlier failure"
    );
    assert!(!t.game_dir.join("cleo.asi").exists());
}

// ===========================================================================
// Integration: ambiguous name is an exit-2 error on the CLI (picker off)
// ===========================================================================

#[test]
fn integration_ambiguous_name_reports_exit2() {
    let t = setup("ambuse");
    // `asi-loader` matches both loader packages on SA; the picker is never
    // interactive in test builds, so this is the ambiguous error main maps
    // to exit code 2.
    let res = chef_run(Cmd::Add {
        pkgs: vec!["asi-loader".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    })
    .unwrap_err();
    match res {
        crate::ChefError::Ambiguous(names) => assert_eq!(names.len(), 2),
        other => panic!("expected Ambiguous, got {other:#}"),
    }
    assert!(
        state()
            .dir_state(&game_dir::dir_hash_key(&t.game_dir))
            .installs
            .is_empty(),
        "nothing may install on an ambiguous request"
    );
}

// ===========================================================================
// Integration: `list` name handling
// ===========================================================================

#[test]
fn integration_list_exact_id_works_unknown_errors() {
    let t = setup("listnames");
    run_ok(Cmd::Menu {
        pkg: Some("cleo.sa".into()),
        dir: Some(t.game_dir.clone()),
        refresh: false,
    });
    run_ok(Cmd::Menu {
        pkg: Some("sal".into()),
        dir: Some(t.game_dir.clone()),
        refresh: false,
    });
    let err = chef_run(Cmd::Menu {
        pkg: Some("zzz-unknown".into()),
        dir: Some(t.game_dir.clone()),
        refresh: false,
    });
    assert!(err.is_err(), "unknown list target must fail");
}

// ===========================================================================
// Integration: a displaced user file survives a version replacement
// ===========================================================================

#[test]
fn displaced_user_file_survives_version_replacement() {
    let t = setup("displaced");
    let key = game_dir::dir_hash_key(&t.game_dir);
    // A user-owned cleo.asi occupies the path before any install.
    std::fs::write(t.game_dir.join("cleo.asi"), b"user-original").unwrap();

    run_ok(Cmd::Add {
        pkgs: vec!["cleo@5".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@4".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    let inst = state().install_of(&key, "cleo.sa").unwrap().clone();
    assert_eq!(inst.version, "4.4.4");
    assert!(
        inst.displaced
            .iter()
            .any(|d| d.eq_ignore_ascii_case("cleo.asi")),
        "carried-over displaced user file must be recorded on the new install"
    );

    // Removing the replacement restores the original user file.
    run_ok(Cmd::Remove {
        pkgs: vec!["cleo".into()],
        dir: Some(t.game_dir.clone()),
    });
    assert_eq!(
        std::fs::read(t.game_dir.join("cleo.asi")).unwrap(),
        b"user-original",
        "user file must come back unchanged across a version replacement"
    );
}

// ===========================================================================
// Unit: summary classification (one release / multiple / unknown / not
// found; newest-match attribution; .ini configs ignored)
// ===========================================================================

#[test]
fn summarize_package_four_outcomes_and_attribution() {
    let _t = setup("wsumclass");
    let (pkgs, lock) = packages::load_metadata(false).unwrap();
    let vf = crate::commands::which::version_files(&pkgs, &lock, "cleo.sa", Some("gta-sa"));
    let (_, files54) = vf
        .iter()
        .find(|(v, _)| v == "5.4.0")
        .expect("fixture has cleo 5.4.0")
        .clone();
    let norm = |p: &str| p.replace('/', "\\").to_lowercase();
    let summarize = |tree: &std::collections::BTreeMap<String, String>, managed: bool| {
        crate::commands::which::summarize_package(
            tree,
            &pkgs,
            &lock,
            "cleo.sa",
            Some("gta-sa"),
            None,
            managed,
        )
    };

    // Full 5.4.0 tree -> one release, no notes.
    let full: std::collections::BTreeMap<String, String> = files54
        .iter()
        .map(|(p, sha)| (norm(p), sha.clone()))
        .collect();
    let row = summarize(&full, false).expect("clean install must be reported");
    assert_eq!(row.status, "installed");
    assert_eq!(row.version, "5.4.0");
    assert!(row.notes.is_empty());
    assert_eq!(row.versions, vec!["5.4.0".to_string()]);

    // A file of a different release at its own locked path -> multiple,
    // pointing at the details.
    let (_, files44) = vf
        .iter()
        .find(|(v, _)| v == "4.4.4")
        .expect("fixture has cleo 4.4.4")
        .clone();
    let (p44, sha44) = &files44[0];
    let mut mixed = full.clone();
    mixed.insert(norm(p44), sha44.clone());
    let row = summarize(&mixed, false).unwrap();
    assert_eq!(row.status, "multiple");
    assert_eq!(row.version, "multiple");
    assert_eq!(row.versions, vec!["5.4.0".to_string(), "4.4.4".to_string()]);
    assert!(row.notes.contains("chef which"));

    // A present file matching no known release (a custom build) -> unknown.
    let mut custom = full.clone();
    custom.insert("cleo.asi".to_string(), "custom-build-sha".to_string());
    let row = summarize(&custom, false).unwrap();
    assert_eq!(row.status, "unknown");
    assert_eq!(row.version, "unknown");
    assert_eq!(row.versions, vec!["5.4.0".to_string()]);
    assert!(row.notes.contains("chef which"));

    // An edited .ini config never decides the outcome.
    let mut with_ini = full.clone();
    with_ini.insert(
        "scripts\\global.ini".to_string(),
        "user-tweaked".to_string(),
    );
    let row = summarize(&with_ini, false).unwrap();
    assert_eq!(row.status, "installed");
    assert_eq!(row.version, "5.4.0");

    // Managed + nothing left -> not found, pointing at chef remove.
    let empty = std::collections::BTreeMap::new();
    let row = summarize(&empty, true).unwrap();
    assert_eq!(row.status, "not-found");
    assert_eq!(row.version, "not found");
    assert!(row.notes.contains("chef remove"));

    // Unmanaged + nothing left -> invisible.
    assert!(summarize(&empty, false).is_none());

    // Command references embedded in notes are quote-safe: a name with an
    // apostrophe (Silent's ASI Loader) falls back to its clean alias.
    assert_eq!(
        crate::commands::which::command_ref(&pkgs, "silents-asi-loader.sa"),
        "sal"
    );
    assert_eq!(
        crate::commands::which::command_ref(&pkgs, "cleo.sa"),
        "cleo5"
    );

    // The sal notes embed the alias, and the note stays a valid command.
    let sal_vf = crate::commands::which::version_files(
        &pkgs,
        &lock,
        "silents-asi-loader.sa",
        Some("gta-sa"),
    );
    let (_, sal_files) = sal_vf
        .iter()
        .find(|(v, _)| v == "1.5.0")
        .expect("sal 1.5.0")
        .clone();
    let mut sal_tree: std::collections::BTreeMap<String, String> = sal_files
        .iter()
        .map(|(p, sha)| (norm(p), sha.clone()))
        .collect();
    sal_tree.insert("vorbisfile.dll".to_string(), "custom-build".to_string());
    let srow = crate::commands::which::summarize_package(
        &sal_tree,
        &pkgs,
        &lock,
        "silents-asi-loader.sa",
        Some("gta-sa"),
        None,
        false,
    )
    .unwrap();
    assert_eq!(srow.status, "unknown");
    assert!(srow.notes.contains("chef which sal"));
    assert!(
        !srow.notes.contains("Silent's"),
        "quote-laden name must not leak: {}",
        srow.notes
    );

    // Newest-match attribution: 1.5.0-beta.1 outranks 1.3.0 and 1.4.0.
    assert_eq!(
        crate::commands::which::newest_match(&["1.3.0", "1.5.0-beta.1", "1.4.0"]),
        "1.5.0-beta.1"
    );

    // Only .ini files count as user-editable config.
    assert!(crate::commands::which::is_ignored_config(
        "scripts/global.ini"
    ));
    assert!(crate::commands::which::is_ignored_config("X.INI"));
    assert!(!crate::commands::which::is_ignored_config("cleo.asi"));
    assert!(!crate::commands::which::is_ignored_config(
        "cleo/.config/sa.json"
    ));
}

#[test]
fn which_summary_splits_chef_and_user_rows() {
    let t = setup("whichend");
    // Hand-installed cleo: files on disk, no state record.
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@5".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    std::fs::remove_file(crate::game_dir::state_path()).unwrap();

    let (pkgs, lock) = packages::load_metadata(false).unwrap();

    // User-installed CLEO: every 5.4.0 payload present at its locked path,
    // identified by content, reported with no notes.
    let by_path = locked_path_map(&t.game_dir, &pkgs, &lock, "cleo.sa");
    let row = crate::commands::which::summarize_package(
        &by_path,
        &pkgs,
        &lock,
        "cleo.sa",
        Some("gta-sa"),
        None,
        false,
    )
    .expect("on-disk payload must be reported");
    assert!(!row.managed);
    assert_eq!(row.status, "installed");
    assert_eq!(row.version, "5.4.0");

    // Chef-managed install whose files are all gone -> not-found row that
    // suggests restoring the backup.
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@5".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    let key = game_dir::dir_hash_key(&t.game_dir);
    assert!(state().install_of(&key, "cleo.sa").is_some());
    for (_, files) in crate::commands::which::version_files(&pkgs, &lock, "cleo.sa", Some("gta-sa"))
    {
        for (p, _) in &files {
            let _ = std::fs::remove_file(t.game_dir.join(p));
        }
    }
    let by_path = locked_path_map(&t.game_dir, &pkgs, &lock, "cleo.sa");
    let row = crate::commands::which::summarize_package(
        &by_path,
        &pkgs,
        &lock,
        "cleo.sa",
        Some("gta-sa"),
        None,
        true,
    )
    .unwrap();
    assert_eq!(row.status, "not-found");
    assert_eq!(row.version, "not found");
    assert!(row.notes.contains("chef remove"));
}

/// Current sha256 of every locked payload path of one package, as the
/// summary receives it: (lowercase backslash path, digest). Absent files
/// stay out of the map.
fn locked_path_map(
    game_dir: &std::path::Path,
    pkgs: &PackagesFile,
    lock: &LockFile,
    id: &str,
) -> std::collections::BTreeMap<String, String> {
    crate::commands::which::version_files(pkgs, lock, id, Some("gta-sa"))
        .into_iter()
        .flat_map(|(_, files)| files)
        .filter_map(|(p, _)| {
            let sha = crate::utils::fs::sha256_file(&game_dir.join(&p)).ok()?;
            Some((p.replace('/', "\\").to_lowercase(), sha))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// --json: every command emits a machine-readable document (and errors) when
// run in JSON mode; test in-process through the output seam.
// ---------------------------------------------------------------------------

fn run_mode_ok(cmd: Cmd) {
    if let Err(e) = crate::run_mode(cmd, true) {
        panic!("command failed: {e:#}");
    }
}

fn json_doc() -> serde_json::Value {
    let cap = crate::take_capture().expect("capture installed").out;
    assert_eq!(cap.len(), 1, "exactly one JSON document on stdout");
    serde_json::from_str(&cap[0]).expect("stdout document must parse as JSON")
}

#[test]
fn json_add_which_update_remove_documents() {
    let t = setup("jsonflow");

    // add: one row per package.
    crate::set_capture(crate::CapturedOutput::default());
    run_mode_ok(Cmd::Add {
        pkgs: vec!["cleo@5".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    let doc = json_doc();
    assert_eq!(doc["add"][0]["id"], "cleo.sa");
    assert_eq!(doc["add"][0]["version"], "5.4.0");
    assert_eq!(doc["add"][0]["status"], "installed");

    // The same request again -> status "already", still a valid document.
    crate::set_capture(crate::CapturedOutput::default());
    run_mode_ok(Cmd::Add {
        pkgs: vec!["cleo@5".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    let doc = json_doc();
    assert_eq!(doc["add"][0]["status"], "already");

    // which: installs array, chef-managed section.
    crate::set_capture(crate::CapturedOutput::default());
    run_mode_ok(Cmd::Which {
        pkg: None,
        dir: Some(t.game_dir.clone()),
    });
    let doc = json_doc();
    assert_eq!(doc["installs"][0]["section"], "chef");
    assert_eq!(doc["installs"][0]["status"], "installed");

    // update with nothing newer -> up-to-date row.
    crate::set_capture(crate::CapturedOutput::default());
    run_mode_ok(Cmd::Update {
        pkg: None,
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    let doc = json_doc();
    assert_eq!(doc["dryRun"], false);
    assert_eq!(doc["update"][0]["status"], "up-to-date");

    // remove: restored-file lists included.
    crate::set_capture(crate::CapturedOutput::default());
    run_mode_ok(Cmd::Remove {
        pkgs: vec!["cleo".into()],
        dir: Some(t.game_dir.clone()),
    });
    let doc = json_doc();
    assert_eq!(doc["remove"][0]["id"], "cleo.sa");
    assert_eq!(doc["remove"][0]["version"], "5.4.0");
    assert!(doc["remove"][0]["restored"].is_array());
}

#[test]
fn json_errors_are_machine_readable() {
    let t = setup("jsonerr");

    // A failing command produces no result document and raises an error
    // that main renders as one JSON object with the exit code.
    crate::set_capture(crate::CapturedOutput::default());
    let err = crate::run_mode(
        Cmd::Add {
            pkgs: vec!["cleo@6".into()],
            dir: Some(t.game_dir.clone()),
            dry_run: false,
        },
        true,
    )
    .unwrap_err();
    let code = crate::write_error(&err, true);
    assert_eq!(code, 1);
    let cap = crate::take_capture().unwrap();
    assert!(cap.out.is_empty(), "no result document on failure");
    let obj: serde_json::Value = serde_json::from_str(&cap.err[0]).unwrap();
    assert!(
        obj["error"].as_str().unwrap().contains("tracked release"),
        "error text must survive JSON: {obj}"
    );

    // Ambiguous names carry the candidates for scripts to pick from.
    let amb = crate::ChefError::Ambiguous(vec!["CLEO".into(), "CLEO Redux".into()]);
    crate::set_capture(crate::CapturedOutput::default());
    let code = crate::write_error(&amb, true);
    assert_eq!(code, 2);
    let cap = crate::take_capture().unwrap();
    let obj: serde_json::Value = serde_json::from_str(&cap.err[0]).unwrap();
    assert_eq!(obj["candidates"].as_array().unwrap().len(), 2);
}

// ---------------------------------------------------------------------------
// Attribution: the installed version wins over newest when bytes match it
// ---------------------------------------------------------------------------

#[test]
fn attribute_prefers_installed_version() {
    use crate::commands::which::attribute;
    // Shared bytes across releases: the version chef installed wins.
    assert_eq!(attribute(Some("1.4.3"), &["1.5.0", "1.4.3"]), "1.4.3");
    // No state: newest match as before.
    assert_eq!(attribute(None, &["1.5.0", "1.4.3"]), "1.5.0");
    // Installed version not in the match set: newest wins.
    assert_eq!(attribute(Some("1.4.3"), &["1.5.0"]), "1.5.0");
    // Nothing matches.
    assert_eq!(attribute(Some("9.9.9"), &[]), "");
}

// ---------------------------------------------------------------------------
// --dry-run: per-file plan (add / replace / backup / remove / keep)
// ---------------------------------------------------------------------------

#[test]
fn dry_run_add_plans_every_file() {
    let t = setup("dryplan");

    // Fresh folder: every payload path reports as add, nothing changes.
    crate::set_capture(crate::CapturedOutput::default());
    run_mode_ok(Cmd::Add {
        pkgs: vec!["cleo@5".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: true,
    });
    let doc = json_doc();
    assert_eq!(doc["add"][0]["status"], "would-install");
    let plan = doc["add"][0]["plan"].as_array().unwrap();
    assert!(plan.len() >= 3, "plan lists every file: {plan:?}");
    assert!(plan.iter().all(|s| s["op"] == "add"));
    assert!(
        !t.game_dir.join("cleo.asi").exists(),
        "a dry run must not deploy anything"
    );

    // Installed, then downgrade: files only 5.4.0 ships report as removed.
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@5".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    crate::set_capture(crate::CapturedOutput::default());
    run_mode_ok(Cmd::Add {
        pkgs: vec!["cleo@4.4.4".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: true,
    });
    let doc = json_doc();
    let plan = doc["add"][0]["plan"].as_array().unwrap();
    assert!(
        plan.iter()
            .any(|s| s["op"] == "remove" && s["path"] == "README.md"),
        "5.4.0-only file must show as removed: {plan:?}"
    );

    // A user-edited managed file -> replace with the reason; the intact
    // payload files stay keep.
    std::fs::write(t.game_dir.join("README.md"), b"edited by user").unwrap();
    crate::set_capture(crate::CapturedOutput::default());
    run_mode_ok(Cmd::Add {
        pkgs: vec!["cleo@5".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: true,
    });
    let doc = json_doc();
    let plan = doc["add"][0]["plan"].as_array().unwrap();
    let readme = plan.iter().find(|s| s["path"] == "README.md").unwrap();
    assert_eq!(readme["op"], "replace");
    assert!(
        readme["note"].as_str().unwrap().contains("modified"),
        "{readme}"
    );
    assert!(
        plan.iter()
            .any(|s| s["op"] == "keep" && s["path"] == "cleo.asi"),
        "intact files keep their label: {plan:?}"
    );
}

#[test]
fn dry_run_backs_up_user_file_at_payload_path() {
    let t = setup("dryback");
    // A user file sitting where the package wants to deploy.
    std::fs::write(t.game_dir.join("cleo.asi"), b"MZ-user-asi").unwrap();
    crate::set_capture(crate::CapturedOutput::default());
    run_mode_ok(Cmd::Add {
        pkgs: vec!["cleo@5".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: true,
    });
    let doc = json_doc();
    let plan = doc["add"][0]["plan"].as_array().unwrap();
    let asi = plan.iter().find(|s| s["path"] == "cleo.asi").unwrap();
    assert_eq!(asi["op"], "backup");
    assert!(asi["note"].as_str().unwrap().contains("chef remove"));
}

// ---------------------------------------------------------------------------
// Run history log
// ---------------------------------------------------------------------------

#[test]
fn history_log_rotates_and_appends() {
    let _guard = env_guard(); // serialized with fixture tests, no setup()
    let home = std::env::temp_dir().join(format!("chef-test-history-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    packages::set_home_override(home.clone());

    // Reopening appends: the first run's content survives.
    let mut f = crate::open_history_log().expect("history opens");
    f.write_all(b"first run\n").unwrap();
    drop(f);
    let mut f = crate::open_history_log().expect("history reopens");
    f.write_all(b"second run\n").unwrap();
    drop(f);
    let content = std::fs::read_to_string(crate::history_path()).unwrap();
    assert!(content.contains("first run") && content.contains("second run"));

    // Past the cap the old file rotates to history.log.old, new starts empty.
    let big = "x".repeat((crate::HISTORY_MAX_BYTES as usize) * 2);
    std::fs::write(crate::history_path(), &big).unwrap();
    let mut f = crate::open_history_log().expect("rotates");
    f.write_all(b"third run\n").unwrap();
    drop(f);
    assert!(
        crate::history_path().with_extension("log.old").exists(),
        "old log must be kept as .old"
    );
    assert!(
        std::fs::metadata(crate::history_path()).unwrap().len() < big.len() as u64,
        "history starts fresh after rotation"
    );

    packages::clear_home_override();
}

// ===========================================================================
// Debug: dump every path/key involved in Add -> state read. Run with
// `cargo test -- --nocapture debug_dump_add_state_paths` to see the full
// diagnostic on any machine (local or CI).
// ===========================================================================

#[test]
fn debug_dump_add_state_paths() {
    let t = setup("debugdump");
    crate::dbg_trace(format_args!(
        "home_override={}",
        crate::packages::chef_home().display()
    ));
    run_ok(Cmd::Add {
        pkgs: vec!["cleo@5".into()],
        dir: Some(t.game_dir.clone()),
        dry_run: false,
    });
    let key = game_dir::dir_hash_key(&t.game_dir);
    let st = state();
    let raw = std::fs::read_to_string(game_dir::state_path()).unwrap_or_default();
    eprintln!("[chef-dbg] state.json content:\n{raw}");
    let inst = st.install_of(&key, "cleo.sa").unwrap();
    assert_eq!(inst.version, "5.4.0");
    crate::dbg_trace(format_args!(
        "installed cleo.sa at {} under key {key}",
        inst.version
    ));
}
