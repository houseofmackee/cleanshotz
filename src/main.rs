// First, so its `t!` macro is visible in every module below.
#[macro_use]
mod i18n;

mod action;
mod app;
mod capture;
mod cli;
mod command;
mod config;
mod desktop_icons;
mod editor;
mod history;
mod logging;
mod ocr;
mod pin;
mod pipeline;
mod platform;
mod quick_access;
mod record;
mod scroll;
mod settings;
mod shortcut;
mod state;
mod steps;
mod studio;
mod ui;
mod updates;
mod upload;
mod video_editor;
mod watermark;

use std::process::ExitCode;

const USAGE: &str = "\
CleanShotZ: screenshots, screen recordings and annotations for macOS

Usage:
  cleanshotz              Start the menu bar app
  cleanshotz --version    Print the version
  cleanshotz --help       Print this help
  cleanshotz secret set NAME
                          Store a secret (read from standard input) in the
                          Keychain, for settings written as keychain:NAME
  cleanshotz secret delete NAME
  cleanshotz upload FILE  Upload a file with the configured destination and
                          print its link
  cleanshotz COMMAND [--parameter value]...
                          Run a command in the app (see below)
";

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
            println!("{USAGE}\n{}", cli::COMMANDS_HELP);
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
        Some("--dev-studio-export") if args.len() == 3 => {
            match studio::export_sample(args[1].as_ref(), args[2].as_ref()) {
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
        Some("--dev-video-export") if args.len() == 8 => {
            use video_editor::plan::*;
            let source = std::path::Path::new(&args[1]);
            let size = record::media::size(source);
            let plan = Plan {
                start: args[3].parse().unwrap_or(0.0),
                end: args[4]
                    .parse()
                    .unwrap_or(f64::MAX)
                    .min(record::media::duration(source)),
                mute: args[5] == "mute",
                resolution: match args[6].as_str() {
                    "1080" => Resolution::P1080,
                    "720" => Resolution::P720,
                    "540" => Resolution::P540,
                    _ => Resolution::Original,
                },
                format: if args[7] == "gif" {
                    Format::Gif
                } else {
                    Format::Mp4
                },
                crop: std::env::var("CSZ_CROP").ok().and_then(|c| {
                    let v: Vec<f64> = c.split(',').filter_map(|n| n.parse().ok()).collect();
                    (v.len() == 4).then(|| capture::geometry::Rect::new(v[0], v[1], v[2], v[3]))
                }),
                speed: std::env::var("CSZ_SPEED")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(1.0),
                quality: std::env::var("CSZ_QUALITY")
                    .ok()
                    .and_then(|q| q.parse().ok()),
                watermark: std::env::var_os("CSZ_WATERMARK").is_some(),
            };
            // The watermark from the settings file.
            let overlay = if plan.watermark {
                let config = config::Config::load_or_create(&config::config_path());
                let style = config.map(|c| c.watermark.style).unwrap_or_default();
                match watermark::mark(&style) {
                    Ok(mark) => Some(watermark::Overlay {
                        mark,
                        style,
                        free: None,
                    }),
                    Err(e) => {
                        eprintln!("cleanshotz: {e}");
                        return ExitCode::FAILURE;
                    }
                }
            } else {
                None
            };
            let preset = preset(plan.resolution, size);
            let gif = (15, gif_width(plan.resolution, size, 1280));
            match video_editor::export::export(
                source,
                &plan,
                preset,
                gif,
                overlay.as_ref(),
                args[2].as_ref(),
            ) {
                Ok(()) => {
                    println!("exported with {preset:?}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("cleanshotz: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        #[cfg(debug_assertions)]
        Some("--dev-stitch") if args.len() >= 3 => {
            let mut scroller = scroll::stitch::Scroller::new(30_000);
            for path in &args[1..] {
                let Some((width, height, pixels)) =
                    capture::image::load(path.as_ref()).and_then(|i| capture::image::to_pixels(&i))
                else {
                    eprintln!("cleanshotz: cannot read {path}");
                    return ExitCode::FAILURE;
                };
                let push = scroller.push(scroll::stitch::Frame {
                    width,
                    height,
                    pixels,
                });
                println!(
                    "{path}: {push:?} axis={:?} size={:?}",
                    scroller.axis(),
                    scroller.size()
                );
            }
            ExitCode::SUCCESS
        }
        #[cfg(debug_assertions)]
        Some("--dev-mix-audio") if args.len() == 2 => {
            match record::media::mix_audio_tracks(args[1].as_ref()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("cleanshotz: {e}");
                    ExitCode::FAILURE
                }
            }
        }
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
        Some("secret") => secret_command(&args[1..]),
        Some("upload") if args.len() == 2 => upload_command(args[1].as_ref()),
        Some(command) if cli::is_command(command) => cli::run(&args),
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

fn secret_command(args: &[String]) -> ExitCode {
    let result = match args {
        [action, name] if action == "set" => {
            read_secret(name).and_then(|value| upload::keychain::set(name, &value))
        }
        [action, name] if action == "delete" => upload::keychain::delete(name),
        _ => Err(format!(
            "usage: cleanshotz secret set|delete NAME\n\n{USAGE}"
        )),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("cleanshotz: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Reads one line from standard input, without echoing it in a terminal.
fn read_secret(name: &str) -> Result<String, String> {
    use std::io::{BufRead, IsTerminal};
    let terminal = std::io::stdin().is_terminal();
    if terminal {
        eprint!("Value for {name}: ");
        let _ = std::process::Command::new("/bin/stty")
            .arg("-echo")
            .status();
    }
    let mut line = String::new();
    let read = std::io::stdin().lock().read_line(&mut line);
    if terminal {
        let _ = std::process::Command::new("/bin/stty").arg("echo").status();
        eprintln!();
    }
    read.map_err(|e| e.to_string())?;
    let value = line.trim_end_matches(['\r', '\n']).to_string();
    if value.is_empty() {
        return Err("the secret is empty".into());
    }
    Ok(value)
}

fn upload_command(path: &std::path::Path) -> ExitCode {
    let config = match config::Config::load_or_create(&config::config_path()) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("cleanshotz: {e}");
            return ExitCode::FAILURE;
        }
    };
    upload::configure(config.upload);
    match upload::upload_file(path) {
        Ok(link) => {
            println!("{link}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cleanshotz: {e}");
            ExitCode::FAILURE
        }
    }
}
