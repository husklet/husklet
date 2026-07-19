//! The shim's process-global GL context + the guest→host command sink.
//!
//! The `egl*`/`gl*` entry points are free `extern "C"` functions, so their shared mutable state lives
//! behind a process-global `Mutex`. The heavy lifting — the GLES/EGL→hl-GPU-IR lowering — is delegated to
//! the `hl_gl` service layer (`record` for the deferred `gl*` ops, `swap` for `eglSwapBuffers`), which
//! mutates a [`GlContext`] and (only at swap) submits protocol `Cmd`s through a
//! [`hl_gpu::RemoteCommandSink`]. That sink is the single boundary to the host GPU-exec service,
//! connected lazily from `$HL_GPU_EXEC` on first submit.
//!
//! This module owns only the C-ABI marshalling state the EGL front-end needs: the opaque display /
//! config / context / surface tokens and the last-error registers `eglGetError`/`glGetError` report. The
//! render semantics are NOT redefined here — they are the shared `hl_gl` services.

use core::cell::Cell;
use core::ffi::c_void;
use std::sync::{Mutex, OnceLock};

use hl_gl::model::context::GlContext;
use hl_gpu::RemoteCommandSink;

/// The single opaque `EGLDisplay` this shim hands out (`eglGetDisplay` → this token). Non-null.
pub const DISPLAY_TOKEN: usize = 1;
/// The single opaque `EGLConfig` this shim advertises. Non-null.
pub const CONFIG_TOKEN: usize = 1;

// EGL error codes the front-end registers (values re-declared from hl_gl::result for the C seam).
pub use hl_gl::result::{EGL_SUCCESS, GL_NO_ERROR};

/// Everything the shim tracks between `egl*`/`gl*` calls.
pub struct State {
    /// `eglInitialize` was called on the display.
    pub inited: bool,
    /// The GL object model + deferred-lowering draw-list (one current context in this model).
    pub ctx: GlContext,
    /// The guest→host boundary: encodes the frame batch and ships it framed over `$HL_GPU_EXEC`.
    pub sink: RemoteCommandSink,

    // The last EGL error is NOT stored here: EGL keys `eglGetError` PER CALLING THREAD (EGL 1.5 §3.1),
    // exactly like the current-binding cells below. A process-global error is wrong under Chrome's
    // multi-threaded GPU service — a present/commit failure on the compositor thread would otherwise
    // poison the raster thread's context (it reads the same error and treats it as `EGL_CONTEXT_LOST`,
    // losing every shared context). It lives in the thread-local [`EGL_ERROR`] cell instead.

    /// `EGLContext` token allocator (opaque, non-null). The "current" binding (context / draw+read
    /// surface / display) is NOT here: EGL keys it PER CALLING THREAD, so it lives in the thread-local
    /// [`current`] cells below (a process-global is wrong under GTK's threads — libepoxy probes
    /// `eglGetCurrentContext()` on the thread that made the context current).
    next_token: usize,

    /// Whether the current window surface is a Wayland window (created from a `wl_egl_window`). Keys the
    /// `eglSwapBuffers` compositor-commit path.
    pub current_is_wayland: bool,
    /// The app's `wl_surface*` (as a `usize`) the current window surface wraps (`0` = none). Recovered
    /// from the `wl_egl_window` in `eglCreateWindowSurface`.
    pub wl_surface_ptr: usize,
    /// The live self-contained `wl_shm` present session to the compositor (`None` in tests / when no
    /// compositor is reachable — the present is then skipped, never faked). This drives the shim's OWN
    /// toplevel and is the FALLBACK when the app-surface presenter is unavailable.
    pub wl: Option<hl_gl::adapter::wayland::Wayland>,
    /// The app-surface presenter: marshals the frame onto the app's OWN `wl_surface` via the app's
    /// `libwayland-client` (the real-window path). Lazily brought up from [`Self::wl_surface_ptr`].
    pub wl_app: Option<hl_gl::adapter::wayland_app::WaylandAppPresenter>,
    /// Latched once the app-surface presenter proved unavailable (libwayland/symbols/global absent), so
    /// bring-up is not retried every `eglSwapBuffers` — the self-owned [`Self::wl`] path is used instead.
    pub wl_app_unavailable: bool,

    /// Count of live `EGLContext`s (bumped by `eglCreateContext`, dropped by `eglDestroyContext`). All EGL
    /// contexts multiplex onto the single shared [`Self::ctx`] (one implicit share group), so the shim
    /// cannot free a single context's resources in isolation — but when this reaches `0` NO context remains,
    /// and the whole working set can be safely retired (see [`Self::destroy_context`]). This is what breaks
    /// Chrome's lost-context death spiral: each recreate cycle's abandoned working set is refunded when the
    /// prior context is destroyed, instead of piling onto the host residency ledger forever.
    pub live_contexts: u32,
}

/// The outcome of an app-surface present attempt, keying `eglSwapBuffers`' fall-back vs error handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppPresentOutcome {
    /// The frame was committed onto the app's own `wl_surface`.
    Presented,
    /// The presenter is unavailable (not a wayland app / libwayland or a symbol / global absent). The
    /// caller falls back to the self-owned [`State::wl`] present — never a faked frame.
    Unavailable,
    /// The presenter exists but a live commit/flush failed — surfaced as `EGL_CONTEXT_LOST`.
    Failed,
}

impl State {
    fn new() -> Self {
        State {
            inited: false,
            ctx: GlContext::new(),
            // Connect target from $HL_GPU_EXEC; the connection itself is opened lazily on first submit.
            sink: RemoteCommandSink::from_env(),
            next_token: 1,
            current_is_wayland: false,
            wl_surface_ptr: 0,
            wl: None,
            wl_app: None,
            wl_app_unavailable: false,
            live_contexts: 0,
        }
    }

    /// Record an `eglCreateContext` — one more live EGL context on the shared model.
    pub fn create_context(&mut self) {
        self.live_contexts = self.live_contexts.saturating_add(1);
    }

    /// Record an `eglDestroyContext`. When it drops the LAST live context, retire the whole working set the
    /// shared model has made resident on the host — queueing a `Destroy*` for every cached IR resource so the
    /// next submitted frame refunds its per-connection residency. See [`GlContext::retire_all`] and
    /// [`Self::live_contexts`]. Idempotent-ish: a stray destroy with no live context is a no-op sweep of the
    /// (already empty) caches.
    pub fn destroy_context(&mut self) {
        self.live_contexts = self.live_contexts.saturating_sub(1);
        if self.live_contexts == 0 {
            self.ctx.retire_all();
        }
    }

    /// Present the read-back frame onto the app's OWN `wl_surface`, lazily bringing up the presenter from
    /// [`Self::wl_surface_ptr`]. A soft (unavailable) bring-up latches [`Self::wl_app_unavailable`] so the
    /// dlopen is not retried each frame; a live commit failure is [`AppPresentOutcome::Failed`]. Never
    /// fakes a present.
    pub fn present_to_app_surface(&mut self, xrgb: &[u8], w: u32, h: u32) -> AppPresentOutcome {
        if self.wl_app.is_none() {
            if self.wl_app_unavailable {
                return AppPresentOutcome::Unavailable;
            }
            match hl_gl::adapter::wayland_app::WaylandAppPresenter::new(self.wl_surface_ptr) {
                Ok(p) => self.wl_app = Some(p),
                Err(_) => {
                    self.wl_app_unavailable = true;
                    return AppPresentOutcome::Unavailable;
                }
            }
        }
        match self.wl_app.as_mut().unwrap().present(xrgb, w, h) {
            Ok(()) => AppPresentOutcome::Presented,
            // A soft error here (e.g. the connection died) still means "no live app present": treat as a
            // hard Failed only when it is genuinely a live-present failure, else fall back.
            Err(e) if e.is_unavailable() => {
                self.wl_app = None;
                self.wl_app_unavailable = true;
                AppPresentOutcome::Unavailable
            }
            Err(_) => AppPresentOutcome::Failed,
        }
    }

    /// Mint a fresh opaque token (for `EGLContext` / `EGLSurface`).
    pub fn mint_token(&mut self) -> *mut c_void {
        let t = self.next_token;
        self.next_token += 1;
        t as *mut c_void
    }

    /// Record an EGL error for the CALLING THREAD (kept until `eglGetError` clears it). Per-thread so a
    /// present failure on one thread never surfaces as a lost context on another (see [`EGL_ERROR`]).
    pub fn set_egl_error(&mut self, e: i32) {
        EGL_ERROR.with(|c| c.set(e));
    }

    /// Read + clear the calling thread's last EGL error.
    pub fn take_egl_error(&mut self) -> i32 {
        EGL_ERROR.with(|c| c.replace(EGL_SUCCESS))
    }

    /// Clear the calling thread's EGL error to `EGL_SUCCESS`. A successful EGL entry point resets the
    /// error (EGL 1.5 §3.1: "the error is set to EGL_SUCCESS on a successful call"). `eglMakeCurrent`
    /// relies on this so a stale error from an earlier failed call cannot make a fresh, valid current
    /// binding look lost.
    pub fn clear_egl_error(&mut self) {
        EGL_ERROR.with(|c| c.set(EGL_SUCCESS));
    }
}

thread_local! {
    /// The last EGL error for THIS thread (`eglGetError` reads + clears it). Thread-local because EGL
    /// scopes errors to the calling thread; a process-global error let a Wayland-commit failure on the
    /// compositor thread poison Chrome's raster/GPU threads (they read the same cell and lose their
    /// shared GL context — the whole page then rasterizes black).
    static EGL_ERROR: core::cell::Cell<i32> = const { core::cell::Cell::new(EGL_SUCCESS) };
}

// ---- The single process-global `State`, shared across BOTH guest objects --------------------------
//
// The `gl*` (record) and `egl*` (swap/flush) entry points live in SEPARATE shared objects now —
// `libGLESv2.so.2` exports `gl*`, `libEGL.so.1` exports `egl*` (matching real Mesa's split) — yet they
// MUST share ONE `State`: `glDrawArrays` (in libGLESv2) records into the draw-list that `eglSwapBuffers`
// (in libEGL) flushes. So the `State` cannot be a plain per-object `static` (that would give each `.so`
// its OWN copy, and nothing would ever render).
//
// Instead exactly ONE object OWNS the `static` — every build EXCEPT the `cfg(gles_client)` one (so
// libEGL, the EGL/lifecycle object, analogous to Mesa's `libglapi` holding the shared dispatch; and a
// plain host build for this crate's own unit/ABI tests) — and exports the C-ABI accessor
// `hl_shim_state_ptr`. The `cfg(gles_client)` object (libGLESv2) instead has a `DT_NEEDED` on
// `libEGL.so.1` and IMPORTS that accessor, so every `with(..)` on either side resolves to the SAME
// `Mutex<State>`. (No cross-allocator hazard: both objects use Rust's default system allocator = libc
// `malloc`/`free`, one heap for the whole process.)

/// The single owned `State` cell + its exported accessor. Present in every object EXCEPT libGLESv2.
#[cfg(not(gles_client))]
mod owner {
    use super::{Mutex, OnceLock, State};
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    /// Pointer to the process-global `State` mutex — the ONE instance the whole process shares.
    /// Exported (default visibility) so libGLESv2 can bind it via `DT_NEEDED libEGL.so.1`.
    #[no_mangle]
    pub extern "C" fn hl_shim_state_ptr() -> *const Mutex<State> {
        STATE.get_or_init(|| Mutex::new(State::new()))
    }
}

/// The libGLESv2 (non-owner) object imports the accessor from libEGL via `DT_NEEDED`.
#[cfg(gles_client)]
extern "C" {
    fn hl_shim_state_ptr() -> *const Mutex<State>;
}

/// Resolve the process-global `State` mutex — the owner's local cell, or the imported accessor.
fn state_ptr() -> *const Mutex<State> {
    #[cfg(gles_client)]
    unsafe {
        hl_shim_state_ptr()
    }
    #[cfg(not(gles_client))]
    owner::hl_shim_state_ptr()
}

/// Run `f` with exclusive access to the global shim state. Non-reentrant — never call [`with`] from
/// inside an `f` (the `Mutex` is not recursive); each entry point does exactly one `with`.
pub struct GlobalState;
impl GlobalState {
pub fn access<R>(f: impl FnOnce(&mut State) -> R) -> R {
    // SAFETY: `state_ptr` returns a `&'static Mutex<State>` (as a raw pointer) that is either the owner's
    // own `OnceLock`-backed cell or the same cell imported from libEGL — never null, never dangling.
    let m: &Mutex<State> = unsafe { &*state_ptr() };
    let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut g)
}
}

/// The per-thread EGL "current" binding — what `eglGetCurrentContext` / `eglGetCurrentDisplay` /
/// `eglGetCurrentSurface(EGL_DRAW|EGL_READ)` report, and the API `eglBindAPI` / `eglQueryAPI` track.
///
/// Real EGL keys ALL of this state to the CALLING THREAD (`eglMakeCurrent` binds a context to the current
/// thread; another thread has its own binding). libepoxy's GL-vs-GLES dispatch selection probes
/// `eglGetCurrentContext()` (and `eglQueryAPI()`) on whichever thread made the context current and aborts
/// on a NULL context — so a process-global binding is not just imprecise, it is wrong under GTK's threads.
///
/// This is deliberately separate from the process-global [`State`]: the GL object model / draw-list / sink
/// (the render state) are shared, but "which context is current on THIS thread" is not.
pub mod current {
    use super::Cell;

    /// `EGL_OPENGL_ES_API` — the only client API this GLES driver serves and the EGL default a thread's
    /// bound API starts at (matches real EGL, which defaults to `EGL_OPENGL_ES_API`).
    pub const EGL_OPENGL_ES_API: u32 = 0x30A0;

    thread_local! {
        /// The context token bound current on this thread (`0` = `EGL_NO_CONTEXT`).
        static CTX: Cell<usize> = const { Cell::new(0) };
        /// The draw surface token of the current binding (`0` = `EGL_NO_SURFACE`).
        static DRAW: Cell<usize> = const { Cell::new(0) };
        /// The read surface token of the current binding (`0` = `EGL_NO_SURFACE`).
        static READ: Cell<usize> = const { Cell::new(0) };
        /// The display token of the current binding (`0` = `EGL_NO_DISPLAY`).
        static DISPLAY: Cell<usize> = const { Cell::new(0) };
        /// The API bound by `eglBindAPI` on this thread; defaults to `EGL_OPENGL_ES_API`.
        static API: Cell<u32> = const { Cell::new(EGL_OPENGL_ES_API) };
    }

    /// Record `eglMakeCurrent(display, draw, read, ctx)` for this thread. A `ctx` of `0`
    /// (`EGL_NO_CONTEXT`) RELEASES the binding — the surfaces + display are cleared too (EGL forbids a
    /// live current surface/display with no current context).
    pub fn make_current(display: usize, draw: usize, read: usize, ctx: usize) {
        if ctx == 0 {
            release();
            return;
        }
        CTX.with(|c| c.set(ctx));
        DRAW.with(|c| c.set(draw));
        READ.with(|c| c.set(read));
        DISPLAY.with(|c| c.set(display));
    }

    /// Clear this thread's current binding (context / surfaces / display) — `eglMakeCurrent` with
    /// `EGL_NO_CONTEXT`, or a `eglReleaseThread`.
    pub fn release() {
        CTX.with(|c| c.set(0));
        DRAW.with(|c| c.set(0));
        READ.with(|c| c.set(0));
        DISPLAY.with(|c| c.set(0));
    }

    /// If `ctx` is the context current on THIS thread, release the binding (used by `eglDestroyContext`).
    pub struct Binding;
    impl Binding {
    pub fn release_if_context(ctx: usize) {
        if CTX.with(|c| c.get()) == ctx {
            release();
        }
    }

    /// If `surface` is the draw or read surface of THIS thread's binding, forget it (used by
    /// `eglDestroySurface`), leaving the context otherwise current.
    pub fn forget_surface(surface: usize) {
        if DRAW.with(|c| c.get()) == surface {
            DRAW.with(|c| c.set(0));
        }
        if READ.with(|c| c.get()) == surface {
            READ.with(|c| c.set(0));
        }
    }
    }

    /// The context current on this thread (`eglGetCurrentContext`; `0` = `EGL_NO_CONTEXT`).
    pub fn context() -> usize {
        CTX.with(|c| c.get())
    }
    /// The display of this thread's current binding (`eglGetCurrentDisplay`; `0` = `EGL_NO_DISPLAY`).
    pub fn display() -> usize {
        DISPLAY.with(|c| c.get())
    }
    /// The draw surface of this thread's current binding (`0` = `EGL_NO_SURFACE`).
    pub fn draw_surface() -> usize {
        DRAW.with(|c| c.get())
    }
    /// The read surface of this thread's current binding (`0` = `EGL_NO_SURFACE`).
    pub fn read_surface() -> usize {
        READ.with(|c| c.get())
    }

    /// Record `eglBindAPI(api)` for this thread.
    impl Binding {
    pub fn bind_api(api: u32) {
        API.with(|c| c.set(api));
    }
    }
    /// The API bound on this thread (`eglQueryAPI`; defaults to `EGL_OPENGL_ES_API`).
    pub fn query_api() -> u32 {
        API.with(|c| c.get())
    }
}
