//! Default-stub bookkeeping for the generated `nvml*` surface (mirrors the cuda/cudart shims').

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

fn seen() -> &'static Mutex<HashSet<&'static str>> {
    static S: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashSet::new()))
}

/// First hit of each name (when `HL_SHIM_DEBUG` is set) logs; the rest are silent. Thread-safe.
#[inline]
pub fn hit(name: &'static str) {
    if std::env::var_os("HL_SHIM_DEBUG").is_none() {
        return;
    }
    if let Ok(mut s) = seen().lock() {
        if s.insert(name) {
            eprintln!("[hl-nvml-shim] unimplemented entry point: {name} (default stub)");
        }
    }
}
