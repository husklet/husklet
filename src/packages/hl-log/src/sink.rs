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
        *self.sink.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = sink;
    }

    pub fn reset(&self) {
        self.set(Box::new(StderrSink));
    }

    pub(crate) fn write(&self, line: &str) {
        self.sink
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .write_line(line);
    }

    /// Returns the process-wide log output.
    pub fn global() -> &'static Self {
        static OUTPUT: OnceLock<Output> = OnceLock::new();
        OUTPUT.get_or_init(Self::new)
    }
}

/// A sink that drops everything. The default for the events channel.
pub struct DiscardSink;

impl Sink for DiscardSink {
    fn write_line(&self, _: &str) {}
}

/// The structured-event channel, separate from [`Output`] because the two have different readers.
///
/// It defaults to DISCARD rather than to stderr, and that is the whole reason it can exist beside the
/// human log without making it worse: a machine-readable stream interleaved into a human one helps
/// neither reader. An application turns it on by pointing it somewhere — a file, a socket, a test
/// collector — and until it does, an `hl_event!` costs its gate and nothing else.
///
/// A verdict is not silent when this is discarding. It writes its human sentence to [`Output`]
/// unconditionally; the record here is the machine-readable copy for whoever asked for one.
pub struct Events {
    sink: Mutex<Box<dyn Sink>>,
}

impl Events {
    fn new() -> Self {
        Self {
            sink: Mutex::new(Box::new(DiscardSink)),
        }
    }

    pub fn set(&self, sink: Box<dyn Sink>) {
        *self.sink.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = sink;
    }

    pub fn reset(&self) {
        self.set(Box::new(DiscardSink));
    }

    pub(crate) fn write(&self, line: &str) {
        self.sink
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .write_line(line);
    }

    /// Returns the process-wide event channel.
    pub fn global() -> &'static Self {
        static EVENTS: OnceLock<Events> = OnceLock::new();
        EVENTS.get_or_init(Self::new)
    }
}
