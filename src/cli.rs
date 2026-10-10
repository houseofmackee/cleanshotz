//! `cleanshotz <command> [--parameter value]...`: sends a command to the
//! running app over its socket, starting the app first if needed.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use crate::app::server::socket_path;
use crate::command;

pub const COMMANDS_HELP: &str = "\
Commands (sent to the running app, which is started if needed):
  capture-area [--x X --y Y --width W --height H] [--display N] [--action A]
                          Select an area, or capture the given one at once
  capture-previous-area [--action A]
  capture-fullscreen [--action A]
  capture-window [--action A]
  self-timer [--action A]
  scrolling-capture [--x X --y Y --width W --height H] [--display N]
                    [--autoscroll]
  all-in-one [--x X --y Y --width W --height H] [--display N]
                          Same as capture-area
  capture-text [--filepath FILE | --x X --y Y --width W --height H]
               [--display N] [--linebreaks true|false]
  record-screen [--x X --y Y --width W --height H] [--display N]
                          Start (or stop) a recording
  pin [--filepath FILE]   Pin an image (asks for one without --filepath)
  open-annotate [--filepath FILE]
  open-from-clipboard
  add-quick-access-overlay --filepath FILE
  hide-desktop-icons | show-desktop-icons | toggle-desktop-icons
  open-history
  restore-recently-closed
  open-settings [--tab TAB]
  quit

  --action is copy, save, annotate, upload or pin: what to do with the
  screenshot instead of the usual copy and save settings.
  Areas are in points from the bottom-left corner of the display; display 1
  is the main display, and without --display the one under the pointer.";

pub fn is_command(name: &str) -> bool {
    command::NAMES.contains(&name)
}

pub fn run(args: &[String]) -> ExitCode {
    match send(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("cleanshotz: {e}");
            ExitCode::FAILURE
        }
    }
}

fn send(args: &[String]) -> Result<(), String> {
    let (name, rest) = args.split_first().ok_or("no command given")?;
    let params = parameters(rest)?;
    // Check here first, for clear errors even when the app is not running.
    command::parse(name, &params)?;
    let url = command::to_url(name, &params);
    let mut stream = connect()?;
    stream
        .write_all(format!("{url}\n").as_bytes())
        .map_err(|e| format!("could not send the command: {e}"))?;
    let mut reply = String::new();
    BufReader::new(&stream)
        .read_line(&mut reply)
        .map_err(|e| format!("no answer from CleanShotZ: {e}"))?;
    match reply.trim_end() {
        "ok" => Ok(()),
        "" => Err("CleanShotZ closed the connection without answering".into()),
        other => Err(other.strip_prefix("error: ").unwrap_or(other).to_string()),
    }
}

/// `--name value`, `--name=value`, or `--name` alone for `true`. File
/// paths are made absolute against the current directory.
fn parameters(args: &[String]) -> Result<Vec<(String, String)>, String> {
    let mut params = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let name = arg
            .strip_prefix("--")
            .filter(|n| !n.is_empty())
            .ok_or_else(|| format!("expected --parameter, got {arg:?}"))?;
        let (name, value) = match name.split_once('=') {
            Some((n, v)) => (n.to_string(), v.to_string()),
            None => match args.get(i + 1).filter(|v| !v.starts_with("--")) {
                Some(value) => {
                    i += 1;
                    (name.to_string(), value.clone())
                }
                None => (name.to_string(), "true".to_string()),
            },
        };
        let value = if name == "filepath" {
            absolute(&value)
        } else {
            value
        };
        params.push((name, value));
        i += 1;
    }
    Ok(params)
}

fn absolute(path: &str) -> String {
    let expanded = crate::capture::naming::expand_home(path);
    if expanded.is_absolute() {
        return expanded.to_string_lossy().into_owned();
    }
    std::env::current_dir()
        .map(|dir| dir.join(&expanded))
        .unwrap_or(expanded)
        .to_string_lossy()
        .into_owned()
}

fn connect() -> Result<UnixStream, String> {
    let path = socket_path();
    if let Ok(stream) = UnixStream::connect(&path) {
        return Ok(stream);
    }
    let bundle = app_bundle().ok_or(
        "CleanShotZ is not running. Start it first (this copy of the command is not inside the app).",
    )?;
    let status = std::process::Command::new("/usr/bin/open")
        .args(["-g", "-a"])
        .arg(&bundle)
        .status()
        .map_err(|e| format!("could not start CleanShotZ: {e}"))?;
    if !status.success() {
        return Err(format!("could not start {}", bundle.display()));
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        if let Ok(stream) = UnixStream::connect(&path) {
            return Ok(stream);
        }
    }
    Err("CleanShotZ started but is not answering commands".into())
}

/// The `.app` this executable is in (following symlinks such as one in
/// /usr/local/bin).
fn app_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    exe.ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn reads_parameters() {
        let params = parameters(&strings(&[
            "--x",
            "10",
            "--width=300",
            "--autoscroll",
            "--action",
            "pin",
        ]))
        .unwrap();
        let expected: Vec<(String, String)> = [
            ("x", "10"),
            ("width", "300"),
            ("autoscroll", "true"),
            ("action", "pin"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        assert_eq!(params, expected);
        assert!(parameters(&strings(&["x", "10"])).is_err());
        let file = parameters(&strings(&["--filepath", "a.png"])).unwrap();
        assert!(Path::new(&file[0].1).is_absolute());
    }
}
