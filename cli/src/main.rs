use chef::cli::Cli;
use clap::Parser;

fn main() {
    chef::commands::upgrade::cleanup_old_binary();
    let cli = Cli::parse();
    // In JSON mode no human chatter may touch stdout or stderr: the result
    // document and any error object are the only output. The history log
    // below always records the run.
    init_logger(cli.json);
    // The invocation lands only in the history log, never on the console.
    log::debug!(
        "==== chef {} ====",
        std::env::args().skip(1).collect::<Vec<_>>().join(" ")
    );
    match chef::run_mode(cli.cmd, cli.json) {
        Ok(()) => {}
        Err(e) => std::process::exit(chef::write_error(&e, cli.json)),
    }
}

// Console logger mirroring the redux setup: no level padding, local
// times, no thread prefix; errors to stderr, everything else to stdout.
// JSON mode silences the console entirely - every command prints its own
// document. Every session also appends to `history.log` under the data
// home, so run operations and messages are available for debugging.
fn init_logger(json: bool) {
    let config = simplelog::ConfigBuilder::new()
        .set_level_padding(simplelog::LevelPadding::Off)
        .set_time_to_local(true)
        .set_thread_level(simplelog::LevelFilter::Off)
        .set_target_level(simplelog::LevelFilter::Off)
        .build();
    let console = if json {
        simplelog::LevelFilter::Off
    } else {
        simplelog::LevelFilter::Info
    };
    let mut loggers: Vec<Box<dyn simplelog::SharedLogger>> = Vec::new();
    loggers.push(simplelog::TermLogger::new(
        console,
        config.clone(),
        simplelog::TerminalMode::Mixed,
        simplelog::ColorChoice::Auto,
    ));
    if let Some(file) = chef::open_history_log() {
        // The file records every message, including the per-run invocation
        // line logged at Debug; the console stays at Info.
        loggers.push(simplelog::WriteLogger::new(
            simplelog::LevelFilter::Debug,
            config,
            file,
        ));
    }
    let _ = simplelog::CombinedLogger::init(loggers);
}
