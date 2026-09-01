use chef::cli::Cli;
use clap::Parser;

fn main() {
    cleanup_old_binary();
    let cli = Cli::parse();
    init_logger(cli.json);

    log::debug!(
        "==== chef {} ====",
        std::env::args().skip(1).collect::<Vec<_>>().join(" ")
    );

    match chef::run_mode(cli.cmd, cli.json) {
        Ok(()) => {}
        Err(e) => std::process::exit(chef::emit::write_error(&e, cli.json)),
    }
}

fn init_logger(json: bool) {
    use simplelog::*;

    let config = ConfigBuilder::new()
        .set_level_padding(LevelPadding::Off)
        .set_time_to_local(true)
        .set_thread_level(LevelFilter::Off)
        .set_target_level(LevelFilter::Off)
        .build();

    let console = if json {
        // In JSON mode the result and any error object are the only output.
        LevelFilter::Off
    } else {
        LevelFilter::Info
    };

    let mut loggers: Vec<Box<dyn simplelog::SharedLogger>> = Vec::new();
    loggers.push(TermLogger::new(
        console,
        config.clone(),
        TerminalMode::Mixed,
        ColorChoice::Auto,
    ));

    // Every session also appends to `history.log` under the chef home folder
    if let Some(file) = chef::open_history_log() {
        // The file records every message, including debug; the console stays at Info.
        loggers.push(WriteLogger::new(LevelFilter::Debug, config, file));
    }
    let _ = CombinedLogger::init(loggers);
}

/// Remove a leftover `.old` binary from a previous upgrade. Best-effort.
pub fn cleanup_old_binary() {
    if let Ok(me) = std::env::current_exe() {
        let old = me.with_extension("exe.old");
        if old.exists() {
            let _ = std::fs::remove_file(old);
        }
    }
}
