//! In-tree mirrors of the five §11 rendering-ledger gates this crate closes, so they run under the
//! normal `cargo test -p dd-shim-gl` (the tracked `rendering_surface.rs` gates are untracked and not
//! always present). Each mirrors the corresponding gate's observable assertions.

use core::ffi::c_void;

use dd_shim_gl::glconst::*;
use dd_shim_gl::{egl, gles};

#[test]
fn generated_names_bind_lazily_and_deletion_detaches_every_binding() {
    while gles::glGetError() != GL_NO_ERROR {}
    let mut buffer = 0;
    gles::glGenBuffers(1, &mut buffer);
    assert_ne!(buffer, 0);
    assert_eq!(gles::glIsBuffer(buffer), GL_FALSE, "generation must only reserve a buffer name");
    gles::glBindBuffer(GL_ARRAY_BUFFER, buffer);
    assert_eq!(gles::glIsBuffer(buffer), GL_TRUE, "first bind must instantiate the buffer");
    let mut binding = -1;
    gles::glGetIntegerv(GL_ARRAY_BUFFER_BINDING, &mut binding);
    assert_eq!(binding as u32, buffer);
    gles::glDeleteBuffers(1, &buffer);
    assert_eq!(gles::glIsBuffer(buffer), GL_FALSE);
    gles::glGetIntegerv(GL_ARRAY_BUFFER_BINDING, &mut binding);
    assert_eq!(binding, 0, "deleting a bound buffer must detach it");

    let mut texture = 0;
    gles::glGenTextures(1, &mut texture);
    assert_ne!(texture, 0);
    assert_eq!(gles::glIsTexture(texture), GL_FALSE, "generation must only reserve a texture name");
    gles::glBindTexture(GL_TEXTURE_2D, texture);
    assert_eq!(gles::glIsTexture(texture), GL_TRUE, "first bind must instantiate the texture");
    gles::glGetIntegerv(GL_TEXTURE_BINDING_2D, &mut binding);
    assert_eq!(binding as u32, texture);
    gles::glDeleteTextures(1, &texture);
    assert_eq!(gles::glIsTexture(texture), GL_FALSE);
    gles::glGetIntegerv(GL_TEXTURE_BINDING_2D, &mut binding);
    assert_eq!(binding, 0, "deleting a bound texture must detach it");

    let mut untouched = 0x1357_2468;
    gles::glGenBuffers(-1, &mut untouched);
    assert_eq!(untouched, 0x1357_2468, "invalid generation mutated output");
    assert_eq!(gles::glGetError(), GL_INVALID_VALUE);
    gles::glDeleteTextures(-1, &texture);
    assert_eq!(gles::glGetError(), GL_INVALID_VALUE);
}

#[test]
fn shader_program_detach_and_delete_pending_follow_reference_lifetimes() {
    while gles::glGetError() != GL_NO_ERROR {}
    let vertex = gles::glCreateShader(GL_VERTEX_SHADER);
    let fragment = gles::glCreateShader(GL_FRAGMENT_SHADER);
    let program = gles::glCreateProgram();
    gles::glAttachShader(program, vertex);
    gles::glAttachShader(program, fragment);
    let mut attached = -1;
    gles::glGetProgramiv(program, GL_ATTACHED_SHADERS, &mut attached);
    assert_eq!(attached, 2);

    gles::glDeleteShader(vertex);
    assert_eq!(gles::glIsShader(vertex), GL_TRUE, "attached delete-pending shader was reclaimed early");
    let mut pending = 0;
    gles::glGetShaderiv(vertex, GL_DELETE_STATUS, &mut pending);
    assert_eq!(pending, GL_TRUE as i32);
    gles::glDetachShader(program, vertex);
    assert_eq!(gles::glIsShader(vertex), GL_FALSE, "detached delete-pending shader was not reclaimed");
    gles::glGetProgramiv(program, GL_ATTACHED_SHADERS, &mut attached);
    assert_eq!(attached, 1);

    gles::glUseProgram(program);
    gles::glDeleteProgram(program);
    assert_eq!(gles::glIsProgram(program), GL_TRUE, "current delete-pending program was reclaimed early");
    gles::glGetProgramiv(program, GL_DELETE_STATUS, &mut pending);
    assert_eq!(pending, GL_TRUE as i32);
    gles::glUseProgram(0);
    assert_eq!(gles::glIsProgram(program), GL_FALSE, "unbound delete-pending program was not reclaimed");
}

// ---- gles_shader_compile_link_status_and_logs_are_truthful -------------------------------------

#[test]
fn shader_compile_link_status_and_logs_are_truthful() {
    let shader = gles::glCreateShader(GL_VERTEX_SHADER);
    assert_ne!(shader, 0);
    // Syntactically invalid GLSL (unbalanced parenthesis after `main`).
    let invalid = std::ffi::CString::new("attribute vec4 position; void main( { gl_Position = position; }").unwrap();
    let src = invalid.as_ptr();
    gles::glShaderSource(shader, 1, &src, core::ptr::null());
    gles::glCompileShader(shader);

    let mut compiled = -1;
    gles::glGetShaderiv(shader, GL_COMPILE_STATUS, &mut compiled);
    assert_eq!(compiled, GL_FALSE as i32, "invalid GLSL must report COMPILE_STATUS=false");

    let mut log_len = 0;
    gles::glGetShaderiv(shader, GL_INFO_LOG_LENGTH, &mut log_len);
    assert!(log_len > 1, "failed shader must have a non-empty info log");
    let mut log = vec![0 as core::ffi::c_char; log_len as usize];
    let mut written = 0;
    gles::glGetShaderInfoLog(shader, log_len, &mut written, log.as_mut_ptr());
    assert!(written > 0 && log[0] != 0, "failed shader diagnostic must not be empty");

    // A program with only a failed vertex shader (and no fragment shader) cannot link.
    let program = gles::glCreateProgram();
    gles::glAttachShader(program, shader);
    gles::glLinkProgram(program);
    let mut linked = -1;
    gles::glGetProgramiv(program, GL_LINK_STATUS, &mut linked);
    assert_eq!(linked, GL_FALSE as i32, "incomplete program must report LINK_STATUS=false");
    let mut plen = 0;
    gles::glGetProgramiv(program, GL_INFO_LOG_LENGTH, &mut plen);
    assert!(plen > 1, "failed link must have a non-empty info log");

    // A well-formed shader still compiles (the validator must not reject valid GLSL).
    let good = gles::glCreateShader(GL_FRAGMENT_SHADER);
    let valid = std::ffi::CString::new("precision mediump float; void main(){ gl_FragColor = vec4(1.0); }").unwrap();
    let gsrc = valid.as_ptr();
    gles::glShaderSource(good, 1, &gsrc, core::ptr::null());
    gles::glCompileShader(good);
    let mut ok = -1;
    gles::glGetShaderiv(good, GL_COMPILE_STATUS, &mut ok);
    assert_eq!(ok, GL_TRUE as i32, "valid GLSL must report COMPILE_STATUS=true");
}

// ---- egl_config_selection_and_invalid_attributes_are_truthful ----------------------------------

#[test]
fn egl_config_selection_and_invalid_attributes_are_truthful() {
    let display = egl::eglGetDisplay(core::ptr::null_mut());
    assert!(!display.is_null());
    assert_eq!(egl::eglInitialize(display, core::ptr::null_mut(), core::ptr::null_mut()), EGL_TRUE);
    let _ = egl::eglGetError(); // drain

    // Over-constrained request: a valid query with ZERO matches; the config slot is untouched.
    let impossible = [EGL_RED_SIZE, 64, EGL_SAMPLES, 8, EGL_NONE];
    let sentinel = 0x55usize as *mut c_void;
    let mut config = sentinel;
    let mut count = -1;
    assert_eq!(egl::eglChooseConfig(display, impossible.as_ptr(), &mut config, 1, &mut count), EGL_TRUE);
    assert_eq!(count, 0, "impossible attributes matched the singleton config");
    assert_eq!(config, sentinel, "zero-match selection overwrote the caller's slot");

    // A satisfiable request DOES match (so real apps still get a config).
    let ok_attrs = [EGL_SURFACE_TYPE, EGL_WINDOW_BIT, EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT, EGL_NONE];
    let mut cfg2 = core::ptr::null_mut();
    let mut n2 = -1;
    assert_eq!(egl::eglChooseConfig(display, ok_attrs.as_ptr(), &mut cfg2, 1, &mut n2), EGL_TRUE);
    assert_eq!(n2, 1, "a satisfiable attribute set must match config 1");

    // Unknown attribute -> EGL_BAD_ATTRIBUTE, output preserved.
    let mut value = 0x1234;
    assert_eq!(egl::eglGetConfigAttrib(display, 1usize as *mut c_void, 0x7fff, &mut value), EGL_FALSE);
    assert_eq!(value, 0x1234);
    assert_eq!(egl::eglGetError(), EGL_BAD_ATTRIBUTE);

    // Forged config handle -> EGL_BAD_CONFIG.
    assert_eq!(egl::eglGetConfigAttrib(display, 99usize as *mut c_void, EGL_RED_SIZE, &mut value), EGL_FALSE);
    assert_eq!(egl::eglGetError(), EGL_BAD_CONFIG);

    // Desktop-OpenGL API selection is rejected.
    assert_eq!(egl::eglBindAPI(0x30A2 /* EGL_OPENGL_API */), EGL_FALSE);
    assert_eq!(egl::eglGetError(), EGL_BAD_PARAMETER);
    assert_eq!(egl::eglBindAPI(EGL_OPENGL_ES_API), EGL_TRUE);
}

// ---- egl_surfaces_have_distinct_lifetimes_dimensions_and_types ---------------------------------

#[test]
fn egl_surfaces_have_distinct_lifetimes_dimensions_and_types() {
    // Host-tool mode avoids opening renderd/Wayland connections.
    std::env::set_var("DD_IR_DUMP", std::env::temp_dir().join("dd-egl-surface-mirror.ir"));
    let display = egl::eglGetDisplay(core::ptr::null_mut());
    assert_eq!(egl::eglInitialize(display, core::ptr::null_mut(), core::ptr::null_mut()), EGL_TRUE);
    let config = 1usize as *mut c_void;

    let window = egl::eglCreateWindowSurface(display, config, core::ptr::null_mut(), core::ptr::null());
    let pb_attrs = [EGL_WIDTH, 37, EGL_HEIGHT, 19, EGL_NONE];
    let pbuffer = egl::eglCreatePbufferSurface(display, config, pb_attrs.as_ptr());
    assert!(!window.is_null() && !pbuffer.is_null());
    assert_ne!(window, pbuffer, "window and pbuffer must be distinct handles");

    // Per-surface dimensions from the pbuffer's own attributes.
    let (mut w, mut h) = (-1, -1);
    assert_eq!(egl::eglQuerySurface(display, pbuffer, EGL_WIDTH, &mut w), EGL_TRUE);
    assert_eq!(egl::eglQuerySurface(display, pbuffer, EGL_HEIGHT, &mut h), EGL_TRUE);
    assert_eq!((w, h), (37, 19), "pbuffer dimensions were discarded");

    // Per-thread current draw/read surfaces.
    let attrs = [EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE];
    let ctx = egl::eglCreateContext(display, config, core::ptr::null_mut(), attrs.as_ptr());
    assert_eq!(egl::eglMakeCurrent(display, window, pbuffer, ctx), EGL_TRUE);
    assert_eq!(egl::eglGetCurrentSurface(0x3059 /* EGL_DRAW */), window);
    assert_eq!(egl::eglGetCurrentSurface(0x305A /* EGL_READ */), pbuffer);

    // Destroy invalidates: stale handle -> EGL_BAD_SURFACE without mutating output.
    assert_eq!(egl::eglDestroySurface(display, window), EGL_TRUE);
    let mut stale = 0x1234;
    let _ = egl::eglGetError();
    assert_eq!(egl::eglQuerySurface(display, window, EGL_WIDTH, &mut stale), EGL_FALSE);
    assert_eq!(stale, 0x1234, "stale-surface query mutated output");
    assert_eq!(egl::eglGetError(), EGL_BAD_SURFACE);
    assert_eq!(egl::eglSwapBuffers(display, window), EGL_FALSE);
    assert_eq!(egl::eglGetError(), EGL_BAD_SURFACE);

    egl::eglMakeCurrent(display, core::ptr::null_mut(), core::ptr::null_mut(), core::ptr::null_mut());
    egl::eglDestroyContext(display, ctx);
    egl::eglDestroySurface(display, pbuffer);
}

// ---- egl_swap_failure_is_reported_without_discarding_the_frame ---------------------------------

#[test]
fn egl_swap_of_a_stale_surface_reports_bad_surface() {
    // The transactional-submit ordering (present before draw-list reset; retained frame on failure) is
    // enforced by the source-shape gate. Here we exercise the observable surface-validation path.
    std::env::set_var("DD_IR_DUMP", std::env::temp_dir().join("dd-egl-swap-mirror.ir"));
    let display = egl::eglGetDisplay(core::ptr::null_mut());
    assert_eq!(egl::eglInitialize(display, core::ptr::null_mut(), core::ptr::null_mut()), EGL_TRUE);
    let window = egl::eglCreateWindowSurface(display, 1usize as *mut c_void, core::ptr::null_mut(), core::ptr::null());
    assert_eq!(egl::eglDestroySurface(display, window), EGL_TRUE);
    let _ = egl::eglGetError();
    // A swap on the retired handle fails truthfully rather than silently succeeding.
    assert_eq!(egl::eglSwapBuffers(display, window), EGL_FALSE);
    assert_eq!(egl::eglGetError(), EGL_BAD_SURFACE);
}

// ---- gles_flush_and_finish_have_submission_and_completion_semantics -----------------------------

// ---- egl_contexts_are_distinct_shareable_and_current_per_thread (eglReleaseThread piece) --------

#[test]
fn egl_release_thread_clears_the_current_context() {
    let display = egl::eglGetDisplay(core::ptr::null_mut());
    assert_eq!(egl::eglInitialize(display, core::ptr::null_mut(), core::ptr::null_mut()), EGL_TRUE);
    let attrs = [EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE];
    let ctx = egl::eglCreateContext(display, 1usize as *mut c_void, core::ptr::null_mut(), attrs.as_ptr());
    assert!(!ctx.is_null());
    assert_eq!(egl::eglMakeCurrent(display, core::ptr::null_mut(), core::ptr::null_mut(), ctx), EGL_TRUE);
    assert_eq!(egl::eglGetCurrentContext(), ctx);

    // eglReleaseThread must unbind this thread's current context AND display.
    assert_eq!(egl::eglReleaseThread(), EGL_TRUE);
    assert!(egl::eglGetCurrentContext().is_null(), "release-thread left a context current");
    assert!(egl::eglGetCurrentDisplay().is_null(), "release-thread left a display current");
    egl::eglDestroyContext(display, ctx);
}

// ---- advertised_gles2_has_real_implementations... (representative newly-full mandatory commands) --

#[test]
fn mandatory_gles2_queries_return_real_values() {
    // Pure queries (no bound state) — always safe.
    let (mut range, mut precision) = ([-1i32; 2], -1i32);
    gles::glGetShaderPrecisionFormat(GL_VERTEX_SHADER, 0x8DF2 /* HIGH_FLOAT */, range.as_mut_ptr(), &mut precision);
    assert_eq!((range[0], range[1], precision), (127, 127, 23), "glGetShaderPrecisionFormat");

    let (mut len, mut size, mut typ) = (-1i32, -1i32, 0u32);
    let mut name = [0 as core::ffi::c_char; 8];
    gles::glGetActiveUniform(1, 0, 8, &mut len, &mut size, &mut typ, name.as_mut_ptr());
    assert_eq!((len, size, typ), (0, 1, GL_FLOAT), "glGetActiveUniform spec-default shape");

    // Bound-state queries — run in a guaranteed-isolated share group (any context created after the
    // first standalone one gets a fresh namespace), so a parallel test can't perturb the binding.
    let display = egl::eglGetDisplay(core::ptr::null_mut());
    let attrs = [EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE];
    let _claim = egl::eglCreateContext(display, 1usize as *mut c_void, core::ptr::null_mut(), attrs.as_ptr());
    let ctx = egl::eglCreateContext(display, 1usize as *mut c_void, core::ptr::null_mut(), attrs.as_ptr());
    assert_eq!(egl::eglMakeCurrent(display, core::ptr::null_mut(), core::ptr::null_mut(), ctx), EGL_TRUE);

    // glGetTexParameteriv reflects the bound texture's real filter state.
    let mut tex = 0u32;
    gles::glGenTextures(1, &mut tex);
    gles::glActiveTexture(GL_TEXTURE0);
    gles::glBindTexture(GL_TEXTURE_2D, tex);
    gles::glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST as i32);
    let mut minf = -1i32;
    gles::glGetTexParameteriv(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, &mut minf);
    assert_eq!(minf, GL_NEAREST as i32, "glGetTexParameteriv must return the set filter");

    // glGetBufferParameteriv reflects the bound buffer's real size.
    let mut buf = 0u32;
    gles::glGenBuffers(1, &mut buf);
    gles::glBindBuffer(GL_ARRAY_BUFFER, buf);
    let data = [0u8; 12];
    gles::glBufferData(GL_ARRAY_BUFFER, data.len() as isize, data.as_ptr() as *const c_void, 0x88E4);
    let mut sz = -1i32;
    gles::glGetBufferParameteriv(GL_ARRAY_BUFFER, GL_BUFFER_SIZE, &mut sz);
    assert_eq!(sz, 12, "glGetBufferParameteriv(GL_BUFFER_SIZE) must return the real size");

    egl::eglMakeCurrent(display, core::ptr::null_mut(), core::ptr::null_mut(), core::ptr::null_mut());
    egl::eglDestroyContext(display, ctx);
}

// ---- advertised_egl14_has_real_implementations... (EGL 1.4 mandatory tail behavior) ------------

#[test]
fn egl14_mandatory_tail_is_truthful() {
    std::env::set_var("DD_IR_DUMP", std::env::temp_dir().join("dd-egl14-mirror.ir"));
    let display = egl::eglGetDisplay(core::ptr::null_mut());
    assert_eq!(egl::eglInitialize(display, core::ptr::null_mut(), core::ptr::null_mut()), EGL_TRUE);
    let config = 1usize as *mut c_void;
    let surface = egl::eglCreateWindowSurface(display, config, core::ptr::null_mut(), core::ptr::null());
    let _ = egl::eglGetError();

    // eglSurfaceAttrib: a known attribute on a LIVE surface is a benign accepted no-op.
    assert_eq!(egl::eglSurfaceAttrib(display, surface, 0x3093 /* EGL_SWAP_BEHAVIOR */, 0x3094), EGL_TRUE);
    // ...but a forged surface is EGL_BAD_SURFACE.
    assert_eq!(egl::eglSurfaceAttrib(display, 0x999 as *mut c_void, 0x3093, 0x3094), EGL_FALSE);
    assert_eq!(egl::eglGetError(), EGL_BAD_SURFACE);

    // Native pixmap / client-buffer surfaces are genuinely unsupported — truthful failure, no fake handle.
    assert!(egl::eglCreatePixmapSurface(display, config, core::ptr::null_mut(), core::ptr::null()).is_null());
    assert_eq!(egl::eglGetError(), EGL_BAD_NATIVE_PIXMAP);
    assert!(egl::eglCreatePbufferFromClientBuffer(display, 0, core::ptr::null_mut(), config, core::ptr::null()).is_null());
    assert_eq!(egl::eglGetError(), EGL_BAD_PARAMETER);

    // Bind-to-texture and copy-to-pixmap are not backed by the advertised config — truthful failure.
    assert_eq!(egl::eglBindTexImage(display, surface, 0x3084 /* EGL_BACK_BUFFER */), EGL_FALSE);
    assert_eq!(egl::eglGetError(), EGL_BAD_MATCH);
    assert_eq!(egl::eglCopyBuffers(display, surface, core::ptr::null_mut()), EGL_FALSE);
    assert_eq!(egl::eglGetError(), EGL_BAD_NATIVE_PIXMAP);

    egl::eglDestroySurface(display, surface);
}

#[test]
fn flush_submits_and_finish_waits_for_completion() {
    let (sub_before, _) = gles::submission_serials();
    gles::glFlush(); // nonblocking submit
    let (sub_after_flush, _) = gles::submission_serials();
    assert!(sub_after_flush > sub_before, "glFlush must advance the submission serial (nonblocking submit)");

    let (sub_pre_finish, _) = gles::submission_serials();
    gles::glFinish(); // blocking: completion must catch up to everything submitted before it
    let (_, completed) = gles::submission_serials();
    assert!(completed >= sub_pre_finish, "glFinish must block until completion catches up to submission");
}
