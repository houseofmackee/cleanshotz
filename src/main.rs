mod action;
mod app;
mod capture;
mod config;
mod logging;
mod platform;
mod shortcut;
mod state;

use std::process::ExitCode;

const USAGE: &str = "\
CleanShotZ: screenshots, screen recordings and annotations for macOS

Usage:
  cleanshotz              Start the menu bar app
  cleanshotz --version    Print the version
  cleanshotz --help       Print this help

Command-line capture commands are planned for milestone M9 (see PLAN.md).";

fn main() -> ExitCode {
    // Launch Services may pass a `-psn_*` process serial number argument.
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with("-psn_"))
        .collect();

    match args.first().map(String::as_str) {
        None => run_app(),
        Some("--version" | "-V") => {
            println!("cleanshotz {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--help" | "-h") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("cleanshotz: unknown argument {other:?}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn run_app() -> ExitCode {
    let _log_guard = logging::init(&config::log_dir());
    let config_path = config::config_path();
    let config = match config::Config::load_or_create(&config_path) {
        Ok(config) => config,
        Err(e) => {
            // Keep running with defaults; the file is left untouched so the
            // user can fix it and choose Reload Settings.
            tracing::error!(error = %e, "using default settings");
            config::Config::default()
        }
    };
    app::run(config, config_path);
    ExitCode::SUCCESS
}
