//! Default-stub bookkeeping for the generated `vk*` long tail.
//!
//! [`Stub::hit`] gives every generated default stub a once-per-name "unimplemented entry point" trace under
//! `HL_SHIM_DEBUG`; [`Call::unsupported`] is the truthful-failure hook a hand-written body calls when it
//! reaches an operation the modeled lowering subset cannot represent (so it returns an accurate
//! `VkResult` instead of a false `VK_SUCCESS`). [`Failure::report`] is for failures whose symptom is
//! indistinguishable from success. Ported from `hl-shim-cuda/src/stub.rs`.
//!
//! All three use `eprintln!` rather than `hl_log`. That is deliberate and load-bearing: the guest cdylib
//! is built `--release` WITHOUT the `release-verbose` feature, so every `hl_warn!`/`hl_info!` in this
//! crate expands to `{}` in the SHIPPED driver. Any diagnostic that must survive the release build has
//! to bypass those macros.

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

pub struct Failure;

/// Report a failure an application would otherwise experience as silence.
///
/// Unlike [`Stub::hit`] and [`Call::unsupported`] this is **not** gated on `HL_SHIM_DEBUG`. It is
/// reserved for the narrow class of failures that are indistinguishable from success from the outside —
/// a present that commits nothing, a driver that cannot reach a window — where staying quiet has cost
/// hours of diagnosis. Reported once per `cmd` per process, so a 60 Hz present loop prints one line and
/// not sixty.
impl Failure {
    pub fn report(cmd: &'static str, detail: &str) {
        if Registry::record(cmd) {
            eprintln!("[hl-vulkan-shim] {cmd}: {detail}");
        }
    }
}
