use super::Cell;

/// `EGL_OPENGL_ES_API` — the only client API this GLES driver serves and the EGL default a thread's
/// bound API starts at (matches real EGL, which defaults to `EGL_OPENGL_ES_API`).
pub const EGL_OPENGL_ES_API: u32 = 0x30A0;

#[repr(C)]
#[derive(Clone, Copy)]
struct State {
    context: usize,
    draw: usize,
    read: usize,
    display: usize,
    api: u32,
}

const EMPTY: State = State {
    context: 0,
    draw: 0,
    read: 0,
    display: 0,
    api: EGL_OPENGL_ES_API,
};

// libEGL owns the binding TLS. libGLESv2 is a distinct shared object, so compiling this thread_local
// in both roles would create two cells: eglMakeCurrent would update libEGL's copy while every gl*
// dispatch would read zeros from libGLESv2's copy. Exporting the owner cell's address preserves EGL's
// per-thread semantics while giving both objects the same binding on the calling thread.
#[cfg(not(gles_client))]
thread_local! {
    static CURRENT: Cell<State> = const { Cell::new(EMPTY) };
}

#[cfg(not(gles_client))]
#[no_mangle]
extern "C" fn hl_shim_current_ptr() -> *const Cell<State> {
    CURRENT.with(|current| current as *const Cell<State>)
}

#[cfg(gles_client)]
extern "C" {
    fn hl_shim_current_ptr() -> *const Cell<State>;
}

fn binding() -> &'static Cell<State> {
    #[cfg(gles_client)]
    let ptr = unsafe { hl_shim_current_ptr() };
    #[cfg(not(gles_client))]
    let ptr = hl_shim_current_ptr();

    // SAFETY: libEGL owns one `CURRENT` cell per thread and returns that calling thread's stable TLS
    // address. libGLESv2 has a DT_NEEDED edge to libEGL, so the imported accessor cannot outlive its
    // owner. Access remains on the calling thread, matching `Cell`'s single-thread contract.
    unsafe { &*ptr }
}

/// Record `eglMakeCurrent(display, draw, read, ctx)` for this thread. A `ctx` of `0`
/// (`EGL_NO_CONTEXT`) RELEASES the binding — the surfaces + display are cleared too (EGL forbids a
/// live current surface/display with no current context).
pub fn make_current(display: usize, draw: usize, read: usize, ctx: usize) {
    if ctx == 0 {
        release();
        return;
    }
    let mut current = binding().get();
    current.context = ctx;
    current.draw = draw;
    current.read = read;
    current.display = display;
    binding().set(current);
}

/// Clear this thread's current binding (context / surfaces / display) — `eglMakeCurrent` with
/// `EGL_NO_CONTEXT`, or a `eglReleaseThread`.
pub fn release() {
    let api = binding().get().api;
    binding().set(State { api, ..EMPTY });
}

/// If `ctx` is the context current on THIS thread, release the binding (used by `eglDestroyContext`).
pub struct Binding;
impl Binding {
    /// If `surface` is the draw or read surface of THIS thread's binding, forget it (used by
    /// `eglDestroySurface`), leaving the context otherwise current.
    pub fn forget_surface(surface: usize) {
        let mut current = binding().get();
        if current.draw == surface {
            current.draw = 0;
        }
        if current.read == surface {
            current.read = 0;
        }
        binding().set(current);
    }
}

/// The context current on this thread (`eglGetCurrentContext`; `0` = `EGL_NO_CONTEXT`).
pub fn context() -> usize {
    binding().get().context
}
/// The display of this thread's current binding (`eglGetCurrentDisplay`; `0` = `EGL_NO_DISPLAY`).
pub fn display() -> usize {
    binding().get().display
}
/// The draw surface of this thread's current binding (`0` = `EGL_NO_SURFACE`).
pub fn draw_surface() -> usize {
    binding().get().draw
}
/// The read surface of this thread's current binding (`0` = `EGL_NO_SURFACE`).
pub fn read_surface() -> usize {
    binding().get().read
}

/// Record `eglBindAPI(api)` for this thread.
impl Binding {
    pub fn bind_api(api: u32) {
        let mut current = binding().get();
        current.api = api;
        binding().set(current);
    }
}
/// The API bound on this thread (`eglQueryAPI`; defaults to `EGL_OPENGL_ES_API`).
pub fn query_api() -> u32 {
    binding().get().api
}
