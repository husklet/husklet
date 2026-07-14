//! GPU IR executor lifecycle for the Smithay compositor path (Phase 6.1–6.2).
//!
//! ## Why this module exists
//! `hl-display/src/main.rs` execs THIS binary when `HL_DISPLAY_SMITHAY=1`, *replacing* the process
//! before it ever starts the GPU executor thread. The legacy hand-written compositor
//! (`hl-display/src/server.rs`) starts the hl-gpu IR executor itself — see `serve_loop_metal` and
//! `run_window` in `hl-display`, which call `metal::start_gpu_bridge()` + spawn
//! `metal_backend::run_executor()`. Without the same startup here, `HL_GPU_BACKEND=wgpu` *and* the
//! default bespoke Metal executor are UNREACHABLE on the Smithay path: an accelerated guest
//! (GLES/EGL, Vulkan, CUDA, GPU-composited Chrome) connects to the executor socket, finds nothing
//! listening, and silently renders white. Only pure `wl_shm` software clients (GTK/cairo) work,
//! because they need no host GPU backend at all. This module closes that gap.
//!
//! ## What it does
//! [`start`] brings up the executor ONCE, *before* the compositor mode is selected, so BOTH the
//! native Cocoa/Metal present loop and the headless `--png` loop get an executor (audit §9.4: "a
//! thread owned before compositor selection"). It respects `HL_GPU_BACKEND` exactly like the legacy
//! path — [`hl_display::metal_backend::run_executor`] itself dispatches to the wgpu backend when
//! [`hl_gpu_wgpu::selected`] is true, else the default Metal replay — so this file names the backend
//! only for logging and never re-implements the selection.
//!
//! ## Platform gating
//! The executor entry points live in `hl-display`'s macOS-only `metal`/`metal_backend` modules (the
//! host GPU is Metal, or wgpu-on-Metal), so the real startup is `#[cfg(target_os = "macos")]`. On the
//! Linux dev host there is no host GPU to start; [`start`] records the executor as unavailable and the
//! Phase 6.2 health check ([`warn_if_accel_client_without_executor`]) then fails VISIBLY if an
//! accelerated client still connects — instead of the old silent white frame.

use std::sync::atomic::{AtomicBool, Ordering};

/// Set once the GPU IR executor thread has actually been spawned for this process.
static EXECUTOR_RUNNING: AtomicBool = AtomicBool::new(false);
/// Guards [`start`] so a second call (e.g. a future supervisor re-entry) is a harmless no-op instead
/// of double-registering the mach bridge / re-binding the executor socket.
static START_ONCE: AtomicBool = AtomicBool::new(false);
/// Ensures the "accelerated client but no executor" error is logged once, not once per frame.
static GPU_WARNED: AtomicBool = AtomicBool::new(false);

/// Human-readable name of the selected host GPU backend, read from the single source of truth
/// (`HL_GPU_BACKEND` via [`hl_gpu_wgpu::selected`]). Used only for startup/health logging.
pub fn backend_name() -> &'static str {
    if hl_gpu_wgpu::selected() {
        "wgpu"
    } else {
        "metal"
    }
}

/// Whether the GPU IR executor thread is running for this compositor process. `false` on a platform
/// with no host GPU (the Linux dev host) or if startup failed.
pub fn is_running() -> bool {
    EXECUTOR_RUNNING.load(Ordering::SeqCst)
}

/// Readiness health handle for accelerated (dmabuf/IOSurface) imports: whether a host GPU executor is
/// up and able to render/present dd IOSurface-backed buffers. This is the ACTIONABLE successor to
/// [`warn_if_accel_client_without_executor`] (which only logs): [`crate::HlState::dmabuf_imported`]
/// REJECTS a hl-tagged dmabuf when this returns `false`, instead of accepting the buffer and letting the
/// client render into a surface nothing can present (a white window). Currently the executor being
/// running IS the health signal; a future supervisor can widen this to a bind/ready result + exit watch.
pub fn executor_healthy() -> bool {
    is_running()
}

/// Record the host GPU executor's health/readiness. Set to `true` once the executor thread is up (see
/// [`start`]) and `false` if it exits or is known-unavailable. Also the seam a test/supervisor uses to
/// assert executor readiness for the accelerated-import path without spawning a real Metal executor.
pub fn set_executor_health(healthy: bool) {
    EXECUTOR_RUNNING.store(healthy, Ordering::SeqCst);
}

/// Start the hl-gpu IR executor (and the IOSurface mach bridge) for the Smithay compositor, once.
///
/// `disp_socket` is the Wayland socket path; the executor socket is derived beside it (or taken from
/// `HL_GPU_EXEC_SOCK`) so a guest built for the legacy path finds the same endpoint on the Smithay
/// path. Idempotent — subsequent calls return immediately.
pub fn start(disp_socket: &str) {
    if START_ONCE.swap(true, Ordering::SeqCst) {
        return;
    }
    start_impl(disp_socket);
}

/// The hl-gpu IR executor socket: `HL_GPU_EXEC_SOCK` if set, else `hl-gpu.sock` beside the display
/// socket. Mirrors `hl-display`'s `gpu_exec_sock` so the Smithay and legacy paths agree on the
/// endpoint. `None` only when the display socket has no parent directory.
#[cfg(target_os = "macos")]
fn exec_sock(disp_socket: &str) -> Option<String> {
    if let Ok(p) = std::env::var("HL_GPU_EXEC_SOCK") {
        return Some(p);
    }
    let dir = std::path::Path::new(disp_socket).parent()?;
    Some(dir.join("hl-gpu.sock").to_string_lossy().into_owned())
}

#[cfg(target_os = "macos")]
fn start_impl(disp_socket: &str) {
    let Some(sock) = exec_sock(disp_socket) else {
        eprintln!(
            "hl-compositor: WARNING cannot resolve a GPU executor socket (display socket {disp_socket:?} \
             has no parent dir); accelerated guests will have NO host GPU backend on this Smithay session"
        );
        return;
    };
    // GPU rung 2: receive guest IOSurface send-rights over the mach bridge so a dmabuf-tagged buffer
    // resolves to the host surface at present time. Harmless idle thread if no accelerated client
    // ever connects (parity with hl-display's serve_loop_metal / run_window).
    hl_display::metal::start_gpu_bridge();
    // GPU rung 3: the guest streams framed hl-gpu IR here; `run_executor` replays it on the host GPU
    // into the resolved IOSurface. It branches on HL_GPU_BACKEND internally (wgpu vs default Metal),
    // so this call is the single startup for BOTH backends — nothing here duplicates that selection.
    let backend = backend_name();
    eprintln!(
        "hl-compositor: starting hl-gpu executor (backend={backend}) on {sock} \
         [Smithay path: accelerated GLES/Vulkan/CUDA/Chrome guests reach the host GPU here]"
    );
    // Mark running BEFORE the thread binds the socket: the health check only needs to know the
    // executor was started for this session, and a client cannot present a GPU frame before it has
    // itself connected to (and been served by) this same executor socket.
    EXECUTOR_RUNNING.store(true, Ordering::SeqCst);
    std::thread::spawn(move || hl_display::metal_backend::run_executor(sock));
}

#[cfg(not(target_os = "macos"))]
fn start_impl(_disp_socket: &str) {
    // The hl-gpu host executor renders on the host GPU (Metal / wgpu-on-Metal), which only exists on
    // the macOS host. On the Linux dev host there is no host GPU backend to start, so the executor
    // stays not-running and `warn_if_accel_client_without_executor` fails visibly if an accelerated
    // client connects — never a silent white frame.
    eprintln!(
        "hl-compositor: no host GPU on this platform; hl-gpu executor not started \
         (HL_GPU_BACKEND={}). Only wl_shm software clients will render.",
        backend_name()
    );
}

/// Phase 6.2 health / capability check. Call this the moment an accelerated client is observed — it
/// has imported a dd IOSurface-backed dmabuf, i.e. it expects the host GPU to render/present its
/// frames. If no executor is running, FAIL VISIBLY: log a prominent, once-only error instead of
/// letting the client render a silent white window. (When the executor *is* running this is a cheap
/// no-op, so it is safe to call on every accelerated import.)
pub fn warn_if_accel_client_without_executor() {
    if is_running() {
        return;
    }
    if GPU_WARNED.swap(true, Ordering::SeqCst) {
        return;
    }
    eprintln!(
        "hl-compositor: ERROR an accelerated client attached a GPU (dmabuf/IOSurface) buffer but NO \
         hl-gpu executor is running — its GPU frames cannot be rendered and WILL appear blank/white. \
         (HL_GPU_BACKEND={}; the host GPU executor is unavailable on this build/platform.)",
        backend_name()
    );
}
