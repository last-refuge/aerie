//! Local, redacted logging. Logs stay on this computer under
//! `$XDG_STATE_HOME/aerie/logs`, rotate daily and keep a week. Nothing is uploaded.

use std::path::PathBuf;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{Builder, Rotation};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::writer::MakeWriterExt;

/// Starts logging; keep the guard alive for the life of the app so logs are flushed.
/// `AERIE_LOG` overrides the level, for example `AERIE_LOG=debug`.
pub fn init() -> Option<WorkerGuard> {
    let filter = EnvFilter::try_from_env("AERIE_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let appender = Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix("aerie")
        .filename_suffix("log")
        .max_log_files(7)
        .build(log_dir());
    let Ok(appender) = appender else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .init();
        return None;
    };
    let (file, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(file.and(std::io::stderr))
        .init();
    Some(guard)
}

pub fn log_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_default()
        .join("aerie")
        .join("logs")
}
