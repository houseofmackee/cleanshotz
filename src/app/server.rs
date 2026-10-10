//! A Unix socket next to the settings file. The `cleanshotz` command line
//! sends one `cleanshotz://` URL per connection and gets back `ok` or
//! `error: <message>`. Only the user's own processes can connect (the
//! socket is mode 0600), the same processes that can already open URLs.

use std::cell::OnceCell;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use dispatch2::DispatchQueue;
use objc2::MainThreadMarker;

type Handler = Box<dyn Fn(&str) -> Result<(), String>>;

thread_local! {
    static HANDLER: OnceCell<Handler> = const { OnceCell::new() };
}

/// Longest request accepted (URLs with long file paths fit easily).
const MAX_REQUEST: u64 = 64 * 1024;

pub fn socket_path() -> PathBuf {
    let config = crate::config::config_path();
    config
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
        .join("cleanshotz.sock")
}

/// Starts listening. `handler` runs on the main thread for each request.
pub fn start(_mtm: MainThreadMarker, handler: impl Fn(&str) -> Result<(), String> + 'static) {
    let path = socket_path();
    if UnixStream::connect(&path).is_ok() {
        tracing::warn!(path = %path.display(), "another CleanShotZ is answering commands; not listening");
        return;
    }
    // A socket left behind by an instance that did not quit cleanly.
    let _ = std::fs::remove_file(&path);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let listener = match UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(e) => {
            tracing::error!(error = %e, path = %path.display(), "could not listen for commands");
            return;
        }
    };
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    HANDLER.with(|h| {
        let _ = h.set(Box::new(handler));
    });
    tracing::info!(path = %path.display(), "listening for commands");
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || serve(stream));
        }
    });
}

/// Removes the socket when the app quits.
pub fn stop() {
    let path = socket_path();
    // Only our own socket: a second instance never bound one.
    if HANDLER.with(|h| h.get().is_some()) {
        let _ = std::fs::remove_file(path);
    }
}

fn serve(stream: UnixStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut line = String::new();
    let read = BufReader::new((&stream).take(MAX_REQUEST)).read_line(&mut line);
    let reply = match read {
        Ok(_) if line.ends_with('\n') => {
            let url = line.trim().to_string();
            let mut result = Err("CleanShotZ did not answer".to_string());
            DispatchQueue::main().exec_sync(|| {
                result = HANDLER.with(|h| match h.get() {
                    Some(handler) => handler(&url),
                    None => Err("CleanShotZ is not ready".into()),
                });
            });
            match result {
                Ok(()) => "ok\n".to_string(),
                Err(e) => format!("error: {}\n", e.replace('\n', " ")),
            }
        }
        Ok(_) => "error: the request was too long or incomplete\n".to_string(),
        Err(e) => format!("error: {e}\n"),
    };
    let _ = (&stream).write_all(reply.as_bytes());
}
