//! Default-stub bookkeeping for the generated `vk*` long tail.
//!
//! [`Stub::hit`] gives every generated default stub a once-per-name "unimplemented entry point" trace under
//! `HL_SHIM_DEBUG`; [`Call::unsupported`] is the truthful-failure hook a hand-written body calls when it
//! reaches an operation the modeled lowering subset cannot represent (so it returns an accurate
//! `VkResult` instead of a false `VK_SUCCESS`). Ported from `hl-shim-cuda/src/stub.rs`.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

struct Registry;

impl Registry {
    fn record(name: &'static str) -> bool {
        static SEEN: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
        SEEN.get_or_init(|| Mutex::new(HashSet::new()))
            .lock()
            .is_ok_and(|mut seen| seen.insert(name))
    }
}

pub struct Stub;

/// Called by every generated stub. First hit of each name (when `HL_SHIM_DEBUG` is set) logs; the rest
/// are silent. Cheap and thread-safe.
impl Stub {
    #[inline]
    pub fn hit(name: &'static str) {
        if std::env::var_os("HL_SHIM_DEBUG").is_none() {
            return;
        }
        if Registry::record(name) {
            eprintln!("[hl-vulkan-shim] unimplemented entry point: {name} (default stub)");
        }
    }
}

pub struct Call;

/// Report an unsupported Vulkan operation the hand-written body could not honestly execute. Once-logs
/// it under `HL_SHIM_DEBUG`. The caller returns the accurate `VkResult` itself.
impl Call {
    pub fn unsupported(cmd: &'static str, detail: &str) {
        if std::env::var_os("HL_SHIM_DEBUG").is_none() {
            return;
        }
        if Registry::record(cmd) {
            eprintln!("[hl-vulkan-shim] unsupported Vulkan operation: {cmd} ({detail})");
        }
    }
}
