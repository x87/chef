//! Command handlers: the meat behind each `chef` subcommand - package
//! resolution, deployment planning and execution, dependency handling,
//! version matching, install removal, upgrade fetching. Command modules
//! (`crate::commands`) only read and validate inputs, then delegate here.

pub mod add;
pub mod menu;
pub mod remove;
pub mod update;
pub mod upgrade;
pub mod which;
