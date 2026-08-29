use std::path::PathBuf;

use log::{info, warn};

use crate::ChefError;
use crate::commands::add as add_cmd;
use crate::game_dir::{self, StateFile};
use crate::match_names;
use crate::packages;

pub fn run(
    pkg: Option<&str>,
    dir: Option<PathBuf>,
    dry_run: bool,
    json: bool,
) -> crate::Result<()> {
    let game_dir = game_dir::resolve_game_dir(dir.as_deref()).map_err(ChefError::Other)?;
    let (pkgs, lock) = packages::load_metadata(false).map_err(ChefError::Other)?;
    let game = game_dir::detect_game(&pkgs, &game_dir)
        .map_err(ChefError::Other)?
        .map(|d| d.family);
    let state = StateFile::load().map_err(ChefError::Other)?;
    let key = game_dir::dir_hash_key(&game_dir);

    // Decide which packages to consider: the requested one, or everything
    // installed in this game directory.
    let targets: Vec<String> = match pkg {
        Some(name) => {
            let norm = match_names::normalize(name);
            if let Some(p) = pkgs
                .packages
                .iter()
                .find(|p| match_names::normalize(&p.id) == norm)
            {
                vec![p.id.clone()]
            } else {
                vec![match_names::resolve_id(&pkgs, name, game.as_deref())?]
            }
        }
        None => state
            .installs_in(&key)
            .into_iter()
            .map(|i| i.package.clone())
            .collect(),
    };

    if targets.is_empty() {
        if json {
            crate::emit_json(&serde_json::json!({ "update": [], "dryRun": dry_run }));
            return Ok(());
        }
        info!("nothing installed in {}", game_dir.display());
        return Ok(());
    }

    let mut changed = 0;

    #[derive(serde::Serialize)]
    struct Row {
        id: String,
        name: String,
        from: String,
        to: String,
        status: &'static str,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        plan: Vec<add_cmd::PlanStep>,
    }
    let mut rows: Vec<Row> = Vec::new();

    for id in &targets {
        let Some(inst) = state.install_of(&key, id) else {
            if pkg.is_some() {
                return Err(ChefError::Other(anyhow::anyhow!(
                    "'{id}' is not installed in {}",
                    game_dir.display()
                )));
            }
            continue;
        };

        let Ok(cur) = semver::Version::parse(&inst.version) else {
            continue;
        };

        let name = pkgs
            .pkg(id)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| id.clone());

        if pkgs.pkg(id).is_none() {
            warn!("'{id}' is no longer a known package - remove it with 'chef remove {id}'");
            continue;
        }

        let res = packages::resolve_spec(&pkgs, &lock, id, game.as_deref(), Some("stable"))
            .map_err(ChefError::Other)?;

        let Ok(new) = semver::Version::parse(&res.version) else {
            continue;
        };

        if new <= cur {
            info!(
                "{name}{} - up to date",
                packages::version_word(&inst.version)
            );
            rows.push(Row {
                id: id.clone(),
                name: name.clone(),
                from: packages::display_version(&inst.version).to_string(),
                to: packages::display_version(&inst.version).to_string(),
                status: "up-to-date",
                plan: Vec::new(),
            });
            continue;
        }

        if dry_run {
            info!(
                "{name}: {} -> {} (dry run)",
                packages::display_version(&inst.version),
                packages::display_version(&res.version)
            );
            let steps = add_cmd::install_one(&res, &game_dir, true)?;
            rows.push(Row {
                id: id.clone(),
                name: name.clone(),
                from: packages::display_version(&inst.version).to_string(),
                to: packages::display_version(&res.version).to_string(),
                status: "would-update",
                plan: steps,
            });
        } else {
            info!(
                "updating {name}: {} -> {}",
                packages::display_version(&inst.version),
                packages::display_version(&res.version)
            );
            add_cmd::install_one(&res, &game_dir, false)?;
            changed += 1;
            rows.push(Row {
                id: id.clone(),
                name: name.clone(),
                from: packages::display_version(&inst.version).to_string(),
                to: packages::display_version(&res.version).to_string(),
                status: "updated",
                plan: Vec::new(),
            });
        }
    }

    if json {
        crate::emit_json(&serde_json::json!({ "update": rows, "dryRun": dry_run }));
        return Ok(());
    }

    if dry_run {
        info!("dry run - nothing was changed");
    } else if changed == 0 {
        info!("everything up to date");
    }

    crate::commands::upgrade::update_notice();
    Ok(())
}
