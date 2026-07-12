//! Default-stub bookkeeping for any generated long-tail entry point.
//!
//! A stub keeps the ABI intact (the app links and runs) but does no work and returns a benign default.
//! To make the long tail *visible*, each stub reports itself once when `DD_SHIM_DEBUG` is set.
//! Identical mechanism to dd-shim-cuda/-gl's `stub.rs`. The whole runtime surface is hand-implemented,
//! so in practice no generated stub calls this — it exists so the generator stays uniform with the
//! driver shim and any future surface addition traces cleanly until ported.

use std::sync::Mutex;
use std::sync::OnceLock;

fn seen() -> &'static Mutex<std::collections::HashSet<&'static str>> {
    static S: OnceLock<Mutex<std::collections::HashSet<&'static str>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// Called by every generated stub. First hit of each name (when `DD_SHIM_DEBUG` is set) logs
/// "unimplemented entry point"; subsequent hits are silent. Cheap and thread-safe.
#[inline]
pub fn hit(name: &'static str) {
    if std::env::var_os("DD_SHIM_DEBUG").is_none() {
        return;
    }
    if let Ok(mut s) = seen().lock() {
        if s.insert(name) {
            eprintln!("[dd-shim-cudart] unimplemented entry point: {name} (default stub)");
        }
    }
}
