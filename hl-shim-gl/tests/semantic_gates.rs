//! In-tree mirrors of the five §11 rendering-ledger gates this crate closes, so they run under the
//! normal `cargo test -p hl-shim-gl` (the tracked `rendering_surface.rs` gates are untracked and not
//! always present). Each mirrors the corresponding gate's observable assertions.

use core::ffi::c_void;

use hl_shim_gl::glconst::*;
use hl_shim_gl::{egl, gles};

fn serial_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Reset the shared default GL share-group while holding the serialization lock, so residual state
    // from a prior (serialized) test can never leak in and cause order-dependent parallel failures.
    gles::reset_gl_state_for_tests();
    g
}

#[test]
fn egl_query_context_validates_lifetime_and_preserves_output() {
    let _serial = serial_guard();
    let dpy=egl::eglGetDisplay(core::ptr::null_mut());
    let attrs=[EGL_CONTEXT_CLIENT_VERSION,2,EGL_NONE];
    let ctx=egl::eglCreateContext(dpy,1usize as *mut c_void,core::ptr::null_mut(),attrs.as_ptr());
    for (attr,want) in [(EGL_CONTEXT_CLIENT_VERSION,2),(EGL_CONTEXT_CLIENT_TYPE,EGL_OPENGL_ES_API as i32),(EGL_CONFIG_ID,1),(EGL_RENDER_BUFFER,EGL_BACK_BUFFER)] {
        let mut value=-1;assert_eq!(egl::eglQueryContext(dpy,ctx,attr,&mut value),EGL_TRUE);assert_eq!(value,want);
    }
    let mut sentinel=0x1234;
    assert_eq!(egl::eglQueryContext(99usize as *mut c_void,ctx,EGL_CONTEXT_CLIENT_VERSION,&mut sentinel),EGL_FALSE);assert_eq!(sentinel,0x1234);assert_eq!(egl::eglGetError(),EGL_BAD_DISPLAY);
    assert_eq!(egl::eglQueryContext(dpy,ctx,0x7fff,&mut sentinel),EGL_FALSE);assert_eq!(sentinel,0x1234);assert_eq!(egl::eglGetError(),EGL_BAD_ATTRIBUTE);
    assert_eq!(egl::eglMakeCurrent(dpy,core::ptr::null_mut(),core::ptr::null_mut(),ctx),EGL_TRUE);
    assert_eq!(egl::eglDestroyContext(dpy,ctx),EGL_TRUE);
    assert_eq!(egl::eglQueryContext(dpy,ctx,EGL_CONTEXT_CLIENT_VERSION,&mut sentinel),EGL_FALSE);assert_eq!(sentinel,0x1234);assert_eq!(egl::eglGetError(),EGL_BAD_CONTEXT);
    assert_eq!(egl::eglReleaseThread(),EGL_TRUE);
}

#[test]
fn sync_objects_track_submission_completion_and_stale_handles() {
    let _serial = serial_guard();
    while gles::glGetError()!=GL_NO_ERROR{}
    let sync=gles::glFenceSync(GL_SYNC_GPU_COMMANDS_COMPLETE,0); assert!(!sync.is_null()); assert_eq!(gles::glIsSync(sync),GL_TRUE);
    let mut status=-1;let mut len=0;gles::glGetSynciv(sync,GL_SYNC_STATUS,1,&mut len,&mut status);assert_eq!(status,GL_UNSIGNALED);
    assert_eq!(gles::glClientWaitSync(sync,0,0),GL_TIMEOUT_EXPIRED);
    assert_eq!(gles::glClientWaitSync(sync,GL_SYNC_FLUSH_COMMANDS_BIT,1),GL_CONDITION_SATISFIED);
    assert_eq!(gles::glClientWaitSync(sync,0,0),GL_ALREADY_SIGNALED);
    gles::glDeleteSync(sync);assert_eq!(gles::glIsSync(sync),GL_FALSE);
    let mut sentinel=0x1234;gles::glGetSynciv(sync,GL_SYNC_STATUS,1,&mut len,&mut sentinel);assert_eq!(sentinel,0x1234);assert_eq!(gles::glGetError(),GL_INVALID_VALUE);
    assert_eq!(gles::glClientWaitSync(sync,0,0),GL_WAIT_FAILED);assert_eq!(gles::glGetError(),GL_INVALID_VALUE);
}

// ---- gles_sync_objects_track_real_submission_completion_and_wait_results (frame-ack coupling) ----

#[test]
fn sync_completion_advances_on_real_frame_submission_ack() {
    let _serial = serial_guard();
    // Host-tool mode: present_frame writes the IR and returns Ok (a synchronous stand-in for the
    // transport's ACK_OK), so the completion boundary is exercised without a live executor socket.
    std::env::set_var("DD_IR_DUMP", std::env::temp_dir().join("dd-sync-present-mirror.ir"));
    while gles::glGetError() != GL_NO_ERROR {}
    let display = egl::eglGetDisplay(core::ptr::null_mut());
    assert_eq!(egl::eglInitialize(display, core::ptr::null_mut(), core::ptr::null_mut()), EGL_TRUE);
    // Present on the DEFAULT share group (no context made current — like the flush/finish gate), so the
    // group `eglCreateWindowSurface` brought the surface up on is the same one `glClear`/`eglSwapBuffers`
    // operate on. (A separately-current context can land in a fresh share group whose surf isn't "up".)
    let surface = egl::eglCreateWindowSurface(display, 1usize as *mut c_void, core::ptr::null_mut(), core::ptr::null());
    assert!(!surface.is_null());
    let _ = egl::eglGetError();

    // A fence created now captures the current submission serial and is UNSIGNALED: no frame carrying
    // its work has been submitted+acked yet. A zero-timeout, no-flush wait must report timeout (it must
    // NOT locally force-complete).
    let sync = gles::glFenceSync(GL_SYNC_GPU_COMMANDS_COMPLETE, 0);
    assert!(!sync.is_null());
    let mut status = -1;
    let mut len = 0;
    gles::glGetSynciv(sync, GL_SYNC_STATUS, 1, &mut len, &mut status);
    assert_eq!(status, GL_UNSIGNALED, "fence signaled before any frame was submitted");
    assert_eq!(
        gles::glClientWaitSync(sync, 0, 0),
        GL_TIMEOUT_EXPIRED,
        "a no-flush zero-timeout wait must not locally complete an un-acked fence"
    );

    // Record a frame and present it. The successful submit+ack (present_frame -> Ok) advances the REAL
    // completion serial past the fence — so it is now signaled WITHOUT any glFinish flush.
    gles::glClear(GL_COLOR_BUFFER_BIT);
    assert_eq!(egl::eglSwapBuffers(display, surface), EGL_TRUE);
    gles::glGetSynciv(sync, GL_SYNC_STATUS, 1, &mut len, &mut status);
    assert_eq!(status, GL_SIGNALED, "a frame ack must complete an earlier fence");
    assert_eq!(
        gles::glClientWaitSync(sync, 0, 0),
        GL_ALREADY_SIGNALED,
        "fence must report already-signaled after the frame ack, with no flush"
    );

    gles::glDeleteSync(sync);
    egl::eglDestroySurface(display, surface);
}

#[test]
fn texture_upload_validation_is_atomic_and_honors_padded_rows() {
    let _serial = serial_guard();
    while gles::glGetError() != GL_NO_ERROR {}
    let mut tex=0; gles::glGenTextures(1,&mut tex); gles::glBindTexture(GL_TEXTURE_2D,tex);
    let base=[7u8;16]; gles::glTexImage2D(GL_TEXTURE_2D,0,GL_RGBA as i32,2,2,0,GL_RGBA,GL_UNSIGNED_BYTE,base.as_ptr().cast());
    let bad=[9u8;16]; gles::glTexImage2D(GL_TEXTURE_2D,0,GL_RGBA as i32,2,2,1,GL_RGBA,GL_UNSIGNED_BYTE,bad.as_ptr().cast());
    assert_eq!(gles::glGetError(),GL_INVALID_VALUE);
    gles::glPixelStorei(GL_UNPACK_ALIGNMENT,8); gles::glPixelStorei(GL_UNPACK_ROW_LENGTH,3);
    let padded:[u8;24]=[1,2,3,4,5,6,7,8,0,0,0,0,0,0,0,0,9,10,11,12,13,14,15,16];
    gles::glTexImage2D(GL_TEXTURE_2D,0,GL_RGBA as i32,2,2,0,GL_RGBA,GL_UNSIGNED_BYTE,padded.as_ptr().cast());
    assert_eq!(gles::glGetError(),GL_NO_ERROR);
    let mut f=0; gles::glGenFramebuffers(1,&mut f); gles::glBindFramebuffer(GL_FRAMEBUFFER,f); gles::glFramebufferTexture2D(GL_FRAMEBUFFER,GL_COLOR_ATTACHMENT0,GL_TEXTURE_2D,tex,0);
    gles::glPixelStorei(GL_PACK_ALIGNMENT,1); gles::glPixelStorei(GL_PACK_ROW_LENGTH,0);
    let mut out=[0u8;16]; gles::glReadPixels(0,0,2,2,GL_RGBA,GL_UNSIGNED_BYTE,out.as_mut_ptr().cast());
    assert_eq!(out,[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16]);
    let generation_before=gles::texture_generation(tex);
    gles::glTexImage2D(GL_TEXTURE_2D,0,GL_RGBA as i32,i32::MAX,i32::MAX,0,GL_RGBA,GL_UNSIGNED_BYTE,bad.as_ptr().cast());
    assert_eq!(gles::glGetError(),GL_INVALID_VALUE,"overflowing upload was accepted");
    let mut unchanged=[0u8;16]; gles::glReadPixels(0,0,2,2,GL_RGBA,GL_UNSIGNED_BYTE,unchanged.as_mut_ptr().cast());
    assert_eq!(unchanged,out,"overflowing upload mutated texture storage");
    assert_eq!(gles::texture_generation(tex),generation_before,"overflowing upload advanced texture generation");
    gles::glTexStorage2D(GL_TEXTURE_2D,1,GL_RGBA,2,2);
    assert_eq!(gles::glGetError(),GL_NO_ERROR);
    gles::glTexImage2D(GL_TEXTURE_2D,0,GL_RGBA as i32,1,1,0,GL_RGBA,GL_UNSIGNED_BYTE,bad.as_ptr().cast());
    assert_eq!(gles::glGetError(),GL_INVALID_OPERATION,"immutable texture was redefined");
    gles::glTexSubImage2D(GL_TEXTURE_2D,0,1,1,2,2,GL_RGBA,GL_UNSIGNED_BYTE,bad.as_ptr().cast());
    assert_eq!(gles::glGetError(),GL_INVALID_VALUE,"out-of-bounds subimage succeeded");
}

#[test]
fn readpixels_validates_pack_layout_and_preserves_output() {
    let _serial = serial_guard();
    while gles::glGetError() != GL_NO_ERROR {}
    let mut tex = 0;
    gles::glGenTextures(1, &mut tex);
    gles::glBindTexture(GL_TEXTURE_2D, tex);
    let pixels: [u8; 16] = [1,2,3,4, 5,6,7,8, 9,10,11,12, 13,14,15,16];
    gles::glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA as i32, 2, 2, 0, GL_RGBA, GL_UNSIGNED_BYTE, pixels.as_ptr().cast());
    let mut fbo = 0;
    gles::glGenFramebuffers(1, &mut fbo);
    gles::glBindFramebuffer(GL_FRAMEBUFFER, fbo);
    gles::glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);

    let mut out = [0xA5u8; 48];
    let serial_before_zero = gles::submission_serials();
    gles::glReadPixels(-99, -99, 0, 2, GL_RGBA, GL_UNSIGNED_BYTE, core::ptr::null_mut());
    assert_eq!(gles::glGetError(), GL_NO_ERROR);
    assert_eq!(gles::submission_serials(), serial_before_zero);
    assert_eq!(out, [0xA5; 48]);
    gles::glReadPixels(0, 0, 0, 0, GL_RGBA, GL_FLOAT, core::ptr::null_mut());
    assert_eq!(gles::glGetError(), GL_INVALID_ENUM);
    gles::glReadPixels(0, 0, 2, 2, GL_RGBA, GL_FLOAT, out.as_mut_ptr().cast());
    assert_eq!(gles::glGetError(), GL_INVALID_ENUM);
    assert_eq!(out, [0xA5; 48]);
    gles::glReadPixels(-1, 0, 2, 2, GL_RGBA, GL_UNSIGNED_BYTE, out.as_mut_ptr().cast());
    assert_eq!(gles::glGetError(), GL_INVALID_VALUE);
    assert_eq!(out, [0xA5; 48]);

    gles::glPixelStorei(GL_PACK_ALIGNMENT, 8);
    gles::glPixelStorei(GL_PACK_ROW_LENGTH, 3);
    gles::glPixelStorei(GL_PACK_SKIP_ROWS, 1);
    gles::glPixelStorei(GL_PACK_SKIP_PIXELS, 1);
    let before = gles::submission_serials().0;
    gles::glReadPixels(0, 0, 2, 2, GL_RGBA, GL_UNSIGNED_BYTE, out.as_mut_ptr().cast());
    assert_eq!(gles::glGetError(), GL_NO_ERROR);
    assert!(gles::submission_serials().1 > before, "readback did not synchronize completion");
    assert_eq!(&out[20..28], &pixels[0..8]);
    assert_eq!(&out[36..44], &pixels[8..16]);
    assert!(out[..20].iter().all(|&b| b == 0xA5));
    assert!(out[28..36].iter().all(|&b| b == 0xA5), "row padding was overwritten");

    gles::glPixelStorei(GL_PACK_ALIGNMENT, 1);
    gles::glPixelStorei(GL_PACK_ROW_LENGTH, 0);
    gles::glPixelStorei(GL_PACK_SKIP_ROWS, 0);
    gles::glPixelStorei(GL_PACK_SKIP_PIXELS, 0);
    let mut pbo = 0;
    gles::glGenBuffers(1, &mut pbo);
    gles::glBindBuffer(GL_PIXEL_PACK_BUFFER, pbo);
    let initial = [0xCCu8; 20];
    gles::glBufferData(GL_PIXEL_PACK_BUFFER, 20, initial.as_ptr().cast(), 0x88E4);
    gles::glReadPixels(0, 0, 2, 2, GL_RGBA, GL_UNSIGNED_BYTE, 8usize as *mut core::ffi::c_void);
    assert_eq!(gles::glGetError(), GL_INVALID_OPERATION, "out-of-range PBO write succeeded");
    let mapped = gles::glMapBufferRange(GL_PIXEL_PACK_BUFFER, 0, 20, 0) as *const u8;
    assert_eq!(unsafe { core::slice::from_raw_parts(mapped, 20) }, &initial);
    gles::glReadPixels(0, 0, 2, 2, GL_RGBA, GL_UNSIGNED_BYTE, 4usize as *mut core::ffi::c_void);
    assert_eq!(gles::glGetError(), GL_NO_ERROR);
    assert_eq!(unsafe { core::slice::from_raw_parts(mapped.add(4), 16) }, &pixels);
    gles::glDeleteBuffers(1, &pbo);
    let mut pack_binding = -1;
    gles::glGetIntegerv(GL_PIXEL_PACK_BUFFER_BINDING, &mut pack_binding);
    assert_eq!(pack_binding, 0, "deleting a PBO left its pack binding stale");

    gles::glPixelStorei(GL_PACK_ALIGNMENT, 3);
    assert_eq!(gles::glGetError(), GL_INVALID_VALUE);
    gles::glBindFramebuffer(GL_FRAMEBUFFER, 0);
    gles::glDeleteFramebuffers(1, &fbo);
    gles::glDeleteTextures(1, &tex);
}

// ---- gles_pixel_store_and_texture_upload_validation_is_atomic_and_checked (compressed/3D) --------

#[test]
fn compressed_texture_upload_is_atomically_validated() {
    let _serial = serial_guard();
    while gles::glGetError() != GL_NO_ERROR {}
    const ETC2_RGB8: u32 = 0x9274; // 8 bytes / 4x4 block
    const ETC2_RGBA8: u32 = 0x9278; // 16 bytes / 4x4 block

    let mut tex = 0;
    gles::glGenTextures(1, &mut tex);
    gles::glBindTexture(GL_TEXTURE_2D, tex);
    // A defined 8x8 RGBA8 base gives the sub-image bounds/atomicity checks something to reference.
    gles::glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA as i32, 8, 8, 0, GL_RGBA, GL_UNSIGNED_BYTE, core::ptr::null());
    assert_eq!(gles::glGetError(), GL_NO_ERROR);
    let gen0 = gles::texture_generation(tex);

    // Non-compressed internalformat → GL_INVALID_ENUM.
    gles::glCompressedTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, 8, 8, 0, 128, core::ptr::null());
    assert_eq!(gles::glGetError(), GL_INVALID_ENUM);
    // Wrong imageSize (8x8 ETC2-RGB8 must be ceil(8/4)*ceil(8/4)*8 = 32) → GL_INVALID_VALUE.
    gles::glCompressedTexImage2D(GL_TEXTURE_2D, 0, ETC2_RGB8, 8, 8, 0, 31, core::ptr::null());
    assert_eq!(gles::glGetError(), GL_INVALID_VALUE);
    // border != 0 → GL_INVALID_VALUE.
    gles::glCompressedTexImage2D(GL_TEXTURE_2D, 0, ETC2_RGB8, 8, 8, 1, 32, core::ptr::null());
    assert_eq!(gles::glGetError(), GL_INVALID_VALUE);
    // Correct 8x8 ETC2-RGB8 (32 bytes) and 4x4 ETC2-RGBA8 (16 bytes) uploads are accepted.
    gles::glCompressedTexImage2D(GL_TEXTURE_2D, 0, ETC2_RGB8, 8, 8, 0, 32, core::ptr::null());
    assert_eq!(gles::glGetError(), GL_NO_ERROR);
    gles::glCompressedTexImage2D(GL_TEXTURE_2D, 0, ETC2_RGBA8, 4, 4, 0, 16, core::ptr::null());
    assert_eq!(gles::glGetError(), GL_NO_ERROR);

    // No live texture bound → GL_INVALID_OPERATION.
    gles::glBindTexture(GL_TEXTURE_2D, 0);
    gles::glCompressedTexImage2D(GL_TEXTURE_2D, 0, ETC2_RGB8, 8, 8, 0, 32, core::ptr::null());
    assert_eq!(gles::glGetError(), GL_INVALID_OPERATION);
    gles::glBindTexture(GL_TEXTURE_2D, tex);

    // Sub-image: interior offset not a multiple of 4 → GL_INVALID_OPERATION.
    gles::glCompressedTexSubImage2D(GL_TEXTURE_2D, 0, 2, 0, 4, 4, ETC2_RGB8, 8, core::ptr::null());
    assert_eq!(gles::glGetError(), GL_INVALID_OPERATION);
    // Sub-image out of bounds (8 + 4 > 8) → GL_INVALID_OPERATION.
    gles::glCompressedTexSubImage2D(GL_TEXTURE_2D, 0, 8, 0, 4, 4, ETC2_RGB8, 8, core::ptr::null());
    assert_eq!(gles::glGetError(), GL_INVALID_OPERATION);
    // Valid block-aligned in-bounds sub-image (4x4 at (4,4), 8 bytes) accepted.
    gles::glCompressedTexSubImage2D(GL_TEXTURE_2D, 0, 4, 4, 4, 4, ETC2_RGB8, 8, core::ptr::null());
    assert_eq!(gles::glGetError(), GL_NO_ERROR);

    // 3D: a 2D target is rejected for the array/3D entry point → GL_INVALID_ENUM.
    gles::glCompressedTexImage3D(GL_TEXTURE_2D, 0, ETC2_RGBA8, 4, 4, 2, 0, 32, core::ptr::null());
    assert_eq!(gles::glGetError(), GL_INVALID_ENUM);
    // Correct 4x4x2 ETC2-RGBA8 (16 * 2 = 32) on the array target; wrong size rejected.
    gles::glCompressedTexImage3D(GL_TEXTURE_2D_ARRAY, 0, ETC2_RGBA8, 4, 4, 2, 0, 32, core::ptr::null());
    assert_eq!(gles::glGetError(), GL_NO_ERROR);
    gles::glCompressedTexImage3D(GL_TEXTURE_2D_ARRAY, 0, ETC2_RGBA8, 4, 4, 2, 0, 33, core::ptr::null());
    assert_eq!(gles::glGetError(), GL_INVALID_VALUE);

    // Atomicity: no rejected (or undecoded) compressed upload mutated the base texture's storage.
    assert_eq!(gles::texture_generation(tex), gen0, "a compressed upload perturbed texture storage");
    gles::glDeleteTextures(1, &tex);
}

// ---- gles_readpixels_validates_pack_layout_and_preserves_output_on_error (default-FB readback) ---

#[test]
fn readpixels_default_framebuffer_reads_zeros_without_error() {
    let _serial = serial_guard();
    while gles::glGetError() != GL_NO_ERROR {}
    gles::glBindFramebuffer(GL_FRAMEBUFFER, 0);
    // The default framebuffer is complete but the shim keeps no CPU default-color plane, so a readback
    // returns zeros (gl_shim.c parity) rather than raising GL_INVALID_OPERATION as it used to.
    let mut out = [0xABu8; 16];
    gles::glReadPixels(0, 0, 2, 2, GL_RGBA, GL_UNSIGNED_BYTE, out.as_mut_ptr().cast());
    assert_eq!(gles::glGetError(), GL_NO_ERROR, "default-FB readback must not error");
    assert_eq!(out, [0u8; 16], "default-FB readback must zero-fill the client buffer");

    // The same default-FB readback into a bound pack buffer (PBO) writes zeros over its prior contents.
    let mut pbo = 0;
    gles::glGenBuffers(1, &mut pbo);
    gles::glBindBuffer(GL_PIXEL_PACK_BUFFER, pbo);
    let init = [0x77u8; 16];
    gles::glBufferData(GL_PIXEL_PACK_BUFFER, 16, init.as_ptr().cast(), 0x88E4);
    gles::glReadPixels(0, 0, 2, 2, GL_RGBA, GL_UNSIGNED_BYTE, core::ptr::null_mut());
    assert_eq!(gles::glGetError(), GL_NO_ERROR);
    let mapped = gles::glMapBufferRange(GL_PIXEL_PACK_BUFFER, 0, 16, 0) as *const u8;
    assert_eq!(
        unsafe { core::slice::from_raw_parts(mapped, 16) },
        &[0u8; 16],
        "default-FB PBO readback must be zeros"
    );
    gles::glBindBuffer(GL_PIXEL_PACK_BUFFER, 0);
    gles::glDeleteBuffers(1, &pbo);
}

fn linked_test_program() -> u32 {
    fn compile(kind: u32, source: &str) -> u32 {
        let shader = gles::glCreateShader(kind);
        let source = std::ffi::CString::new(source).unwrap();
        let ptr = source.as_ptr();
        gles::glShaderSource(shader, 1, &ptr, core::ptr::null());
        gles::glCompileShader(shader);
        shader
    }
    let vertex = compile(GL_VERTEX_SHADER, "attribute vec2 p; void main(){ gl_Position=vec4(p,0.0,1.0); }");
    let fragment = compile(GL_FRAGMENT_SHADER, "precision mediump float; void main(){ gl_FragColor=vec4(1.0); }");
    let program = gles::glCreateProgram();
    gles::glAttachShader(program, vertex);
    gles::glAttachShader(program, fragment);
    gles::glLinkProgram(program);
    program
}

#[test]
fn draw_validation_rejects_inputs_before_recording() {
    let _serial = serial_guard();
    while gles::glGetError() != GL_NO_ERROR {}
    gles::glBindFramebuffer(GL_FRAMEBUFFER, 0);

    gles::glDrawArrays(0xFFFF, 0, 3);
    assert_eq!(gles::glGetError(), GL_INVALID_ENUM);
    gles::glDrawArrays(GL_TRIANGLES, -1, 3);
    assert_eq!(gles::glGetError(), GL_INVALID_VALUE);
    gles::glDrawArrays(GL_TRIANGLES, 0, 3);
    assert_eq!(gles::glGetError(), GL_INVALID_OPERATION, "draw without linked current program succeeded");

    let program = linked_test_program();
    gles::glUseProgram(program);
    let mut vbo = 0;
    gles::glGenBuffers(1, &mut vbo);
    gles::glBindBuffer(GL_ARRAY_BUFFER, vbo);
    let vertices = [0.0f32; 6];
    gles::glBufferData(
        GL_ARRAY_BUFFER,
        std::mem::size_of_val(&vertices) as isize,
        vertices.as_ptr().cast(),
        0x88E4,
    );
    gles::glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 0, core::ptr::null());
    gles::glEnableVertexAttribArray(0);
    gles::glDrawArrays(GL_TRIANGLES, 0, 4);
    assert_eq!(gles::glGetError(), GL_INVALID_OPERATION, "vertex range beyond the buffer succeeded");
    gles::glDrawArrays(GL_TRIANGLES, 0, 3);
    assert_eq!(gles::glGetError(), GL_NO_ERROR, "valid array draw was rejected");

    let mut ibo = 0;
    gles::glGenBuffers(1, &mut ibo);
    gles::glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, ibo);
    let indices = [0u16, 1, 2];
    gles::glBufferData(
        GL_ELEMENT_ARRAY_BUFFER,
        std::mem::size_of_val(&indices) as isize,
        indices.as_ptr().cast(),
        0x88E4,
    );
    gles::glDrawElements(GL_TRIANGLES, 3, GL_FLOAT, core::ptr::null());
    assert_eq!(gles::glGetError(), GL_INVALID_ENUM);
    gles::glDrawElements(GL_TRIANGLES, 3, GL_UNSIGNED_SHORT, 1usize as *const core::ffi::c_void);
    assert_eq!(gles::glGetError(), GL_INVALID_OPERATION, "misaligned index offset succeeded");
    gles::glDrawElements(GL_TRIANGLES, 4, GL_UNSIGNED_SHORT, core::ptr::null());
    assert_eq!(gles::glGetError(), GL_INVALID_OPERATION, "index range beyond the buffer succeeded");
    gles::glDrawElements(GL_TRIANGLES, 3, GL_UNSIGNED_SHORT, core::ptr::null());
    assert_eq!(gles::glGetError(), GL_NO_ERROR, "valid indexed draw was rejected");
}

// ---- gles_draw_calls_validate_all_inputs_before_snapshot_or_recording (mapped + limits) ---------

#[test]
fn draw_validation_rejects_mapped_buffers_and_over_limit_attribs() {
    let _serial = serial_guard();
    while gles::glGetError() != GL_NO_ERROR {}
    gles::glBindFramebuffer(GL_FRAMEBUFFER, 0);

    // Negotiated limit: an attribute index at/beyond GL_MAX_VERTEX_ATTRIBS (16) is out of range.
    gles::glVertexAttribPointer(16, 2, GL_FLOAT, GL_FALSE, 0, core::ptr::null());
    assert_eq!(gles::glGetError(), GL_INVALID_VALUE, "over-limit attrib index must be rejected");
    gles::glEnableVertexAttribArray(99);
    assert_eq!(gles::glGetError(), GL_INVALID_VALUE, "over-limit enable must be rejected");
    // Index 15 (the last legal slot) is accepted.
    gles::glVertexAttribPointer(15, 2, GL_FLOAT, GL_FALSE, 0, core::ptr::null());
    assert_eq!(gles::glGetError(), GL_NO_ERROR, "the last legal attrib index was rejected");

    // Set up a valid array draw, then map its VBO and prove the draw is refused while mapped.
    let program = linked_test_program();
    gles::glUseProgram(program);
    let mut vbo = 0;
    gles::glGenBuffers(1, &mut vbo);
    gles::glBindBuffer(GL_ARRAY_BUFFER, vbo);
    let vertices = [0.0f32; 6];
    gles::glBufferData(GL_ARRAY_BUFFER, std::mem::size_of_val(&vertices) as isize, vertices.as_ptr().cast(), 0x88E4);
    gles::glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 0, core::ptr::null());
    gles::glEnableVertexAttribArray(0);
    // Baseline: the draw is valid.
    gles::glDrawArrays(GL_TRIANGLES, 0, 3);
    assert_eq!(gles::glGetError(), GL_NO_ERROR, "valid array draw was rejected");
    let serial_before = gles::submission_serials();

    // Map the vertex buffer: a draw sourcing from it is now GL_INVALID_OPERATION and records nothing.
    let mapped = gles::glMapBufferRange(GL_ARRAY_BUFFER, 0, std::mem::size_of_val(&vertices) as isize, 0x0002);
    assert!(!mapped.is_null());
    gles::glDrawArrays(GL_TRIANGLES, 0, 3);
    assert_eq!(gles::glGetError(), GL_INVALID_OPERATION, "draw from a mapped vertex buffer must be rejected");
    assert_eq!(gles::submission_serials(), serial_before, "a rejected draw must not record/submit");

    // After unmapping, the same draw is valid again.
    assert_eq!(gles::glUnmapBuffer(GL_ARRAY_BUFFER), GL_TRUE);
    gles::glDrawArrays(GL_TRIANGLES, 0, 3);
    assert_eq!(gles::glGetError(), GL_NO_ERROR, "draw after unmap was rejected");

    // Indexed draw from a mapped element buffer is likewise rejected.
    let mut ibo = 0;
    gles::glGenBuffers(1, &mut ibo);
    gles::glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, ibo);
    let indices = [0u16, 1, 2];
    gles::glBufferData(GL_ELEMENT_ARRAY_BUFFER, std::mem::size_of_val(&indices) as isize, indices.as_ptr().cast(), 0x88E4);
    gles::glDrawElements(GL_TRIANGLES, 3, GL_UNSIGNED_SHORT, core::ptr::null());
    assert_eq!(gles::glGetError(), GL_NO_ERROR, "valid indexed draw was rejected");
    let em = gles::glMapBufferRange(GL_ELEMENT_ARRAY_BUFFER, 0, std::mem::size_of_val(&indices) as isize, 0x0002);
    assert!(!em.is_null());
    gles::glDrawElements(GL_TRIANGLES, 3, GL_UNSIGNED_SHORT, core::ptr::null());
    assert_eq!(gles::glGetError(), GL_INVALID_OPERATION, "indexed draw from a mapped element buffer must be rejected");
    assert_eq!(gles::glUnmapBuffer(GL_ELEMENT_ARRAY_BUFFER), GL_TRUE);

    gles::glDeleteBuffers(1, &vbo);
    gles::glDeleteBuffers(1, &ibo);
}

#[test]
fn framebuffer_completeness_tracks_color_attachment_and_blocks_draws() {
    let _serial = serial_guard();
    while gles::glGetError() != GL_NO_ERROR {}

    let mut fbo = 0;
    gles::glGenFramebuffers(1, &mut fbo);
    gles::glBindFramebuffer(GL_FRAMEBUFFER, fbo);
    assert_eq!(
        gles::glCheckFramebufferStatus(GL_FRAMEBUFFER),
        GL_FRAMEBUFFER_INCOMPLETE_MISSING_ATTACHMENT
    );

    // An incomplete FBO must reject before draw snapshot/recording side effects.
    gles::glDrawArrays(GL_TRIANGLES, 0, 3);
    assert_eq!(gles::glGetError(), GL_INVALID_FRAMEBUFFER_OPERATION);
    gles::glClear(GL_COLOR_BUFFER_BIT);
    assert_eq!(gles::glGetError(), GL_INVALID_FRAMEBUFFER_OPERATION);

    let mut texture = 0;
    gles::glGenTextures(1, &mut texture);
    gles::glBindTexture(GL_TEXTURE_2D, texture);
    gles::glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, texture, 0);
    assert_eq!(
        gles::glCheckFramebufferStatus(GL_FRAMEBUFFER),
        GL_FRAMEBUFFER_INCOMPLETE_ATTACHMENT,
        "a live texture without storage is not a complete attachment"
    );

    gles::glTexImage2D(
        GL_TEXTURE_2D,
        0,
        GL_RGBA as i32,
        8,
        4,
        0,
        GL_RGBA,
        GL_UNSIGNED_BYTE,
        core::ptr::null(),
    );
    assert_eq!(gles::glCheckFramebufferStatus(GL_FRAMEBUFFER), GL_FRAMEBUFFER_COMPLETE);

    // Deleting the attachment detaches it from the framebuffer and restores missing-attachment status.
    gles::glDeleteTextures(1, &texture);
    assert_eq!(
        gles::glCheckFramebufferStatus(GL_FRAMEBUFFER),
        GL_FRAMEBUFFER_INCOMPLETE_MISSING_ATTACHMENT
    );

    assert_eq!(gles::glCheckFramebufferStatus(0xDEAD), 0);
    assert_eq!(gles::glGetError(), GL_INVALID_ENUM);
    gles::glBindFramebuffer(GL_FRAMEBUFFER, 0);
    assert_eq!(gles::glCheckFramebufferStatus(GL_FRAMEBUFFER), GL_FRAMEBUFFER_COMPLETE);
    gles::glDeleteFramebuffers(1, &fbo);
}

// ---- gles_framebuffer_completeness_reflects_attachment_state_and_blocks_draws (depth/stencil) ----

#[test]
fn framebuffer_depth_stencil_completeness_and_read_blit_guards() {
    let _serial = serial_guard();
    while gles::glGetError() != GL_NO_ERROR {}
    const RGBA8: u32 = 0x8058;
    const DEPTH_COMPONENT16: u32 = 0x81A5;
    const DEPTH24_STENCIL8: u32 = 0x88F0;
    const DEPTH_STENCIL_ATTACHMENT: u32 = 0x821A;

    // A complete 8x4 color-texture FBO (unchanged color-only behavior).
    let mut fbo = 0;
    gles::glGenFramebuffers(1, &mut fbo);
    gles::glBindFramebuffer(GL_FRAMEBUFFER, fbo);
    let mut tex = 0;
    gles::glGenTextures(1, &mut tex);
    gles::glBindTexture(GL_TEXTURE_2D, tex);
    gles::glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA as i32, 8, 4, 0, GL_RGBA, GL_UNSIGNED_BYTE, core::ptr::null());
    gles::glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);
    assert_eq!(gles::glCheckFramebufferStatus(GL_FRAMEBUFFER), GL_FRAMEBUFFER_COMPLETE);

    // A matching 8x4 depth renderbuffer keeps it complete.
    let mut depth = 0;
    gles::glGenRenderbuffers(1, &mut depth);
    gles::glBindRenderbuffer(GL_RENDERBUFFER, depth);
    gles::glRenderbufferStorage(GL_RENDERBUFFER, DEPTH_COMPONENT16, 8, 4);
    gles::glFramebufferRenderbuffer(GL_FRAMEBUFFER, GL_DEPTH_ATTACHMENT, GL_RENDERBUFFER, depth);
    assert_eq!(gles::glGetError(), GL_NO_ERROR, "attaching a depth renderbuffer must be accepted");
    assert_eq!(gles::glCheckFramebufferStatus(GL_FRAMEBUFFER), GL_FRAMEBUFFER_COMPLETE);

    // A dimension mismatch (depth rbo resized to 16x4) makes the FBO incomplete.
    gles::glRenderbufferStorage(GL_RENDERBUFFER, DEPTH_COMPONENT16, 16, 4);
    assert_eq!(
        gles::glCheckFramebufferStatus(GL_FRAMEBUFFER),
        GL_FRAMEBUFFER_INCOMPLETE_DIMENSIONS,
        "mismatched depth/color dimensions must be incomplete"
    );
    // An incomplete draw FBO blocks draws and clears (attachment-state reflected).
    gles::glDrawArrays(GL_TRIANGLES, 0, 3);
    assert_eq!(gles::glGetError(), GL_INVALID_FRAMEBUFFER_OPERATION);
    // Restore a matching depth size → complete again.
    gles::glRenderbufferStorage(GL_RENDERBUFFER, DEPTH_COMPONENT16, 8, 4);
    assert_eq!(gles::glCheckFramebufferStatus(GL_FRAMEBUFFER), GL_FRAMEBUFFER_COMPLETE);

    // A stencil attachment whose format has no stencil aspect (a depth-only rbo) is incomplete.
    gles::glFramebufferRenderbuffer(GL_FRAMEBUFFER, GL_STENCIL_ATTACHMENT, GL_RENDERBUFFER, depth);
    assert_eq!(
        gles::glCheckFramebufferStatus(GL_FRAMEBUFFER),
        GL_FRAMEBUFFER_INCOMPLETE_ATTACHMENT,
        "a depth-only rbo cannot satisfy a stencil attachment"
    );
    // Detach the bad stencil; a combined DEPTH24_STENCIL8 rbo satisfies both aspects at once.
    gles::glFramebufferRenderbuffer(GL_FRAMEBUFFER, GL_STENCIL_ATTACHMENT, GL_RENDERBUFFER, 0);
    gles::glFramebufferRenderbuffer(GL_FRAMEBUFFER, GL_DEPTH_ATTACHMENT, GL_RENDERBUFFER, 0);
    let mut ds = 0;
    gles::glGenRenderbuffers(1, &mut ds);
    gles::glBindRenderbuffer(GL_RENDERBUFFER, ds);
    gles::glRenderbufferStorage(GL_RENDERBUFFER, DEPTH24_STENCIL8, 8, 4);
    gles::glFramebufferRenderbuffer(GL_FRAMEBUFFER, DEPTH_STENCIL_ATTACHMENT, GL_RENDERBUFFER, ds);
    assert_eq!(gles::glCheckFramebufferStatus(GL_FRAMEBUFFER), GL_FRAMEBUFFER_COMPLETE);
    // Deleting the depth-stencil renderbuffer detaches it → back to color-only complete.
    gles::glDeleteRenderbuffers(1, &ds);
    assert_eq!(gles::glCheckFramebufferStatus(GL_FRAMEBUFFER), GL_FRAMEBUFFER_COMPLETE);

    // Read/blit guard: an incomplete READ framebuffer blocks glBlitFramebuffer (source guard).
    let mut incomplete = 0;
    gles::glGenFramebuffers(1, &mut incomplete);
    gles::glBindFramebuffer(GL_READ_FRAMEBUFFER, incomplete); // no attachment → incomplete
    gles::glBindFramebuffer(GL_DRAW_FRAMEBUFFER, fbo); // complete
    while gles::glGetError() != GL_NO_ERROR {}
    gles::glBlitFramebuffer(0, 0, 8, 4, 0, 0, 8, 4, GL_COLOR_BUFFER_BIT, GL_NEAREST);
    assert_eq!(
        gles::glGetError(),
        GL_INVALID_FRAMEBUFFER_OPERATION,
        "blit from an incomplete read framebuffer must be rejected"
    );
    let _ = RGBA8;

    gles::glBindFramebuffer(GL_FRAMEBUFFER, 0);
    gles::glDeleteFramebuffers(1, &fbo);
    gles::glDeleteFramebuffers(1, &incomplete);
    gles::glDeleteTextures(1, &tex);
    gles::glDeleteRenderbuffers(1, &depth);
}

#[test]
fn generated_names_bind_lazily_and_deletion_detaches_every_binding() {
    let _serial = serial_guard();
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
    let _serial = serial_guard();
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
    let _serial = serial_guard();
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
    let _serial = serial_guard();
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
    let _serial = serial_guard();
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
    let _serial = serial_guard();
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
    let _serial = serial_guard();
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
    let _serial = serial_guard();
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
    let _serial = serial_guard();
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
    let _serial = serial_guard();
    let (sub_before, _) = gles::submission_serials();
    gles::glFlush(); // nonblocking submit
    let (sub_after_flush, _) = gles::submission_serials();
    assert!(sub_after_flush > sub_before, "glFlush must advance the submission serial (nonblocking submit)");

    let (sub_pre_finish, _) = gles::submission_serials();
    gles::glFinish(); // blocking: completion must catch up to everything submitted before it
    let (_, completed) = gles::submission_serials();
    assert!(completed >= sub_pre_finish, "glFinish must block until completion catches up to submission");
}
