//! User-facing package name matching (merged from `match_names`): strict
//! exact matching against a package's product `name` or one of its
//! `aliases`, game narrowing, and the interactive picker for ambiguous
//! candidates. Loose matching (prefix / substring / edit distance) and
//! matching against the internal package `id` were removed.

use crate::ChefError;
use crate::utils::term;

use super::catalog::{PackageEntry, PackagesFile};

/// Normalize a package name or alias for matching.
pub fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_lowercase()
}

/// A package reference that is safe to paste inside single quotes in a
/// `chef` command. By convention the first alias is the shortest usable
/// name (`sal`, `cleo5`, `ual`), so notes and menus embed that; the
/// display name is used only when the package has no aliases, the id as a
/// last resort.
pub fn command_ref(pkgs: &PackagesFile, id: &str) -> String {
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

/// A successful name match: one package entry.
#[derive(Debug, Clone)]
pub struct NameMatch<'a> {
    pub pkg: &'a PackageEntry,
}

impl<'a> NameMatch<'a> {
    /// The singleton key used in state / store paths.
    pub fn package_key(&self) -> &str {
        &self.pkg.id
    }

    /// Display name (product name from the catalog).
    pub fn display(&self) -> &str {
        &self.pkg.name
    }
}

struct Key<'a> {
    text: String,
    pkg: &'a PackageEntry,
}

fn collect_keys(pkgs: &PackagesFile) -> Vec<Key<'_>> {
    let mut keys = Vec::new();
    for p in &pkgs.packages {
        keys.push(Key {
            text: normalize(&p.name),
            pkg: p,
        });
        for a in &p.aliases {
            keys.push(Key {
                text: normalize(a),
                pkg: p,
            });
        }
    }
    keys.retain(|k| !k.text.is_empty());
    keys
}

/// Pick among candidates: a single one wins; several go to the
/// interactive picker on a TTY or become an ambiguous error (exit code 2).
pub fn disambiguate<'a>(cands: Vec<NameMatch<'a>>) -> crate::Result<NameMatch<'a>> {
    match cands.as_slice() {
        [one] => Ok(one.clone()),
        many => {
            let names: Vec<String> = many.iter().map(|m| m.display().to_string()).collect();
            if term::interactive() {
                let opts = names.clone();
                match term::pick("ambiguous package name - did you mean:", &opts) {
                    Some(i) => Ok(many[i].clone()),
                    None => Err(ChefError::Other(anyhow::anyhow!("no package selected"))),
                }
            } else {
                Err(ChefError::Ambiguous(names))
            }
        }
    }
}

/// Resolve a user-supplied name to one canonical package id. Game-strict:
/// unknown names and names whose packages do not cover the detected game
/// are errors; ambiguous candidates go to the picker or exit code 2.
pub fn resolve_id(pkgs: &PackagesFile, name: &str, game: Option<&str>) -> crate::Result<String> {
    let hits = resolve(pkgs, name, game)?;
    if let Some(g) = game
        && hits.iter().all(|m| !pkgs.covers_game(&m.pkg.id, g))
    {
        return Err(ChefError::Other(anyhow::anyhow!(
            "unknown package '{name}'"
        )));
    }
    let m = disambiguate(narrow_by_game(hits, pkgs, game))?;
    Ok(m.package_key().to_string())
}

/// Resolve a user-supplied name to package candidates.
///
/// Matching is strict: the input must equal, after normalization, a
/// package's product `name` or one of its `aliases`. Prefix, substring
/// and edit-distance matching are gone, and the canonical `id` is not a
/// user-facing match key. Anything else is unknown.
///
/// Zero matches is an error; multiple candidates are returned so the
/// caller can pick interactively or fail with exit code 2.
pub fn resolve<'a>(
    pkgs: &'a PackagesFile,
    input: &str,
    _game: Option<&str>,
) -> anyhow::Result<Vec<NameMatch<'a>>> {
    let norm = normalize(input);
    if norm.is_empty() {
        anyhow::bail!("empty package name");
    }

    let keys = collect_keys(pkgs);

    let mut hits: Vec<NameMatch<'a>> = Vec::new();
    for k in &keys {
        if k.text == norm && !hits.iter().any(|m| m.pkg.id == k.pkg.id) {
            hits.push(NameMatch { pkg: k.pkg });
        }
    }

    if hits.is_empty() {
        anyhow::bail!("unknown package '{}'", input);
    }
    Ok(hits)
}

/// Narrow candidates to the ones usable in a detected game (a version
/// record covers the game). When none qualifies - or no game is known -
/// the original candidate set is returned unchanged so the caller can
/// still prompt/fail normally.
pub fn narrow_by_game<'a>(
    cands: Vec<NameMatch<'a>>,
    pkgs: &PackagesFile,
    game: Option<&str>,
) -> Vec<NameMatch<'a>> {
    match game {
        None => cands,
        Some(g) => {
            let narrowed: Vec<NameMatch> = cands
                .iter()
                .filter(|m| pkgs.covers_game(&m.pkg.id, g))
                .cloned()
                .collect();
            if narrowed.is_empty() { cands } else { narrowed }
        }
    }
}
