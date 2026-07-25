//! Output sink: where formatted log lines go.
//!
//! The default sink writes to a locked stderr in a single `write_all`. Apps and
//! tests can swap the sink via [`Output::set`] (e.g. a `TestSink` that collects lines,
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

/// Synchronized access to the active sink.
pub struct Output {
    sink: Mutex<Box<dyn Sink>>,
}

impl Output {
    fn new() -> Self {
        Self {
            sink: Mutex::new(Box::new(StderrSink)),
        }
    }

    pub fn set(&self, sink: Box<dyn Sink>) {
        *self.sink.lock().unwrap_or_else(|error| error.into_inner()) = sink;
    }

    pub fn reset(&self) {
        self.set(Box::new(StderrSink));
    }

    pub(crate) fn write(&self, line: &str) {
        self.sink
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .write_line(line);
    }

    /// Returns the process-wide log output.
    pub fn global() -> &'static Self {
        static OUTPUT: OnceLock<Output> = OnceLock::new();
        OUTPUT.get_or_init(Self::new)
    }
}
