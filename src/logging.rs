use std::path::Path;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

/// Logs to stderr and to `<dir>/cleanshotz.log`. The level is controlled by
/// `RUST_LOG` (default `info`). Keep the returned guard alive so buffered
/// lines are flushed on exit.
pub fn init(dir: &Path) -> Option<WorkerGuard> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let stderr = fmt::layer().with_writer(std::io::stderr);

    let (file, guard) = match std::fs::create_dir_all(dir) {
        Ok(()) => {
            let (writer, guard) = tracing_appender::non_blocking(tracing_appender::rolling::never(
                dir,
                "cleanshotz.log",
            ));
            (
                Some(fmt::layer().with_ansi(false).with_writer(writer)),
                Some(guard),
            )
        }
        Err(e) => {
            eprintln!(
                "cleanshotz: cannot create log directory {}: {e}",
                dir.display()
            );
            (None, None)
        }
    };

    tracing_subscriber::registry()
        .with(filter)
        .with(stderr)
        .with(file)
        .init();
    guard
}
