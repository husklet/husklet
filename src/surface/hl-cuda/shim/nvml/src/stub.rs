//! Default-stub bookkeeping for the generated `nvml*` surface (mirrors the cuda/cudart shims').

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

/// First hit of each name (when `HL_SHIM_DEBUG` is set) logs; the rest are silent. Thread-safe.
impl Stub {
    #[inline]
    pub fn hit(name: &'static str) {
        if std::env::var_os("HL_SHIM_DEBUG").is_none() {
            return;
        }
        if Registry::record(name) {
            eprintln!("[hl-nvml-shim] unimplemented entry point: {name} (default stub)");
        }
    }
}
