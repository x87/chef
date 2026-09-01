//! User-facing package name matching (merged from `match_names`): exact,
//! prefix/substring, and Levenshtein fallback resolution, game narrowing,
//! and the interactive picker for ambiguous candidates.

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
        for raw in [&p.id, &p.name] {
            keys.push(Key {
                text: normalize(raw),
                pkg: p,
            });
        }
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

/// Levenshtein edit distance (small DP; inputs are short names).
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
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

    fn collect<'a>(hits: &mut Vec<NameMatch<'a>>, seen: &mut Vec<&'a PackageEntry>, k: &Key<'a>) {
        if !seen.iter().any(|p| p.id == k.pkg.id) {
            seen.push(k.pkg);
            hits.push(NameMatch { pkg: k.pkg });
        }
    }

    let mut hits: Vec<NameMatch<'a>> = Vec::new();
    let mut seen: Vec<&'a PackageEntry> = Vec::new();

    // 1. exact normalized match wins outright.
    for k in &keys {
        if k.text == norm {
            collect(&mut hits, &mut seen, k);
        }
    }
    if !hits.is_empty() {
        return Ok(hits);
    }

    // 2+3. prefix + substring/token candidates together.
    for k in &keys {
        if k.text.starts_with(&norm) {
            collect(&mut hits, &mut seen, k);
        }
    }
    for k in &keys {
        if k.text.contains(&norm) {
            collect(&mut hits, &mut seen, k);
        }
    }
    if !hits.is_empty() {
        return Ok(hits);
    }

    // 4. Levenshtein <= 2 fallback - keep only the closest distance
    // (prevents a 1-edit typo of a filtered package from falling back to a
    // 2-edit typo of an unrelated package and leaking its existence via
    // game-filtered `which`).
    let mut best: Option<usize> = None;
    let mut cands: Vec<&Key<'_>> = Vec::new();

    for k in &keys {
        let d = levenshtein(&norm, &k.text);
        if d <= 2 {
            match best {
                None => {
                    best = Some(d);
                    cands = vec![k];
                }
                Some(b) if d < b => {
                    best = Some(d);
                    cands = vec![k];
                }
                Some(b) if d == b => cands.push(k),
                _ => {}
            }
        }
    }

    for k in cands {
        collect(&mut hits, &mut seen, k);
    }

    if !hits.is_empty() {
        return Ok(hits);
    }

    anyhow::bail!("unknown package '{}'", input)
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
