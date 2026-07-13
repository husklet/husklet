//! Runtime support for the generated long-tail entry points (see `build.rs`).
//!
//! Every entry point the shim exports is classified in the generated capability inventory
//! ([`crate::CAPABILITIES`]) as `full` (hand-written real body), `partial` (a spec-legitimate no-op /
//! default-return that matches gl_shim.c and never claims to have done real work), or `stub` (an
//! operation a conforming driver performs, which the shim does NOT implement).
//!
//! The generated bodies are *truthful*:
//! - **partial** calls report the spec default (zeros/empty for queries, a benign no-op for optional
//!   state) and ALWAYS initialize their output parameters — but raise no error, because a no-op is the
//!   correct degraded behavior.
//! - **stub** calls set the API-correct GL/EGL error ([`fail_gl`]/[`fail_egl`]) so `glGetError` /
//!   `eglGetError` report the failure, initialize every output parameter to a deterministic zero, and
//!   return the spec's failure value. They NEVER report success-by-default.
//!
//! Two debugging aids sit on top:
//! - `DD_SHIM_DEBUG` (lenient, default): logs each unimplemented entry point once, the first time an
//!   app calls it — for exploratory "what does this app actually use" runs.
//! - `DD_SHIM_STRICT=1`: aborts at the FIRST `stub` call, printing the command, thread, and recent call
//!   history, so an unsupported call fails loudly instead of silently degrading.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

fn seen() -> &'static Mutex<std::collections::HashSet<&'static str>> {
    static S: OnceLock<Mutex<std::collections::HashSet<&'static str>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

const HISTORY_CAP: usize = 32;

fn history() -> &'static Mutex<VecDeque<&'static str>> {
    static H: OnceLock<Mutex<VecDeque<&'static str>>> = OnceLock::new();
    H.get_or_init(|| Mutex::new(VecDeque::with_capacity(HISTORY_CAP)))
}

fn strict() -> bool {
    static STRICT: OnceLock<bool> = OnceLock::new();
    *STRICT.get_or_init(|| std::env::var_os("DD_SHIM_STRICT").is_some())
}

fn debug() -> bool {
    // Not cached: exploratory runs may set DD_SHIM_DEBUG after the first calls; keep it cheap-but-live.
    std::env::var_os("DD_SHIM_DEBUG").is_some()
}

fn record(name: &'static str) {
    if let Ok(mut h) = history().lock() {
        if h.len() == HISTORY_CAP {
            h.pop_front();
        }
        h.push_back(name);
    }
}

fn trace_once(name: &'static str, kind: &str) {
    if !debug() {
        return;
    }
    if let Ok(mut s) = seen().lock() {
        if s.insert(name) {
            eprintln!("[dd-shim-gl] {kind} entry point: {name}");
        }
    }
}

/// Called by every generated **stub** (an unsupported operation). Records the call, once-logs it under
/// `DD_SHIM_DEBUG`, and — under `DD_SHIM_STRICT` — aborts the process with a diagnostic. Returns so the
/// generated body can then set the error state and initialize outputs (in lenient mode).
#[inline]
pub fn stub_call(name: &'static str) {
    record(name);
    trace_once(name, "unimplemented (stub)");
    if strict() {
        strict_abort(name);
    }
}

/// Called by every generated **partial** (a spec-legitimate no-op / default query). Records the call
/// and once-logs it under `DD_SHIM_DEBUG`, but never aborts — a no-op is the correct degraded behavior.
#[inline]
pub fn partial_call(name: &'static str) {
    record(name);
    trace_once(name, "no-op (partial)");
}

/// Deterministically initialize an output parameter: zero `count` elements of `p` (no-op if null).
/// The generated bodies call this for every non-const (`*mut`) pointer parameter so no out handle or
/// query result is ever left holding uninitialized garbage.
///
/// # Safety
/// `p` must be null or valid for writes of `count * size_of::<T>()` bytes. The generator only passes a
/// `count > 1` for the `glGen*(GLsizei n, T* out)` array contract where `n` is by definition the length
/// of `out`; every other call uses `count == 1`.
#[inline]
pub unsafe fn out_zero<T>(p: *mut T, count: usize) {
    if !p.is_null() && count > 0 {
        core::ptr::write_bytes(p, 0u8, count);
    }
}

/// Set the GL error flag for a stub (API-correct failure; see [`crate::state::set_gl_error`]).
#[inline]
pub fn fail_gl(err: u32) {
    crate::state::set_gl_error(err);
}

/// Set the EGL error for a stub. EGL semantics: `eglGetError` reports the last EGL call's status, so
/// this overwrites unconditionally.
#[inline]
pub fn fail_egl(err: i32) {
    crate::state::egl().error = err;
}

fn strict_abort(name: &'static str) -> ! {
    // Guard against re-entrancy (abort machinery must not recurse through another stub).
    static ABORTING: AtomicBool = AtomicBool::new(false);
    if ABORTING.swap(true, Ordering::SeqCst) {
        std::process::abort();
    }
    let recent: Vec<&str> = history().lock().map(|h| h.iter().copied().collect()).unwrap_or_default();
    let (major, minor) = {
        let e = crate::state::egl();
        (e.ctx_major, e.ctx_minor)
    };
    eprintln!("========================================================================");
    eprintln!("[dd-shim-gl] DD_SHIM_STRICT: aborting on unsupported entry point");
    eprintln!("  command : {name}");
    eprintln!("  object  : (unsupported capability — no backing object)");
    eprintln!("  thread  : {:?}", std::thread::current().id());
    eprintln!("  context : GLES {major}.{minor} (advertised {})", crate::ADVERTISED_GL_VERSION_STR);
    eprintln!("  history : {} recent call(s), most recent last:", recent.len());
    for n in &recent {
        eprintln!("            {n}");
    }
    eprintln!("  Set DD_SHIM_DEBUG=1 (without DD_SHIM_STRICT) to log unsupported calls without aborting.");
    eprintln!("========================================================================");
    std::process::abort();
}
