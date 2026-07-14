//! Default-stub bookkeeping + the Phase-0 truthful-failure / strict-mode machinery for the generated
//! long-tail `vk*` entry points.
//!
//! A generated stub keeps the ABI intact (the loader resolves the symbol, the app runs) but has no
//! real body. Per Phase 0 (`docs/codex-rendering.md` §6, §2.2) it must **fail truthfully**, never
//! report false success: a `VkResult` stub returns the API-defined error (`VK_ERROR_FEATURE_NOT_PRESENT`
//! for an unimplemented core command, `VK_ERROR_EXTENSION_NOT_PRESENT` for a command from an
//! unadvertised extension — see `build.rs`), a create/allocate stub also nulls its output handle, and
//! a `void`/`VkBool32`/pointer stub returns the truthful zero/`VK_FALSE`/NULL. The exact error each
//! stub returns is recorded in the generated capability inventory (`crate::capability::CAPABILITIES`).
//!
//! Every stub call funnels through [`hit`], which:
//!   * records the call in a recent-history ring (for the strict-mode report),
//!   * once-per-name logs "unimplemented entry point" under `HL_SHIM_DEBUG` (exploratory runs),
//!   * and — under `HL_SHIM_STRICT=1` — prints command + object + recent history and aborts the
//!     process at the FIRST unsupported call, so an app run stops exactly where hl cannot honestly act
//!     instead of silently mis-executing.
//!
//! Identical mechanism to hl-shim-cuda / hl-shim-gl `stub.rs`.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

fn seen() -> &'static Mutex<std::collections::HashSet<&'static str>> {
    static S: OnceLock<Mutex<std::collections::HashSet<&'static str>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// Recent-call history ring (most-recent-last), for the strict-mode abort report.
const HISTORY_CAP: usize = 32;
fn history() -> &'static Mutex<VecDeque<&'static str>> {
    static H: OnceLock<Mutex<VecDeque<&'static str>>> = OnceLock::new();
    H.get_or_init(|| Mutex::new(VecDeque::with_capacity(HISTORY_CAP)))
}

/// Snapshot the history ring (oldest → newest).
fn history_snapshot() -> Vec<&'static str> {
    history().lock().map(|h| h.iter().copied().collect()).unwrap_or_default()
}

/// `HL_SHIM_STRICT=1` (any non-empty value) is set — abort at the first unsupported Vulkan call.
pub fn strict_enabled() -> bool {
    std::env::var_os("HL_SHIM_STRICT").is_some()
}

/// Build the strict-mode abort report: the command, its object/context detail, and the recent-call
/// history. Factored out so it is unit-testable without actually aborting the process.
pub fn strict_report(cmd: &str, detail: &str) -> String {
    let mut s = String::new();
    s.push_str("[hl-shim-vk] STRICT: aborting at first unsupported Vulkan call\n");
    s.push_str(&format!("  command: {cmd}\n"));
    s.push_str(&format!("  object : {detail}\n"));
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
fn trigger_abort(cmd: &str) -> ! {
    eprint!("{}", strict_report(cmd, "generated stub — no real body"));
    std::process::abort()
}

#[cfg(test)]
fn trigger_abort(_cmd: &str) {
    STRICT_TRIPPED.with(|c| c.set(true));
}

/// Called by every generated stub. Records the call in the strict-mode history ring; first hit of each
/// name (when `HL_SHIM_DEBUG` is set) logs "unimplemented entry point"; and under `HL_SHIM_STRICT=1`
/// aborts the process at this first unsupported call (Phase-0 strict gate). The stub itself returns the
/// truthful API error (chosen by `build.rs` from the command's return type + origin). Cheap when off.
#[inline]
pub fn hit(name: &'static str) {
    if let Ok(mut h) = history().lock() {
        if h.len() == HISTORY_CAP {
            h.pop_front();
        }
        h.push_back(name);
    }
    if std::env::var_os("HL_SHIM_DEBUG").is_some() {
        if let Ok(mut s) = seen().lock() {
            if s.insert(name) {
                eprintln!("[hl-shim-vk] unimplemented entry point: {name} (default stub)");
            }
        }
    }
    if strict_enabled() {
        trigger_abort(name);
    }
}
