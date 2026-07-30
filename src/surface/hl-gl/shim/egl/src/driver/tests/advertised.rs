//! An advertised extension's commands must RESOLVE. ANGLE and libepoxy resolve an advertised extension's
//! entry points through `eglGetProcAddress` and call them without a null check, so a resolvable-but-null
//! pointer is a jump to address zero inside the guest — a segfault with no diagnostic, at a point unrelated
//! to what the application was doing. Same failure shape as the `eglinfo` `*EXT` crash fixed earlier.

use super::*;

const EGL_MIPMAP_LEVEL: i32 = 0x3083;
const EGL_SWAP_BEHAVIOR: i32 = 0x3093;
const EGL_BUFFER_DESTROYED: i32 = 0x3095;
const EGL_BAD_NATIVE_PIXMAP_T: i32 = 0x300A;
const GL_BUFFER_MAP_POINTER: u32 = 0x88BD;

fn resolves(name: &str) -> bool {
    let c = std::ffi::CString::new(name).unwrap();
    !eglGetProcAddress(c.as_ptr()).is_null()
}

/// The GL error register is context-local, so a `gl*` assertion needs this thread bound to a context.
fn bind_current() {
    let attributes = [EGL_CONTEXT_CLIENT_VERSION, 3, EGL_NONE];
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

/// Each of these was named by an advertised extension while existing nowhere in the tree:
/// `GL_CHROMIUM_bind_generates_resource`, `GL_ANGLE_request_extension`, `GL_OES_mapbuffer`.
#[test]
fn every_command_of_an_advertised_extension_resolves() {
    for name in [
        "glBindGeneratesResourceCHROMIUM",
        "glDisableExtensionANGLE",
        "glGetBufferPointervOES",
        // the halves that already existed, so a regression in either direction is visible here
        "glRequestExtensionANGLE",
        "glMapBufferOES",
        "glUnmapBufferOES",
        "glGetBufferPointerv",
    ] {
        assert!(resolves(name), "{name} is advertised and must resolve");
    }
}

/// The OES spelling is the same query: an unmapped buffer reads back a null pointer, and a `pname` other
/// than `GL_BUFFER_MAP_POINTER` is `GL_INVALID_ENUM` (ES 3.0 6.1.14).
#[test]
fn the_oes_buffer_pointer_query_answers_like_the_core_one() {
    bind_current();

    let mut name = 0;
    glGenBuffers(1, &mut name);
    glBindBuffer(0x8892, name);
    glBufferData(0x8892, 8, core::ptr::null(), 0x88E4);
    assert_eq!(glGetError(), GL_NO_ERROR);

    let mut pointer = 1 as *mut c_void;
    glGetBufferPointervOES(0x8892, GL_BUFFER_MAP_POINTER, &mut pointer);
    assert!(pointer.is_null(), "an unmapped buffer has no map pointer");
    assert_eq!(glGetError(), GL_NO_ERROR);

    glGetBufferPointervOES(0x8892, 0xDEAD, &mut pointer);
    assert_eq!(
        glGetError(),
        GL_INVALID_ENUM,
        "an unknown pname is rejected"
    );
}

/// The inventory is static, so neither ANGLE spelling can change it — and refusing to disable
/// bind-generates-resource is honest: this model always materializes a name on first bind.
#[test]
fn the_static_extension_inventory_refuses_every_request_to_change_it() {
    bind_current();
    let name = std::ffi::CString::new("GL_OES_texture_npot").unwrap();

    glRequestExtensionANGLE(name.as_ptr());
    assert_eq!(glGetError(), GL_INVALID_OPERATION);
    glDisableExtensionANGLE(name.as_ptr());
    assert_eq!(glGetError(), GL_INVALID_OPERATION);
    glDisableExtensionANGLE(core::ptr::null());
    assert_eq!(
        glGetError(),
        GL_INVALID_OPERATION,
        "a null name is not a deref"
    );

    glBindGeneratesResourceCHROMIUM(EGL_TRUE as u8);
    assert_eq!(
        glGetError(),
        GL_NO_ERROR,
        "enabled is the state the model is already in"
    );
    glBindGeneratesResourceCHROMIUM(0);
    assert_eq!(
        glGetError(),
        GL_INVALID_OPERATION,
        "the model cannot stop materializing names on bind, so it says so"
    );
}

/// A surface operation must not report success for a handle the driver does not know, nor for an attribute
/// EGL does not define: the caller then proceeds believing state changed.
#[test]
fn surface_operations_refuse_an_unknown_handle_and_an_undefined_attribute() {
    let display = DISPLAY_TOKEN as *mut c_void;
    let surface = WindowSurface::create(core::ptr::null_mut());
    let stranger = 0xDEAD_BEEFusize as *mut c_void;
    let _ = eglGetError();

    for (attribute, value) in [
        (EGL_MIPMAP_LEVEL, 0),
        (EGL_SWAP_BEHAVIOR, EGL_BUFFER_DESTROYED),
    ] {
        assert_eq!(
            eglSurfaceAttrib(display, surface, attribute, value),
            EGL_TRUE,
            "a defined attribute on a live surface is still accepted"
        );
    }

    assert_eq!(
        eglSurfaceAttrib(display, stranger, EGL_MIPMAP_LEVEL, 0),
        EGL_FALSE
    );
    assert_eq!(eglGetError(), EGL_BAD_SURFACE);
    assert_eq!(eglSurfaceAttrib(display, surface, 0x1234, 0), EGL_FALSE);
    assert_eq!(eglGetError(), EGL_BAD_ATTRIBUTE);
    assert_eq!(
        eglSurfaceAttrib(display, surface, EGL_SWAP_BEHAVIOR, 0x1234),
        EGL_FALSE
    );
    assert_eq!(eglGetError(), EGL_BAD_ATTRIBUTE);
    assert_eq!(
        eglSurfaceAttrib(core::ptr::null_mut(), surface, EGL_MIPMAP_LEVEL, 0),
        EGL_FALSE
    );
    assert_eq!(eglGetError(), EGL_BAD_DISPLAY);

    // No render-to-texture pbuffer and no native pixmap target are modeled: refuse, do not claim success.
    assert_eq!(
        eglBindTexImage(display, surface, EGL_BACK_BUFFER),
        EGL_FALSE
    );
    assert_eq!(eglGetError(), EGL_BAD_SURFACE);
    assert_eq!(eglBindTexImage(display, surface, 0x1234), EGL_FALSE);
    assert_eq!(eglGetError(), EGL_BAD_PARAMETER);
    assert_eq!(
        eglReleaseTexImage(display, stranger, EGL_BACK_BUFFER),
        EGL_FALSE
    );
    assert_eq!(eglGetError(), EGL_BAD_SURFACE);
    assert_eq!(
        eglCopyBuffers(display, surface, core::ptr::null_mut()),
        EGL_FALSE
    );
    assert_eq!(eglGetError(), EGL_BAD_NATIVE_PIXMAP_T);
    assert_eq!(
        eglCopyBuffers(display, stranger, core::ptr::null_mut()),
        EGL_FALSE
    );
    assert_eq!(eglGetError(), EGL_BAD_SURFACE);
}
