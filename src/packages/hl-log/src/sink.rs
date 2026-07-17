//! Output sink: where formatted log lines go.
//!
//! The default sink writes to a locked stderr in a single `write_all`. Apps and
//! tests can swap the sink via [`set_sink`] (e.g. a `TestSink` that collects lines,
//! or an app router that fans out to a file + terminal).

use std::io::Write;
use std::sync::{Mutex, OnceLock};

/// A destination for fully-formatted log lines. Implementations must be cheap and
/// must not themselves log (no re-entrancy).
pub trait Sink: Send + Sync {
    /// Write one already-formatted line (it already ends in `\n`).
    fn write_line(&self, s: &str);
}

/// The default sink: a single locked-stderr `write_all`.
pub struct StderrSink;

impl Sink for StderrSink {
    fn write_line(&self, s: &str) {
        let stderr = std::io::stderr();
        let mut lock = stderr.lock();
        // Best-effort: logging never propagates I/O errors to the caller.
        let _ = lock.write_all(s.as_bytes());
    }
}

/// The active sink. Boxed so it can be swapped at runtime. `OnceLock<Mutex<..>>`
/// gives us a lazily-created, swappable slot without external deps.
static SINK: OnceLock<Mutex<Box<dyn Sink>>> = OnceLock::new();

fn slot() -> &'static Mutex<Box<dyn Sink>> {
    SINK.get_or_init(|| Mutex::new(Box::new(StderrSink)))
}

/// Replace the active sink. Used by tests (collect lines) and the app (route logs).
pub fn set_sink(sink: Box<dyn Sink>) {
    *slot().lock().unwrap_or_else(|e| e.into_inner()) = sink;
}

/// Restore the default stderr sink.
pub fn reset_sink() {
    set_sink(Box::new(StderrSink));
}

/// Hand a formatted line to the active sink.
pub(crate) fn write_line(s: &str) {
    slot()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .write_line(s);
}
