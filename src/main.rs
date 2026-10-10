mod action;
mod app;
mod capture;
mod config;
mod editor;
mod logging;
mod ocr;
mod pin;
mod platform;
mod quick_access;
mod record;
mod shortcut;
mod state;
mod ui;

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
        #[cfg(debug_assertions)]
        Some("--dev-render-sample") if args.len() == 3 => {
            match editor::render_sample(args[1].as_ref(), args[2].as_ref()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("cleanshotz: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        #[cfg(debug_assertions)]
        Some("--dev-ocr-sample") if args.len() == 2 => match editor::ocr_sample(args[1].as_ref()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("cleanshotz: {e}");
                ExitCode::FAILURE
            }
        },
        #[cfg(debug_assertions)]
        Some("--dev-gif") if args.len() == 3 => {
            match record::media::to_gif(args[1].as_ref(), args[2].as_ref(), 15, 1280) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("cleanshotz: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        #[cfg(debug_assertions)]
        Some("--dev-record-sample") if args.len() >= 3 => {
            let seconds: f64 = args[2].parse().unwrap_or(3.0);
            let audio = args.get(3).is_some_and(|a| a == "audio");
            match record::record_sample(args[1].as_ref(), seconds, audio) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("cleanshotz: {e}");
                    ExitCode::FAILURE
                }
            }
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
