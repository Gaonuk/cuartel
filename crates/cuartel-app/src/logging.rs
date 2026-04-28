//! File-based logging for cuartel-app.
//!
//! Writes a daily-rolling log to `~/Library/Logs/Cuartel/cuartel.log`
//! (the macOS convention; visible in `Console.app` under "Log Reports"
//! → user's library). Bridges the existing `log::*` calls into
//! `tracing` so we don't have to rewrite every call site.
//!
//! Console output is intentionally suppressed: the goal is for session
//! lifecycle / ACP boot / state transitions to be invisible to the user
//! by default. Set `CUARTEL_LOG_STDERR=1` (or `RUST_LOG=...`) to mirror
//! to stderr while debugging.
//!
//! Returns a `WorkerGuard` that must be kept alive for the lifetime of
//! the process — dropping it stops the background writer thread and
//! truncates buffered log lines.

use std::path::PathBuf;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// Standard macOS user-log path. `~/Library/Logs/Cuartel/`.
fn log_dir() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join("Library/Logs/Cuartel"))
        .unwrap_or_else(|| PathBuf::from("/tmp/cuartel-logs"))
}

pub fn init() -> Option<WorkerGuard> {
    let dir = log_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!(
            "cuartel: could not create log dir {}: {e}; falling back to stderr-only",
            dir.display()
        );
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer().with_writer(std::io::stderr))
            .init();
        return None;
    }

    let file_appender = RollingFileAppender::new(Rotation::DAILY, &dir, "cuartel.log");
    let (file_writer, guard) = tracing_appender::non_blocking(file_appender);

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,cuartel=debug,cuartel_app=debug"));

    let file_layer = fmt::layer()
        .with_writer(file_writer)
        .with_ansi(false)
        .with_target(true)
        .with_thread_names(false)
        .with_file(false)
        .with_line_number(false);

    let registry = tracing_subscriber::registry().with(filter).with(file_layer);

    if std::env::var("CUARTEL_LOG_STDERR").is_ok() {
        registry
            .with(fmt::layer().with_writer(std::io::stderr))
            .init();
    } else {
        registry.init();
    }

    tracing::info!(
        log_dir = %dir.display(),
        "cuartel logging initialized (file sink, daily rotation)"
    );
    Some(guard)
}
