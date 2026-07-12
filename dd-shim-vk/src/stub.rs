//! Default-stub bookkeeping for the generated long-tail `vk*` entry points.
//!
//! A stub keeps the ABI intact (the loader resolves the symbol, the app runs) but does no work and
//! returns `VK_SUCCESS`. To make the long tail *visible* (so we know which entry points a given
//! Vulkan app actually calls, and in what order, to prioritize porting), each stub reports itself
//! once when `DD_SHIM_DEBUG` is set. Identical mechanism to dd-shim-gl / dd-shim-cuda `stub.rs`.

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
            eprintln!("[dd-shim-vk] unimplemented entry point: {name} (default stub)");
        }
    }
}
