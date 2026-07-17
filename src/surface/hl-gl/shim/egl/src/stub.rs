//! Default-stub bookkeeping for the generated `egl*`/`gl*` long tail.
//!
//! [`hit`] gives every generated default stub a once-per-name "unimplemented entry point" trace under
//! `HL_SHIM_DEBUG`; [`unsupported`] is the truthful-failure hook a hand-written body calls when it
//! reaches an operation the modeled GL/IR subset cannot represent (so it records an accurate GL/EGL error
//! instead of a false success). Mirrors `hl-cuda/shim/cuda/src/stub.rs`.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

fn seen() -> &'static Mutex<HashSet<&'static str>> {
    static S: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Called by every generated stub. First hit of each name (when `HL_SHIM_DEBUG` is set) logs; the rest
/// are silent. Cheap and thread-safe.
#[inline]
pub fn hit(name: &'static str) {
    if std::env::var_os("HL_SHIM_DEBUG").is_none() {
        return;
    }
    if let Ok(mut s) = seen().lock() {
        if s.insert(name) {
            eprintln!("[hl-gl-shim] unimplemented entry point: {name} (default stub)");
        }
    }
}

/// Report an unsupported GL/EGL operation the hand-written body could not honestly execute. Once-logs it
/// under `HL_SHIM_DEBUG`. The caller records the accurate GL/EGL error itself.
pub fn unsupported(cmd: &'static str, detail: &str) {
    if std::env::var_os("HL_SHIM_DEBUG").is_some() {
        if let Ok(mut s) = seen().lock() {
            if s.insert(cmd) {
                eprintln!("[hl-gl-shim] unsupported GL/EGL operation: {cmd} ({detail})");
            }
        }
    }
}

/// Once-log a successfully implemented entry point while diagnosing a real loader. This is intentionally
/// behind the same explicit debug gate as stub reporting and is silent in normal guest processes.
pub fn trace(cmd: &'static str, detail: &str) {
    if std::env::var_os("HL_SHIM_DEBUG").is_some() {
        if let Ok(mut s) = seen().lock() {
            if s.insert(cmd) {
                eprintln!("[hl-gl-shim] {cmd}: {detail}");
            }
        }
    }
}
