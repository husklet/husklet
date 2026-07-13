//! Mirrors of the two RED conformance probes (dd-tests/guests/gui_matrix), encoding their assertions as
//! in-tree Rust tests so the object/context model (audit §9.3) is guarded offline:
//!   * `gui_egl_error_lifecycle`   — first-error retention, clear-on-read, no state mutation on reject.
//!   * `gui_egl_sharegroup_threads`— unique context handles, share-group visibility, unrelated-context
//!     isolation, per-thread current, cross-context deletion.

use core::ffi::c_void;

use dd_shim_gl::egl::{
    eglCreateContext, eglDestroyContext, eglGetCurrentContext, eglGetCurrentDisplay, eglGetError, eglMakeCurrent,
};
use dd_shim_gl::gles::{
    glBindBuffer, glBufferData, glDeleteBuffers, glEnable, glGenBuffers, glGetError, glGetIntegerv, glIsBuffer,
    glUniform1i, glUseProgram, glViewport,
};

const GL_NO_ERROR: u32 = 0;
const GL_INVALID_ENUM: u32 = 0x0500;
const GL_INVALID_VALUE: u32 = 0x0501;
const GL_INVALID_OPERATION: u32 = 0x0502;
const GL_VIEWPORT: u32 = 0x0BA2;
const GL_ARRAY_BUFFER_BINDING: u32 = 0x8894;
const GL_ARRAY_BUFFER: u32 = 0x8892;
const GL_STATIC_DRAW: u32 = 0x88E4;
const EGL_SUCCESS: i32 = 0x3000;
const EGL_CONTEXT_CLIENT_VERSION: i32 = 0x3098;
const EGL_NONE: i32 = 0x3038;

fn dpy() -> *mut c_void {
    1 as *mut c_void
}
fn create_es2(share: *mut c_void) -> *mut c_void {
    let attrs = [EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE];
    eglCreateContext(dpy(), 1 as *mut c_void, share, attrs.as_ptr())
}
fn make_current(ctx: *mut c_void) -> bool {
    eglMakeCurrent(dpy(), core::ptr::null_mut(), core::ptr::null_mut(), ctx) == 1
}

/// A raw EGL handle made Send so it can cross a thread boundary (as the C probe passes it by value).
#[derive(Clone, Copy)]
struct H(usize);
unsafe impl Send for H {}
impl H {
    fn p(self) -> *mut c_void {
        self.0 as *mut c_void
    }
}

// ---- gui_egl_error_lifecycle -------------------------------------------------------------------

#[test]
fn error_lifecycle_retention_clear_and_nonmutation() {
    let ctx = create_es2(core::ptr::null_mut());
    assert!(!ctx.is_null());
    assert!(make_current(ctx));

    // Drain, then confirm a clean start.
    let _ = glGetError();
    assert_eq!(glGetError(), GL_NO_ERROR, "initial");

    // Invalid viewport dimensions -> INVALID_VALUE, and the previous viewport is NOT modified.
    glViewport(3, 5, 31, 37);
    let mut before = [0i32; 4];
    glGetIntegerv(GL_VIEWPORT, before.as_mut_ptr());
    glViewport(11, 13, -1, 17);
    assert_eq!(glGetError(), GL_INVALID_VALUE, "negative_viewport");
    let mut after = [0i32; 4];
    glGetIntegerv(GL_VIEWPORT, after.as_mut_ptr());
    assert_eq!(before, after, "negative viewport must not mutate state");

    // Invalid capability enum -> INVALID_ENUM.
    glEnable(0xdead);
    assert_eq!(glGetError(), GL_INVALID_ENUM, "invalid_enable");

    // Invalid buffer target -> INVALID_ENUM and ARRAY_BUFFER_BINDING is untouched.
    let mut buffer = 0u32;
    glGenBuffers(1, &mut buffer);
    let mut bind_before = -1i32;
    glGetIntegerv(GL_ARRAY_BUFFER_BINDING, &mut bind_before);
    glBindBuffer(0xbeef, buffer);
    assert_eq!(glGetError(), GL_INVALID_ENUM, "invalid_buffer_target");
    let mut bind_after = -1i32;
    glGetIntegerv(GL_ARRAY_BUFFER_BINDING, &mut bind_after);
    assert_eq!(bind_before, bind_after, "invalid bind must not mutate binding");

    // Negative object count -> INVALID_VALUE and the output sentinel is NOT written.
    let mut sentinel = 0xfeed_1234u32;
    glGenBuffers(-1, &mut sentinel);
    assert_eq!(glGetError(), GL_INVALID_VALUE, "negative_gen_count");
    assert_eq!(sentinel, 0xfeed_1234u32, "negative gen must not write output");

    // First-error retention: a later error does NOT overwrite the first, and read clears it.
    glEnable(0xdead); // INVALID_ENUM (first)
    glViewport(0, 0, -2, 1); // would be INVALID_VALUE, but the first error is retained
    assert_eq!(glGetError(), GL_INVALID_ENUM, "first_error_retained");
    assert_eq!(glGetError(), GL_NO_ERROR, "error_cleared");

    // A uniform with no program in use -> INVALID_OPERATION (does not silently succeed).
    glUseProgram(0);
    glUniform1i(0, 1);
    assert_eq!(glGetError(), GL_INVALID_OPERATION, "uniform_without_program");

    make_current(core::ptr::null_mut());
    eglDestroyContext(dpy(), ctx);
}

// ---- gui_egl_sharegroup_threads ----------------------------------------------------------------

#[test]
fn sharegroup_identity_visibility_isolation_and_delete() {
    let a = create_es2(core::ptr::null_mut());
    let b = create_es2(a); // shares a's namespace
    let c = create_es2(core::ptr::null_mut()); // independent namespace
    assert!(!a.is_null() && !b.is_null() && !c.is_null(), "contexts must be non-null");
    assert!(a != b && a != c && b != c, "context handles must be unique");

    // Create a buffer while A is current; A and B (shared) see it, C (unrelated) does not.
    assert!(make_current(a));
    let mut shared = 0u32;
    glGenBuffers(1, &mut shared);
    glBindBuffer(GL_ARRAY_BUFFER, shared);
    let payload = 0x1234_5678u32;
    glBufferData(GL_ARRAY_BUFFER, 4, &payload as *const u32 as *const c_void, GL_STATIC_DRAW);
    assert!(shared != 0 && glIsBuffer(shared) == 1, "A must see its own buffer");

    assert!(make_current(b));
    assert_eq!(glIsBuffer(shared), 1, "shared context B must see A's buffer");

    assert!(make_current(c));
    assert_eq!(glIsBuffer(shared), 0, "unrelated context C must NOT see A's buffer");

    // Cross-context deletion within the share group: delete through B, gone in A.
    assert!(make_current(b));
    glDeleteBuffers(1, &shared);
    assert!(make_current(a));
    assert_eq!(glIsBuffer(shared), 0, "delete through B must remove the shared object from A");

    make_current(core::ptr::null_mut());
    eglDestroyContext(dpy(), b);
    eglDestroyContext(dpy(), c);
    eglDestroyContext(dpy(), a);
    assert_eq!(eglGetError(), EGL_SUCCESS, "clean lifecycle raises no EGL error");
}

#[test]
fn per_thread_current_is_independent() {
    let a = create_es2(core::ptr::null_mut());
    let b = create_es2(core::ptr::null_mut());
    assert!(!a.is_null() && !b.is_null());
    let (ha, hb) = (H(a as usize), H(b as usize));

    // Two threads make DIFFERENT contexts current concurrently; each observes its own, then unbinds.
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let run = |h: H, bar: std::sync::Arc<std::sync::Barrier>| {
        std::thread::spawn(move || {
            assert!(make_current(h.p()), "make current");
            bar.wait();
            assert_eq!(eglGetCurrentContext(), h.p(), "thread observes its own context");
            assert!(!eglGetCurrentDisplay().is_null(), "current display set while current");
            bar.wait();
            assert!(make_current(core::ptr::null_mut()));
            assert!(eglGetCurrentContext().is_null(), "unbound -> EGL_NO_CONTEXT");
            assert!(eglGetCurrentDisplay().is_null(), "unbound -> EGL_NO_DISPLAY");
        })
    };
    let ta = run(ha, barrier.clone());
    let tb = run(hb, barrier.clone());
    ta.join().unwrap();
    tb.join().unwrap();

    eglDestroyContext(dpy(), a);
    eglDestroyContext(dpy(), b);
}
