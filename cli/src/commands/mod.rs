pub mod add;
pub mod menu;
pub mod remove;
pub mod update;
pub mod upgrade;
pub mod which;

use crate::cli::Cmd;

/// Split one positional package argument into `(name, Option<version spec>)`
/// on the first `@` (`cleo@5.4.0` -> `("cleo", Some("5.4.0"))`). Pure input
/// shaping shared by every command that takes package arguments.
pub(crate) fn split_pkg_spec(input: &str) -> (&str, Option<&str>) {
    match input.split_once('@') {
        Some((n, s)) => (n, Some(s)),
        None => (input, None),
    }
}

pub fn dispatch(cmd: Cmd, json: bool) -> crate::Result<()> {
    match cmd {
        Cmd::Add { pkgs, dir, dry_run } => add::run(&pkgs, dir, dry_run, json),
        Cmd::Remove { pkgs, dir } => remove::run(&pkgs, dir, json),
        Cmd::Menu { pkg, dir, refresh } => menu::run(pkg.as_deref(), dir, json, refresh),
        Cmd::Which { pkg, dir } => which::run(pkg.as_deref(), dir, json),
        Cmd::Update { pkg, dir, dry_run } => update::run(pkg.as_deref(), dir, dry_run, json),
        Cmd::Upgrade { check } => upgrade::run(check, json),
    }
}
