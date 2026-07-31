use super::*;

// Desktop OpenGL API enum — NOT served by this GLES-only driver (eglBindAPI must reject it).
const EGL_OPENGL_API: u32 = 0x30A2;

fn context() -> *mut c_void {
    let attributes = [
        EGL_CONTEXT_CLIENT_VERSION,
        3,
        EGL_CONTEXT_MINOR_VERSION_KHR,
        1,
        EGL_NONE,
    ];
    eglCreateContext(
        DISPLAY_TOKEN as *mut c_void,
        CONFIG_TOKEN as *mut c_void,
        core::ptr::null_mut(),
        attributes.as_ptr(),
    )
}

fn surface() -> *mut c_void {
    WindowSurface::create(core::ptr::null_mut())
}

/// eglMakeCurrent records (ctx, draw, read, display) as THIS thread's current binding, and the
/// eglGetCurrent* getters return exactly those; EGL_NO_CONTEXT (null) clears the whole binding.
#[test]
fn make_current_round_trips_and_no_context_clears() {
    let dpy = DISPLAY_TOKEN as *mut c_void;
    let draw = surface();
    let read = surface();
    let ctx = context();

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

#[test]
fn swap_rejects_a_surface_other_than_the_current_draw_surface() {
    let display = DISPLAY_TOKEN as *mut c_void;
    let draw = surface();
    let other = surface();
    let context = context();
    assert_eq!(eglMakeCurrent(display, draw, draw, context), EGL_TRUE);

    assert_eq!(eglSwapBuffers(display, other), EGL_FALSE);
    assert_eq!(eglGetError(), EGL_BAD_SURFACE);
    assert_eq!(eglGetCurrentSurface(EGL_DRAW), draw);

    assert_eq!(
        eglMakeCurrent(
            display,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        ),
        EGL_TRUE
    );
    assert_eq!(eglDestroySurface(display, draw), EGL_TRUE);
    assert_eq!(eglDestroySurface(display, other), EGL_TRUE);
    assert_eq!(eglDestroyContext(display, context), EGL_TRUE);
}

#[test]
fn release_thread_unbinds_and_finalizes_pending_context_destruction() {
    let display = DISPLAY_TOKEN as *mut c_void;
    let context = context();
    assert_eq!(
        eglMakeCurrent(
            display,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            context,
        ),
        EGL_TRUE
    );
    assert_eq!(eglDestroyContext(display, context), EGL_TRUE);
    assert!(GlobalState::access(|state| state
        .context_attributes(context as usize)
        .is_some()));

    assert_eq!(eglReleaseThread(), EGL_TRUE);

    assert!(eglGetCurrentContext().is_null());
    assert!(
        GlobalState::access(|state| state.context_attributes(context as usize).is_none()),
        "eglReleaseThread left a pending-destroy context bound"
    );
}

/// A getter on a thread that never made a context current returns null (EGL_NO_*), and one thread's
/// current binding is INDEPENDENT of another's — the thread-local guarantee libepoxy relies on.
#[test]
fn current_binding_is_thread_local_and_independent() {
    let ctx_a = context() as usize;
    let surface_a = surface();
    eglMakeCurrent(
        DISPLAY_TOKEN as *mut c_void,
        surface_a,
        surface_a,
        ctx_a as *mut c_void,
    );
    assert_eq!(eglGetCurrentContext() as usize, ctx_a);

    let ctx_b = context() as usize;
    let surface_b = surface() as usize;
    let observed = std::thread::spawn(move || {
        // A fresh thread has NO current context, regardless of the parent's binding.
        let before = eglGetCurrentContext();
        // It can make its OWN context current without disturbing the parent.
        eglMakeCurrent(
            DISPLAY_TOKEN as *mut c_void,
            surface_b as *mut c_void,
            surface_b as *mut c_void,
            ctx_b as *mut c_void,
        );
        let current = eglGetCurrentContext() as usize;
        eglMakeCurrent(
            DISPLAY_TOKEN as *mut c_void,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        );
        (before as usize, current)
    })
    .join()
    .unwrap();

    assert_eq!(observed.0, 0, "a fresh thread starts with EGL_NO_CONTEXT");
    assert_eq!(observed.1, ctx_b, "the child thread bound its own context");
    // The parent's binding is untouched by the child.
    assert_eq!(
        eglGetCurrentContext() as usize,
        ctx_a,
        "the parent thread's current context is independent"
    );
    // Cleanup this thread's binding.
    eglMakeCurrent(
        DISPLAY_TOKEN as *mut c_void,
        core::ptr::null_mut(),
        core::ptr::null_mut(),
        core::ptr::null_mut(),
    );
}

/// EGL 1.4 §3.1: a command that SUCCEEDS sets the error status to `EGL_SUCCESS`; only a failing command
/// leaves an error behind.
///
/// This test previously asserted the opposite — "successful calls must not clear a pending error" — and
/// so defended the defect it was best placed to catch. The consequence was not cosmetic: the near
/// universal error-checking idiom (dEQP's `EGLU_CHECK_CALL`, and every toolkit that wraps EGL the same
/// way) reads `eglGetError()` and IGNORES return values, so one legitimately-failing call poisoned every
/// later check. dEQP-GLES2 — 17,485 spec-derived cases — aborted before running a single test and blamed
/// the innocent call that followed. This test was green throughout.
#[test]
fn a_successful_call_clears_the_error_a_failing_one_left() {
    // A real failing EGL call leaves a pending error on this thread.
    assert_eq!(eglBindAPI(EGL_OPENGL_API), EGL_FALSE);
    let dpy = DISPLAY_TOKEN as *mut c_void;
    let ctx = context();

    // The next command SUCCEEDS, so the error it did not raise must not survive it.
    assert_eq!(
        eglMakeCurrent(dpy, core::ptr::null_mut(), core::ptr::null_mut(), ctx),
        EGL_TRUE
    );
    assert_eq!(
        eglGetError(),
        hl_gl::result::EGL_SUCCESS,
        "a succeeding command must report EGL_SUCCESS, not the previous command's error"
    );

    // A failing command still reports ITS error, and reading it resets the status.
    assert_eq!(eglBindAPI(EGL_OPENGL_API), EGL_FALSE);
    assert_eq!(eglGetError(), EGL_BAD_PARAMETER);
    assert_eq!(eglGetError(), hl_gl::result::EGL_SUCCESS);

    // Cleanup this thread's binding.
    eglMakeCurrent(
        DISPLAY_TOKEN as *mut c_void,
        core::ptr::null_mut(),
        core::ptr::null_mut(),
        core::ptr::null_mut(),
    );
}

#[test]
fn lifecycle_rejects_foreign_handles_and_defers_current_context_destroy() {
    let display = DISPLAY_TOKEN as *mut c_void;
    let ctx = context();

    assert_eq!(
        eglMakeCurrent(
            display,
            core::ptr::null_mut(),
            surface(),
            core::ptr::null_mut()
        ),
        EGL_FALSE
    );
    assert_eq!(eglGetError(), EGL_BAD_MATCH);
    assert_eq!(
        eglDestroyContext(0xDEADusize as *mut c_void, ctx),
        EGL_FALSE
    );
    assert_eq!(eglGetError(), EGL_BAD_DISPLAY);

    assert_eq!(
        eglMakeCurrent(display, core::ptr::null_mut(), core::ptr::null_mut(), ctx),
        EGL_TRUE
    );
    assert_eq!(eglDestroyContext(display, ctx), EGL_TRUE);
    assert_eq!(eglGetCurrentContext(), ctx);
    assert!(
        GlobalState::access(|state| state.context_attributes(ctx as usize).is_some()),
        "current context storage remains alive after destroy"
    );

    assert_eq!(
        eglMakeCurrent(
            display,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut()
        ),
        EGL_TRUE
    );
    assert!(
        GlobalState::access(|state| state.context_attributes(ctx as usize).is_none()),
        "releasing the last current binding finalizes deferred destruction"
    );
    assert_eq!(eglDestroyContext(display, ctx), EGL_FALSE);
    assert_eq!(eglGetError(), EGL_BAD_CONTEXT);
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
    let attributes = [
        EGL_CONTEXT_CLIENT_VERSION,
        3,
        EGL_CONTEXT_MINOR_VERSION_KHR,
        1,
        EGL_NONE,
    ];
    let ctx = eglCreateContext(
        dpy,
        0x1 as *mut c_void,
        core::ptr::null_mut(),
        attributes.as_ptr(),
    );
    assert!(!ctx.is_null());

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

    // A null out-param and an unknown attribute are rejected without a dereference.
    assert_eq!(
        eglQueryContext(dpy, ctx, EGL_CONTEXT_CLIENT_TYPE, core::ptr::null_mut()),
        EGL_FALSE,
        "a null value pointer is rejected"
    );
    let mut unknown: i32 = -455_764_240;
    assert_eq!(
        eglQueryContext(dpy, ctx, 0xBEEF, &mut unknown as *mut i32),
        EGL_FALSE
    );
    assert_eq!(unknown, -455_764_240);
    assert_eq!(eglDestroyContext(dpy, ctx), EGL_TRUE);
}

/// glGetIntegerv / glGetInteger64v ALWAYS write the out-param (never leave it as uninitialized
/// garbage): GL_MAX_TEXTURE_SIZE is the 8192 executor ceiling and an unknown pname writes 0.
#[test]
fn gl_get_integerv_always_writes_the_out_param() {
    let display = DISPLAY_TOKEN as *mut c_void;
    let context = context();
    assert_eq!(
        eglMakeCurrent(
            display,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            context
        ),
        EGL_TRUE
    );
    // Seed with a garbage sentinel; a correct getter overwrites it.
    let mut v: i32 = -455_764_240;
    glGetIntegerv(GL_MAX_TEXTURE_SIZE, &mut v as *mut i32);
    assert_eq!(
        v, 8192,
        "GL_MAX_TEXTURE_SIZE is the truthful executor ceiling, not garbage"
    );

    let mut v64: i64 = -1;
    glGetInteger64v(GL_MAX_TEXTURE_SIZE, &mut v64 as *mut i64);
    assert_eq!(
        v64, 8192,
        "glGetInteger64v writes the same truthful ceiling"
    );

    // An unhandled integer pname defaults to 0 — never the untouched garbage sentinel.
    let mut u: i32 = -455_764_240;
    glGetIntegerv(0xBEEF, &mut u as *mut i32);
    assert_eq!(
        u, 0,
        "an unknown pname writes 0, never uninitialized memory"
    );
    assert_eq!(
        eglMakeCurrent(
            display,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut()
        ),
        EGL_TRUE
    );
    assert_eq!(eglDestroyContext(display, context), EGL_TRUE);
}

/// After its share group dies, a context must be RELEASABLE and must say why it cannot be re-bound.
///
/// Both halves were wrong and in opposite directions. Releasing the binding acquired the outgoing
/// group's lease to write surface targets back into it, so a dead group refused
/// `eglMakeCurrent(dpy, NULL, NULL, NULL)` — the first step of the only recovery EGL defines — and left
/// the application bound to a context it could not get off. `eglDestroyContext` tolerated the very same
/// dead lease and carried on, which is what shows the strictness was an oversight rather than a policy.
///
/// And the refusal to re-bind reported `EGL_BAD_CONTEXT`, which tells an application it passed a handle
/// that was never valid. A correct one then re-checks the handle, finds nothing wrong, and has nowhere
/// left to go. `EGL_CONTEXT_LOST` is the code that means destroy this and build another.
#[test]
fn a_lost_context_can_be_released_and_reports_the_loss_rather_than_a_bad_handle() {
    let dpy = DISPLAY_TOKEN as *mut c_void;
    let draw = surface();
    let read = surface();
    let ctx = context();
    assert_eq!(eglMakeCurrent(dpy, draw, read, ctx), EGL_TRUE);

    crate::state::GlobalState::lose_current_group("test: GPU transport submission failed");

    assert_eq!(
        eglMakeCurrent(
            dpy,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut()
        ),
        EGL_TRUE,
        "releasing a lost context must succeed: there is nothing left to write into a dead group, and \
         an application that cannot unbind cannot recover"
    );

    assert_eq!(
        eglMakeCurrent(dpy, draw, read, ctx),
        EGL_FALSE,
        "a lost context must not bind"
    );
    assert_eq!(
        eglGetError(),
        EGL_CONTEXT_LOST,
        "the refusal must name the loss, not the handle"
    );
}
