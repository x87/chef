use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "chef", version, about)]
pub struct Cli {
    /// Emit JSON (the result, and errors) for scripted invocations.
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Install packages into a game directory.
    Add {
        /// One or more packages, each with an optional version spec:
        /// cleo@latest sal, cleo@5, cleo@v4.4.4, universal-asi-loader.
        #[arg(required = true)]
        pkgs: Vec<String>,
        /// Game directory (defaults to the current directory).
        #[arg(long)]
        dir: Option<std::path::PathBuf>,
        /// Resolve and print the plan without touching anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove installed packages.
    Remove {
        /// One or more packages, optionally with an exact version:
        /// cleo sal, cleo-redux@1.5.0.
        #[arg(required = true)]
        pkgs: Vec<String>,
        #[arg(long)]
        dir: Option<std::path::PathBuf>,
    },
    /// Show available packages and versions.
    Menu {
        /// Restrict output to one package.
        pkg: Option<String>,
        /// Game directory (defaults to the current directory).
        #[arg(long)]
        dir: Option<std::path::PathBuf>,
        /// Force re-fetch of the package catalog + digest lock.
        #[arg(long)]
        refresh: bool,
    },
    /// Report what is installed in a game directory.
    Which {
        /// Restrict to one package: name (file tree of every release) or name@version.
        pkg: Option<String>,
        #[arg(long)]
        dir: Option<std::path::PathBuf>,
    },
    /// Update installed packages to the newest stable release.
    Update {
        /// Restrict to one package (default: every installed package).
        pkg: Option<String>,
        #[arg(long)]
        dir: Option<std::path::PathBuf>,
        /// Report what would update without touching anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Update the chef binary itself.
    Upgrade {
        /// Only report whether an update exists.
        #[arg(long)]
        check: bool,
    },
}
