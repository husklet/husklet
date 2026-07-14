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

    /// Last EGL error (`eglGetError` reads + clears it). The GL error register lives on the modeled
    /// `ctx` ([`GlContext::gl_error`]) so it is unit-testable in the `hl_gl` lib crate.
    pub egl_error: i32,

    /// `EGLContext` token allocator (opaque, non-null); the current bound context token (`0` = none).
    next_token: usize,
    pub current_ctx: usize,
    /// The current `EGLSurface` token (`0` = none). The single window surface lives in `ctx.surf`.
    pub current_surface: usize,
}

impl State {
    fn new() -> Self {
        State {
            inited: false,
            ctx: GlContext::new(),
            // Connect target from $HL_GPU_EXEC; the connection itself is opened lazily on first submit.
            sink: RemoteCommandSink::from_env(),
            egl_error: EGL_SUCCESS,
            next_token: 1,
            current_ctx: 0,
            current_surface: 0,
        }
    }

    /// Mint a fresh opaque token (for `EGLContext` / `EGLSurface`).
    pub fn mint_token(&mut self) -> *mut c_void {
        let t = self.next_token;
        self.next_token += 1;
        t as *mut c_void
    }

    /// Record an EGL error (kept until `eglGetError` clears it).
    pub fn set_egl_error(&mut self, e: i32) {
        self.egl_error = e;
    }

    /// Read + clear the last EGL error.
    pub fn take_egl_error(&mut self) -> i32 {
        std::mem::replace(&mut self.egl_error, EGL_SUCCESS)
    }
}

static STATE: OnceLock<Mutex<State>> = OnceLock::new();

/// Run `f` with exclusive access to the global shim state. Non-reentrant — never call [`with`] from
/// inside an `f` (the `Mutex` is not recursive); each entry point does exactly one `with`.
pub fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let m = STATE.get_or_init(|| Mutex::new(State::new()));
    let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut g)
}
