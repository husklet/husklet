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
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::image::{Image, Images};
use crate::transport::{DisplayTransport, Plan, Ready};
use hl_gl::model::context::{GlSurface, SurfaceKind, SurfaceTarget};
use hl_gl::model::texture::SharedPixels;
use hl_gpu::protocol::model::capability::{FeatureRequest, PresentKind};
use hl_gpu::protocol::model::command::WIRE_VERSION;
use hl_gpu::transport::DEFAULT_EXEC_SOCK;
use hl_gpu::{CommandSink, RemoteCommandSink, TransportConfig};

mod binding;
mod context;
pub mod current;
mod global;
mod group;
mod io;
mod model;
mod plan;
mod submit;
use context::{BindError as ContextBindError, Contexts};
pub use global::GlobalState;
use global::EGL_ERROR;
pub(crate) use group::GroupData;
use io::IoPlan;
pub(crate) use io::{IoResult, Observation};
use plan::SubmitPlan;

/// The single opaque `EGLDisplay` this shim hands out (`eglGetDisplay` → this token). Non-null.
pub const DISPLAY_TOKEN: usize = 1;
/// The single opaque `EGLConfig` this shim advertises. Non-null.
pub const CONFIG_TOKEN: usize = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextAttributes {
    pub client_version: i32,
    pub minor_version: i32,
    pub robust_access: bool,
    pub reset_strategy: i32,
    pub no_error: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EglSync {
    pub context: usize,
    pub gl: usize,
}

impl Default for ContextAttributes {
    fn default() -> Self {
        Self {
            client_version: 3,
            minor_version: 1,
            robust_access: false,
            reset_strategy: 0x31BE,
            no_error: false,
        }
    }
}

struct Surface {
    pub render: GlSurface,
    pub kind: SurfaceKind,
    pub wl_surface: usize,
    pub native_window: usize,
    pub wl_app: Option<hl_gl::adapter::wayland_app::WaylandAppPresenter>,
    pub wl_app_unavailable: bool,
    pub target: SurfaceTarget,
}

#[derive(Clone, Copy)]
pub struct SurfaceInfo {
    pub render: GlSurface,
    pub kind: SurfaceKind,
    pub wl_surface: usize,
    pub native_window: usize,
}

pub struct SurfaceSlot {
    state: Mutex<Surface>,
}

impl SurfaceSlot {
    fn new(surface: Surface) -> Self {
        Self {
            state: Mutex::new(surface),
        }
    }

    fn info(&self) -> SurfaceInfo {
        let surface = self.state.lock().expect("surface slot poisoned");
        SurfaceInfo {
            render: surface.render,
            kind: surface.kind,
            wl_surface: surface.wl_surface,
            native_window: surface.native_window,
        }
    }

    fn take_target(&self) -> SurfaceTarget {
        let mut surface = self.state.lock().expect("surface slot poisoned");
        core::mem::take(&mut surface.target)
    }

    fn install_target(&self, target: SurfaceTarget) {
        if target.is_empty() {
            return;
        }
        let mut surface = self.state.lock().expect("surface slot poisoned");
        debug_assert!(surface.target.is_empty());
        surface.target = target;
    }
}

#[derive(Clone)]
pub struct ImportedImage {
    pub generation: u64,
    pub image: Arc<Image>,
    pub shared: Option<Arc<SharedPixels>>,
}

// EGL error codes the front-end registers (values re-declared from hl_gl::result for the C seam).
pub use hl_gl::result::{EGL_SUCCESS, GL_NO_ERROR};

/// Everything the shim tracks between `egl*`/`gl*` calls.
pub struct State {
    /// `eglInitialize` was called on the display.
    pub inited: bool,
    /// Context-local state by opaque EGL context token.
    contexts: Contexts,
    /// Display-owned transport connects before sandboxing and moves the socket into its FIFO actor.
    transport: Arc<DisplayTransport>,
    ready: Option<Ready>,
    /// An explicitly projected GPU endpoint must be opened during EGL initialization, before clients such
    /// as Chromium enter a syscall sandbox that denies creating new sockets.
    remote_required: bool,

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
    surfaces: HashMap<usize, Arc<SurfaceSlot>>,
    surface_current: HashMap<usize, usize>,
    pending_surfaces: HashSet<usize>,
    surface_retirements: Vec<hl_gpu::Cmd>,
    pub images: Images,
    /// Largest single readback buffer accepted by the negotiated GPU executor.
    pub max_buffer_bytes: u64,
    /// Publication serials are process-monotonic and deliberately never rolled back after a failed batch.
    next_external_serial: Option<u64>,

    /// Whether the current window surface is a Wayland window (created from a `wl_egl_window`). Keys the
    /// `eglSwapBuffers` compositor-commit path.
    pub current_is_wayland: bool,
    /// The app's `wl_surface*` (as a `usize`) the current window surface wraps (`0` = none). Recovered
    /// from the `wl_egl_window` in `eglCreateWindowSurface`.
    pub wl_surface_ptr: usize,
    current_surface: usize,
    /// The live self-contained `wl_shm` present session to the compositor (`None` in tests / when no
    /// compositor is reachable — the present is then skipped, never faked). This drives the shim's OWN
    /// toplevel and is the FALLBACK when the app-surface presenter is unavailable.
    pub wl: Option<hl_gl::adapter::wayland::Wayland>,
    /// The app-surface presenter: marshals the frame onto the app's OWN `wl_surface` via the app's
    /// `libwayland-client` (the real-window path). Lazily brought up from [`Self::wl_surface_ptr`].
    native_present: bool,

    /// Count of live `EGLContext`s. Each context belongs to an explicit share group; the last context in a
    /// group retires that group's working set independently.
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MakeCurrentError {
    Context,
    Surface,
    Access,
}

struct BoundSurface {
    token: usize,
    slot: Arc<SurfaceSlot>,
    info: SurfaceInfo,
    live: bool,
}

struct Binding {
    previous_context: usize,
    previous_group: Option<Arc<group::GroupSlot>>,
    previous_draw: Option<BoundSurface>,
    previous_read: Option<BoundSurface>,
    context: usize,
    group: Option<Arc<group::GroupSlot>>,
    draw: Option<BoundSurface>,
    read: Option<BoundSurface>,
    retire: Option<context::Retire>,
}

struct InFlightGroup {
    group: Arc<group::GroupSlot>,
    armed: bool,
}

impl InFlightGroup {
    fn new(group: Arc<group::GroupSlot>) -> Self {
        Self { group, armed: true }
    }

    fn complete(&mut self) {
        self.armed = false;
    }
}

impl Drop for InFlightGroup {
    fn drop(&mut self) {
        if self.armed {
            self.group
                .lose("GPU transport abandoned an accepted operation");
        }
    }
}

#[cfg(test)]
#[path = "state/tests.rs"]
mod tests;
