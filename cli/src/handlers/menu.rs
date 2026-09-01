use serde_json::json;
use std::collections::HashSet;
use std::path::PathBuf;

use crate::ChefError;
use crate::packages::{self, PackagesFile};

/// A display row: one package id and its product name.
#[derive(Clone)]
struct Row {
    id: String,
    name: String,
}

fn rows_for_all(pkgs: &PackagesFile) -> Vec<Row> {
    pkgs.packages
        .iter()
        .map(|p| Row {
            id: p.id.clone(),
            name: p.name.clone(),
        })
        .collect()
}

/// `available_versions` entries as display strings ("1.5.0 (preview)").
fn avail_texts(entries: &[(String, bool)]) -> Vec<String> {
    entries
        .iter()
        .map(|(v, pre)| {
            let v = packages::list_version(v);
            if *pre {
                format!("{v} (preview)")
            } else {
                v.to_string()
            }
        })
        .collect()
}

/// Restrict `all` rows to one user-supplied package. Exact id wins;
/// otherwise name/alias resolution with `list`'s convention: ambiguous
/// candidates are an error (exit 2), never a picker.
fn narrow_rows(
    all: Vec<Row>,
    pkgs: &PackagesFile,
    name: &str,
    game: Option<&str>,
) -> crate::Result<Vec<Row>> {
    let norm = packages::normalize(name);

    if let Some(exact) = all.iter().find(|r| packages::normalize(&r.id) == norm) {
        return Ok(vec![exact.clone()]);
    }

    let hits = packages::resolve(pkgs, name, game).map_err(ChefError::Other)?;
    if let Some(g) = game
        && hits.iter().all(|m| !pkgs.covers_game(&m.pkg.id, g))
    {
        return Err(ChefError::Other(anyhow::anyhow!(
            "unknown package '{name}'"
        )));
    }

    let narrowed = packages::narrow_by_game(hits, pkgs, game);
    match narrowed.as_slice() {
        [one] => Ok(all.into_iter().filter(|r| r.id == one.pkg.id).collect()),
        many => Err(ChefError::Ambiguous(
            many.iter().map(|m| m.display().to_string()).collect(),
        )),
    }
}

/// Build and present the menu. `dir` is already resolved best-effort by
/// the command layer (the menu tolerates a missing game directory).
pub fn run(
    pkg: Option<&str>,
    dir: Option<PathBuf>,
    json: bool,
    refresh: bool,
) -> crate::Result<()> {
    let (pkgs, lock) = packages::load_metadata(refresh).map_err(ChefError::Other)?;
    let all = rows_for_all(&pkgs);

    // Restrict AVAILABLE to releases usable in the detected game.
    let detected_game = dir
        .as_deref()
        .and_then(|d| crate::game_dir::detect_game(&pkgs, d).ok().flatten())
        .map(|g| g.family);

    // Live refresh: prune stale state where 0 files present (and not moved)
    // so manual deletes become "not installed".
    if let Some(d) = dir.as_deref()
        && let Ok(mut state) = crate::game_dir::StateFile::load()
        && crate::game_dir::prune_stale_state(d, &mut state)
    {
        let _ = state.save();
    }

    let mut rows: Vec<Row> = match pkg {
        None => all,
        Some(name) => narrow_rows(all, &pkgs, name, detected_game.as_deref())?,
    };

    // Only display packages relevant for the current game (if detected).
    // Outside a game directory every package is relevant.
    if let Some(game) = detected_game.as_deref() {
        rows.retain(|r| !packages::available_versions(&pkgs, &lock, &r.id, Some(game)).is_empty());
    }
    // Deduplicate by title (e.g. CLEO Redux has two ids for different games
    // but one display name). Keep first per title.
    {
        let mut seen = HashSet::new();
        rows.retain(|r| seen.insert(r.name.clone()));
    }

    if json {
        let entries = rows
            .iter()
            .map(|r| {
                let entries =
                    packages::available_versions(&pkgs, &lock, &r.id, detected_game.as_deref());
                // JSON stays machine-readable: plain semvers plus a separate
                // preview field for the marked entry.
                let versions: Vec<String> = entries.iter().map(|(v, _)| v.clone()).collect();
                let latest = entries
                    .iter()
                    .find(|(_, pre)| !*pre)
                    .map(|(v, _)| v.clone());
                let preview = entries.iter().find(|(_, pre)| *pre).map(|(v, _)| v.clone());
                json!({
                    "package": r.id,
                    "title": r.name,
                    "versions": versions,
                    "latest": latest,
                    "preview": preview,
                    // "managed": managed_for(r, &pkgs, dir.as_deref()),
                })
            })
            .collect::<Vec<_>>();
        crate::emit::emit_json(&json!({
            "dir": dir.map(|d| d.to_string_lossy().to_string()),
            "packages": entries,
        }));
        return Ok(());
    }

    // Dynamic column widths.
    let w_title = rows
        .iter()
        .map(|r| r.name.chars().count())
        .max()
        .unwrap_or(5)
        .max(5);
    let w_avail = rows
        .iter()
        .map(|r| {
            avail_texts(&packages::available_versions(
                &pkgs,
                &lock,
                &r.id,
                detected_game.as_deref(),
            ))
            .iter()
            .map(|s| s.chars().count())
            .sum()
        })
        .max()
        .unwrap_or(9)
        .max(9);
    println!("{:<w_title$} {:<w_avail$}", "TITLE", "AVAILABLE");

    for r in &rows {
        let entries = packages::available_versions(&pkgs, &lock, &r.id, detected_game.as_deref());
        let avail = avail_texts(&entries).join(", ");
        // let inst = managed_for_display(r, &pkgs, dir.as_deref()).unwrap_or_else(|| "-".into());
        println!("{:<w_title$} {:<w_avail$}", r.name, avail);
    }
    Ok(())
}
