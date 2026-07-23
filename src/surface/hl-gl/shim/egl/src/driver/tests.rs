use super::*;
#[cfg(test)]
mod current_binding_tests {
    use super::*;

    // Desktop OpenGL API enum — NOT served by this GLES-only driver (eglBindAPI must reject it).
    const EGL_OPENGL_API: u32 = 0x30A2;

    /// eglMakeCurrent records (ctx, draw, read, display) as THIS thread's current binding, and the
    /// eglGetCurrent* getters return exactly those; EGL_NO_CONTEXT (null) clears the whole binding.
    #[test]
    fn make_current_round_trips_and_no_context_clears() {
        let dpy = 0x0D15 as *mut c_void;
        let draw = 0xD8A as *mut c_void;
        let read = 0x8EAD as *mut c_void;
        let ctx = 0xC0FFEE as *mut c_void;

        assert_eq!(eglMakeCurrent(dpy, draw, read, ctx), EGL_TRUE);
        assert_eq!(
            eglGetCurrentContext(),
            ctx,
            "current context is the one just made current"
        );
        assert_eq!(
            eglGetCurrentDisplay(),
            dpy,
            "current display is the one passed to makeCurrent"
        );
        assert_eq!(
            eglGetCurrentSurface(EGL_DRAW),
            draw,
            "EGL_DRAW surface round-trips"
        );
        assert_eq!(
            eglGetCurrentSurface(EGL_READ),
            read,
            "EGL_READ surface round-trips"
        );

        // EGL_NO_CONTEXT releases the binding: every getter returns null again.
        assert_eq!(
            eglMakeCurrent(
                dpy,
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut()
            ),
            EGL_TRUE
        );
        assert!(
            eglGetCurrentContext().is_null(),
            "EGL_NO_CONTEXT clears the current context"
        );
        assert!(
            eglGetCurrentDisplay().is_null(),
            "EGL_NO_CONTEXT clears the current display"
        );
        assert!(
            eglGetCurrentSurface(EGL_DRAW).is_null(),
            "EGL_NO_CONTEXT clears the draw surface"
        );
        assert!(
            eglGetCurrentSurface(EGL_READ).is_null(),
            "EGL_NO_CONTEXT clears the read surface"
        );
    }

    /// A getter on a thread that never made a context current returns null (EGL_NO_*), and one thread's
    /// current binding is INDEPENDENT of another's — the thread-local guarantee libepoxy relies on.
    #[test]
    fn current_binding_is_thread_local_and_independent() {
        let ctx_a = 0xA11 as usize;
        eglMakeCurrent(
            0x1 as *mut c_void,
            0x2 as *mut c_void,
            0x2 as *mut c_void,
            ctx_a as *mut c_void,
        );
        assert_eq!(eglGetCurrentContext() as usize, ctx_a);

        let observed = std::thread::spawn(|| {
            // A fresh thread has NO current context, regardless of the parent's binding.
            let before = eglGetCurrentContext();
            // It can make its OWN context current without disturbing the parent.
            let ctx_b = 0xB22 as usize;
            eglMakeCurrent(
                0x1 as *mut c_void,
                0x3 as *mut c_void,
                0x3 as *mut c_void,
                ctx_b as *mut c_void,
            );
            (before as usize, eglGetCurrentContext() as usize)
        })
        .join()
        .unwrap();

        assert_eq!(observed.0, 0, "a fresh thread starts with EGL_NO_CONTEXT");
        assert_eq!(observed.1, 0xB22, "the child thread bound its own context");
        // The parent's binding is untouched by the child.
        assert_eq!(
            eglGetCurrentContext() as usize,
            ctx_a,
            "the parent thread's current context is independent"
        );
        // Cleanup this thread's binding.
        eglMakeCurrent(
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        );
    }

    /// A successful `eglMakeCurrent` resets the CALLING THREAD's EGL error to `EGL_SUCCESS` (EGL 1.5 §3.1).
    /// REGRESSION (#144): a transient Wayland present failure records `EGL_BAD_SURFACE`/`EGL_CONTEXT_LOST`;
    /// without this reset the stale error survived, and the next time Chrome polled `eglGetError` after
    /// binding a perfectly valid context it read the old error as a freshly-lost context — losing every
    /// shared context and rasterizing the whole page black. Binding a context must clear it.
    #[test]
    fn egl_make_current_clears_the_thread_egl_error() {
        // A real failing EGL call leaves a pending error on this thread.
        assert_eq!(eglBindAPI(EGL_OPENGL_API), EGL_FALSE);
        let dpy = 0x1 as *mut c_void;
        let ctx = 0x2 as *mut c_void;
        assert_eq!(eglMakeCurrent(dpy, ctx, ctx, ctx), EGL_TRUE);
        assert_eq!(
            eglGetError(),
            hl_gl::result::EGL_SUCCESS,
            "a successful eglMakeCurrent must reset the thread's EGL error (no stale error survives)"
        );
        // Cleanup this thread's binding.
        eglMakeCurrent(
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        );
    }

    /// EGL errors are scoped to the CALLING THREAD (EGL 1.5 §3.1). REGRESSION (#144): the error register was
    /// a process-global, so a Wayland-commit failure on Chrome's compositor thread surfaced as a lost
    /// context on its raster/GPU thread (they shared the cell) and collapsed the entire shared-context GPU
    /// stack. A pending error on one thread must be invisible to another.
    #[test]
    fn egl_error_does_not_leak_across_threads() {
        // Record a pending error on THIS thread via a real failing EGL call.
        assert_eq!(eglBindAPI(EGL_OPENGL_API), EGL_FALSE);
        // A different thread must observe EGL_SUCCESS — never this thread's error.
        let child = std::thread::spawn(|| eglGetError()).join().unwrap();
        assert_eq!(
            child,
            hl_gl::result::EGL_SUCCESS,
            "a present failure on one thread must not poison another"
        );
        // This thread still holds its own error until it reads it.
        assert_eq!(
            eglGetError(),
            EGL_BAD_PARAMETER,
            "the setting thread keeps its own pending error"
        );
    }

    /// eglQueryAPI defaults to EGL_OPENGL_ES_API and returns whatever eglBindAPI(EGL_OPENGL_ES_API) set;
    /// a non-GLES API is rejected (EGL_BAD_PARAMETER) and does not change the queried API.
    #[test]
    fn bind_api_records_gles_and_rejects_other_apis() {
        // Default (nothing bound on this fresh thread) is the GLES API epoxy expects.
        assert_eq!(
            eglQueryAPI(),
            EGL_OPENGL_ES_API,
            "the default bound API is EGL_OPENGL_ES_API"
        );

        assert_eq!(
            eglBindAPI(EGL_OPENGL_ES_API),
            EGL_TRUE,
            "binding the GLES API succeeds"
        );
        assert_eq!(
            eglQueryAPI(),
            EGL_OPENGL_ES_API,
            "eglQueryAPI reports the bound GLES API"
        );

        // A GLES-only driver rejects desktop GL; the queried API is unchanged.
        assert_eq!(
            eglBindAPI(EGL_OPENGL_API),
            EGL_FALSE,
            "binding desktop OpenGL is rejected"
        );
        assert_eq!(
            eglGetError(),
            EGL_BAD_PARAMETER,
            "the rejected bind raised EGL_BAD_PARAMETER"
        );
        assert_eq!(
            eglQueryAPI(),
            EGL_OPENGL_ES_API,
            "a rejected bind leaves the queried API as GLES"
        );
    }

    /// eglQueryContext(EGL_CONTEXT_CLIENT_TYPE) MUST report EGL_OPENGL_ES_API. libepoxy's
    /// `epoxy_egl_get_current_gl_context_api()` (dispatch_common.c) queries exactly this to classify the
    /// current context; a `0`/EGL_NONE answer makes `epoxy_get_proc_address` abort with "Couldn't find
    /// current GLX or EGL context" — which is what blocked GTK4's GskGL bring-up until this was fixed.
    #[test]
    fn query_context_reports_gles_client_type_for_epoxy() {
        let dpy = 0x1 as *mut c_void;
        let ctx = 0x2 as *mut c_void;

        let mut client_type: i32 = -455_764_240; // garbage sentinel a correct getter overwrites
        assert_eq!(
            eglQueryContext(
                dpy,
                ctx,
                EGL_CONTEXT_CLIENT_TYPE,
                &mut client_type as *mut i32
            ),
            EGL_TRUE,
            "eglQueryContext succeeds"
        );
        assert_eq!(
            client_type as u32, EGL_OPENGL_ES_API,
            "EGL_CONTEXT_CLIENT_TYPE is the GLES client API epoxy classifies on (not 0/garbage)"
        );

        // The client version this ES3 driver reports, and the back-buffered render buffer.
        let mut version: i32 = -1;
        eglQueryContext(
            dpy,
            ctx,
            EGL_CONTEXT_CLIENT_VERSION,
            &mut version as *mut i32,
        );
        assert_eq!(version, 3, "EGL_CONTEXT_CLIENT_VERSION reports ES3");
        let mut rbuf: i32 = -1;
        eglQueryContext(dpy, ctx, EGL_RENDER_BUFFER, &mut rbuf as *mut i32);
        assert_eq!(
            rbuf, EGL_BACK_BUFFER,
            "EGL_RENDER_BUFFER reports the back buffer"
        );

        // A null out-param is rejected without a deref; an unknown attribute writes 0 and still succeeds.
        assert_eq!(
            eglQueryContext(dpy, ctx, EGL_CONTEXT_CLIENT_TYPE, core::ptr::null_mut()),
            EGL_FALSE,
            "a null value pointer is rejected"
        );
        let mut unknown: i32 = -455_764_240;
        eglQueryContext(dpy, ctx, 0xBEEF, &mut unknown as *mut i32);
        assert_eq!(
            unknown, 0,
            "an unknown attribute writes 0, never uninitialized memory"
        );
    }

    /// glGetIntegerv / glGetInteger64v ALWAYS write the out-param (never leave it as uninitialized
    /// garbage): GL_MAX_TEXTURE_SIZE is the 16384 executor ceiling and an unknown pname writes 0.
    #[test]
    fn gl_get_integerv_always_writes_the_out_param() {
        // Seed with a garbage sentinel; a correct getter overwrites it.
        let mut v: i32 = -455_764_240;
        glGetIntegerv(GL_MAX_TEXTURE_SIZE, &mut v as *mut i32);
        assert_eq!(
            v, 16384,
            "GL_MAX_TEXTURE_SIZE is the truthful executor ceiling, not garbage"
        );

        let mut v64: i64 = -1;
        glGetInteger64v(GL_MAX_TEXTURE_SIZE, &mut v64 as *mut i64);
        assert_eq!(
            v64, 16384,
            "glGetInteger64v writes the same truthful ceiling"
        );

        // An unhandled integer pname defaults to 0 — never the untouched garbage sentinel.
        let mut u: i32 = -455_764_240;
        glGetIntegerv(0xBEEF, &mut u as *mut i32);
        assert_eq!(
            u, 0,
            "an unknown pname writes 0, never uninitialized memory"
        );
    }
}
