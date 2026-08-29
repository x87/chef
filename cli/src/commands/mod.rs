pub mod add;
pub mod menu;
pub mod remove;
pub mod update;
pub mod upgrade;
pub mod which;

use crate::cli::Cmd;

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
