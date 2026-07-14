//! Default-stub bookkeeping + the truthful-failure / strict-mode machinery for the runtime surface.
//!
//! Two jobs:
//!   * [`hit`] — once-per-name "unimplemented entry point" tracing for any generated default stub (the
//!     whole runtime surface is hand-implemented, so in practice none call this; the hook stays for
//!     generator uniformity with the driver shim).
//!   * [`unsupported`] + [`note`] — the Phase-0 truthfulness path: when a hand-written entry point
//!     reaches an operation the IR / PTX executor cannot represent (an unsupported PTX instruction, an
//!     unmodeled CUDA feature), it returns the *accurate* `cudaError_t` (never a false `cudaSuccess`)
//!     and reports the unsupported call here. With `HL_SHIM_STRICT=1` the process aborts at the first
//!     such call with the command, object/context detail, and a recent-call history.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

fn seen() -> &'static Mutex<std::collections::HashSet<&'static str>> {
    static S: OnceLock<Mutex<std::collections::HashSet<&'static str>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// Called by every generated stub. First hit of each name (when `HL_SHIM_DEBUG` is set) logs
/// "unimplemented entry point"; subsequent hits are silent. Cheap and thread-safe.
#[inline]
pub fn hit(name: &'static str) {
    if std::env::var_os("HL_SHIM_DEBUG").is_none() {
        return;
    }
    if let Ok(mut s) = seen().lock() {
        if s.insert(name) {
            eprintln!("[dd-shim-cudart] unimplemented entry point: {name} (default stub)");
        }
    }
}

/// Recent-call history ring (most-recent-last), for the strict-mode abort report.
const HISTORY_CAP: usize = 32;
fn history() -> &'static Mutex<VecDeque<String>> {
    static H: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();
    H.get_or_init(|| Mutex::new(VecDeque::with_capacity(HISTORY_CAP)))
}

/// Record a significant call in the strict-mode history ring.
pub fn note(entry: impl Into<String>) {
    if let Ok(mut h) = history().lock() {
        if h.len() == HISTORY_CAP {
            h.pop_front();
        }
        h.push_back(entry.into());
    }
}

fn history_snapshot() -> Vec<String> {
    history().lock().map(|h| h.iter().cloned().collect()).unwrap_or_default()
}

/// `HL_SHIM_STRICT=1` (any value) is set — abort at the first unsupported CUDA call.
pub fn strict_enabled() -> bool {
    std::env::var_os("HL_SHIM_STRICT").is_some()
}

/// Build the strict-mode abort report (command + object/context detail + recent-call history).
/// Factored out so it is unit-testable without aborting the process.
pub fn strict_report(cmd: &str, detail: &str) -> String {
    let mut s = String::new();
    s.push_str("[dd-shim-cudart] STRICT: aborting at first unsupported CUDA call\n");
    s.push_str(&format!("  command: {cmd}\n"));
    s.push_str(&format!("  detail : {detail}\n"));
    let hist = history_snapshot();
    if hist.is_empty() {
        s.push_str("  history: (none)\n");
    } else {
        s.push_str("  history (oldest first):\n");
        for (i, h) in hist.iter().enumerate() {
            s.push_str(&format!("    {i:>2}. {h}\n"));
        }
    }
    s
}

// Test-only strict trip flag: in `cfg(test)` the strict path records that it *would* have aborted
// (instead of killing the test process) so the abort decision is assertable.
#[cfg(test)]
thread_local! {
    pub static STRICT_TRIPPED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(not(test))]
fn trigger_abort() -> ! {
    std::process::abort()
}

#[cfg(test)]
fn trigger_abort() {
    STRICT_TRIPPED.with(|c| c.set(true));
}

/// Report an unsupported CUDA operation: record it in the history ring, once-log it under
/// `HL_SHIM_DEBUG`, and — under `HL_SHIM_STRICT=1` — print the full report and abort the process. The
/// caller returns the accurate `cudaError_t` itself.
pub fn unsupported(cmd: &'static str, detail: &str) {
    note(format!("{cmd} -> UNSUPPORTED ({detail})"));
    if std::env::var_os("HL_SHIM_DEBUG").is_some() {
        if let Ok(mut s) = seen().lock() {
            if s.insert(cmd) {
                eprintln!("[dd-shim-cudart] unsupported CUDA operation: {cmd} ({detail})");
            }
        }
    }
    if strict_enabled() {
        eprint!("{}", strict_report(cmd, detail));
        trigger_abort();
    }
}
