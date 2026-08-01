//! Guest-hostile C-ABI inputs: every case here is a length, count, or extent an application chose, and
//! the assertion is that the driver REFUSES it — a returned GL error and an unchanged object — rather
//! than reading past the buffer the number indexes or aborting on an allocation it cannot make.

use super::*;

const GL_ARRAY_BUFFER: u32 = 0x8892;
const GL_STATIC_DRAW: u32 = 0x88E4;
const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_RGBA: u32 = 0x1908;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_COMPRESSED_RGBA8_ETC2_EAC: u32 = 0x9278;
const OVER_MAX: i32 = query::MAX_TEXTURE_SIZE + 1;

/// Bind this thread to a live context so the `gl*` entry points record against real state.
fn bind_current() {
    // A surface cannot exist on an uninitialized display; model the eglInitialize a real caller does.
    GlobalState::access(|state| state.inited = true);
    let attributes = [
        EGL_CONTEXT_CLIENT_VERSION,
        3,
        EGL_CONTEXT_MINOR_VERSION_KHR,
        1,
        EGL_NONE,
    ];
    let context = eglCreateContext(
        DISPLAY_TOKEN as *mut c_void,
        CONFIG_TOKEN as *mut c_void,
        core::ptr::null_mut(),
        attributes.as_ptr(),
    );
    let surface = WindowSurface::create(core::ptr::null_mut());
    assert_eq!(
        eglMakeCurrent(DISPLAY_TOKEN as *mut c_void, surface, surface, context),
        EGL_TRUE
    );
    while glGetError() != GL_NO_ERROR {}
}

fn bound_buffer_bytes(target: u32) -> Vec<u8> {
    GlobalState::context(|state| {
        let name = state.gl.buffer_for_target(target);
        state
            .gl
            .buffers
            .get(name)
            .map(|buffer| buffer.data.to_vec())
            .unwrap_or_default()
    })
}

/// `glBufferData(target, size, NULL, usage)` reserves `size` bytes. GLES3.0 2.9.2 makes a store the GL
/// cannot create GL_OUT_OF_MEMORY; an infallible `vec![0; size]` instead aborted the whole driver.
#[test]
fn a_buffer_reservation_the_driver_cannot_allocate_is_out_of_memory() {
    bind_current();
    let mut name = 0;
    glGenBuffers(1, &mut name);
    glBindBuffer(GL_ARRAY_BUFFER, name);
    glBufferData(GL_ARRAY_BUFFER, 8, core::ptr::null(), GL_STATIC_DRAW);
    assert_eq!(glGetError(), GL_NO_ERROR);

    glBufferData(
        GL_ARRAY_BUFFER,
        isize::MAX,
        core::ptr::null(),
        GL_STATIC_DRAW,
    );
    assert_eq!(
        glGetError(),
        GL_OUT_OF_MEMORY,
        "an unsatisfiable reservation is reported, not aborted"
    );
    assert_eq!(
        bound_buffer_bytes(GL_ARRAY_BUFFER).len(),
        8,
        "the refused reservation left the existing store untouched"
    );
}

/// `glBufferSubData`'s `size` indexes GUEST memory. GLES3.0 2.9.3 rejects a range past the bound buffer's
/// size, so the range must be bounded BEFORE the client pointer is read: `size = isize::MAX` against an
/// eight-byte buffer otherwise copied `isize::MAX` bytes out of an eight-byte allocation.
#[test]
fn a_sub_data_range_past_the_bound_buffer_is_refused_before_the_pointer_is_read() {
    bind_current();
    let mut name = 0;
    glGenBuffers(1, &mut name);
    glBindBuffer(GL_ARRAY_BUFFER, name);
    let payload = [0xA5u8; 8];
    glBufferData(GL_ARRAY_BUFFER, 8, payload.as_ptr().cast(), GL_STATIC_DRAW);
    assert_eq!(glGetError(), GL_NO_ERROR);

    for (offset, size) in [
        (0isize, isize::MAX),
        (0, 9),
        (4, 8),
        (-1, 4),
        (0, -4),
        (isize::MAX, 4),
    ] {
        glBufferSubData(GL_ARRAY_BUFFER, offset, size, payload.as_ptr().cast());
        assert_eq!(
            glGetError(),
            GL_INVALID_VALUE,
            "offset={offset} size={size} is out of range for an 8-byte buffer"
        );
    }
    assert_eq!(
        bound_buffer_bytes(GL_ARRAY_BUFFER),
        payload,
        "every refused range left the store byte-for-byte unchanged"
    );

    glBufferSubData(GL_ARRAY_BUFFER, 4, 4, [0x11u8; 4].as_ptr().cast());
    assert_eq!(glGetError(), GL_NO_ERROR, "an in-range write still lands");
    assert_eq!(
        bound_buffer_bytes(GL_ARRAY_BUFFER),
        [0xA5, 0xA5, 0xA5, 0xA5, 0x11, 0x11, 0x11, 0x11]
    );
}

/// `glTexImage2D`'s source span is `width * height * bpp` of GUEST memory. GLES3.0 3.8.3 rejects an
/// extent above GL_MAX_TEXTURE_SIZE, so the extent must be checked before the span is read — 65536x65536
/// with a small pixel buffer made the shim read ~17 GiB out of bounds.
#[test]
fn a_texture_extent_above_the_limit_is_refused_without_reading_the_pixels() {
    bind_current();
    let mut name = 0;
    glGenTextures(1, &mut name);
    glBindTexture(GL_TEXTURE_2D, name);
    let pixels = [0x3Cu8; 4 * 4 * 4];

    for (width, height) in [
        (OVER_MAX, OVER_MAX),
        (65536, 65536),
        (OVER_MAX, 4),
        (4, OVER_MAX),
        (i32::MAX, i32::MAX),
    ] {
        glTexImage2D(
            GL_TEXTURE_2D,
            0,
            GL_RGBA as i32,
            width,
            height,
            0,
            GL_RGBA,
            GL_UNSIGNED_BYTE,
            pixels.as_ptr().cast(),
        );
        assert_eq!(
            glGetError(),
            GL_INVALID_VALUE,
            "{width}x{height} is above GL_MAX_TEXTURE_SIZE"
        );
        assert!(
            GlobalState::context(|state| state.gl.textures.get(name).is_none_or(|t| !t.has_data())),
            "{width}x{height} materialized no pixel plane"
        );
    }

    glTexImage2D(
        GL_TEXTURE_2D,
        0,
        GL_RGBA as i32,
        4,
        4,
        0,
        GL_RGBA,
        GL_UNSIGNED_BYTE,
        pixels.as_ptr().cast(),
    );
    assert_eq!(glGetError(), GL_NO_ERROR, "a legal extent still uploads");
}

/// `glCompressedTexImage2D` reserves `width * height * 4` for the undecodable payload. That arithmetic is
/// the driver's own, so an over-limit extent (GLES3.0 3.8.6: GL_INVALID_VALUE) must be refused before it:
/// `i32::MAX` squared overflowed the allocator's capacity and `panic = "abort"` killed the driver.
#[test]
fn a_compressed_extent_above_the_limit_is_refused_before_the_reservation() {
    bind_current();
    let mut name = 0;
    glGenTextures(1, &mut name);
    glBindTexture(GL_TEXTURE_2D, name);
    let block = [0x11u8; 16];

    for (width, height) in [
        (i32::MAX, i32::MAX),
        (65536, 65536),
        (OVER_MAX, 4),
        (4, OVER_MAX),
    ] {
        glCompressedTexImage2D(
            GL_TEXTURE_2D,
            0,
            GL_COMPRESSED_RGBA8_ETC2_EAC,
            width,
            height,
            0,
            block.len() as i32,
            block.as_ptr().cast(),
        );
        assert_eq!(
            glGetError(),
            GL_INVALID_VALUE,
            "{width}x{height} is above GL_MAX_TEXTURE_SIZE"
        );
    }

    glCompressedTexImage2D(
        GL_TEXTURE_2D,
        0,
        GL_COMPRESSED_RGBA8_ETC2_EAC,
        4,
        4,
        0,
        block.len() as i32,
        block.as_ptr().cast(),
    );
    assert_eq!(glGetError(), GL_NO_ERROR, "a legal extent still allocates");
}

/// `eglTerminate` on a display this driver never issued must be refused, and must not tear down the one
/// it did issue.
///
/// Every other display-taking entry point — `eglInitialize`, `eglCreateContext`, `eglMakeCurrent`,
/// `eglDestroyContext`, `eglCreateWindowSurface`, `eglDestroySurface`, `eglCreatePbufferSurface`,
/// `eglQueryContext`, `eglQuerySurface` — checks the handle against `DISPLAY_TOKEN` and answers
/// `EGL_BAD_DISPLAY`. `eglTerminate` ignored its argument entirely, returned `EGL_TRUE`, and tore down
/// the real display's state on the way. Ten call sites agreeing and one disagreeing is what marks the
/// one as the oversight.
///
/// What it tore down is not bookkeeping: `State::terminate` clears `native_present`, and
/// `reserve_native_frame` returns `None` without it, so every later frame in the process degrades from a
/// zero-copy present to a readback. An application handing this driver another vendor's display — or
/// tearing down a second display at shutdown — silently lost zero-copy for the rest of its life.
#[test]
fn terminating_a_display_this_driver_never_issued_is_refused_and_tears_nothing_down() {
    GlobalState::access(|state| state.inited = true);

    let bogus = 0xdead_usize as *mut c_void;
    assert_eq!(
        eglTerminate(bogus),
        EGL_FALSE,
        "a display this driver never issued is not terminable"
    );
    assert_eq!(
        eglGetError(),
        EGL_BAD_DISPLAY,
        "the refusal must name the display"
    );
    assert!(
        GlobalState::access(|state| state.inited),
        "a refused eglTerminate must leave the real display initialized"
    );
}

