//! Advertised extensions versus resolvable entry points.
//!
//! An extension name in `eglQueryString(EGL_EXTENSIONS)` or `glGetString(GL_EXTENSIONS)` is a PROMISE that
//! its commands exist. Callers act on the promise without a null check — that is exactly how the real
//! `eglinfo` SIGSEGV'd against this driver once (see `tests/egl_surfaceless_config.rs`). A name whose
//! commands do not resolve is a silent trap: the string says yes, the call sites crash or take a
//! nonexistent path.
//!
//! GL extension commands are obtained through `eglGetProcAddress` (GLES spec §A.3 / EGL 1.5 §3.10): they
//! need not be exported from the `.so`, but the getter must return a non-null address for every one.

use super::*;

/// One advertised extension and the commands its specification defines. An extension with no commands has
/// an empty list; it is still listed so the table reads as the complete advertised inventory.
struct Extension {
    name: &'static str,
    commands: &'static [&'static str],
}

const fn ext(name: &'static str, commands: &'static [&'static str]) -> Extension {
    Extension { name, commands }
}

/// EGL extensions: the CLIENT string (queried with `EGL_NO_DISPLAY`) and the DISPLAY string.
const EGL_EXTENSION_COMMANDS: &[Extension] = &[
    ext("EGL_EXT_client_extensions", &[]),
    ext(
        "EGL_EXT_platform_base",
        &[
            "eglGetPlatformDisplayEXT",
            "eglCreatePlatformWindowSurfaceEXT",
            "eglCreatePlatformPixmapSurfaceEXT",
        ],
    ),
    ext("EGL_EXT_platform_wayland", &[]),
    ext("EGL_KHR_platform_wayland", &[]),
    // A platform name for eglGetPlatformDisplay; the entry points come from EGL_EXT_platform_base.
    ext("EGL_MESA_platform_surfaceless", &[]),
    // Likewise a platform name only: the device handles come from the EGL_EXT_device_* family.
    ext("EGL_EXT_platform_device", &[]),
    ext(
        "EGL_EXT_device_base",
        &[
            "eglQueryDeviceAttribEXT",
            "eglQueryDeviceStringEXT",
            "eglQueryDevicesEXT",
            "eglQueryDisplayAttribEXT",
        ],
    ),
    ext("EGL_EXT_device_enumeration", &["eglQueryDevicesEXT"]),
    ext(
        "EGL_EXT_device_query",
        &[
            "eglQueryDeviceAttribEXT",
            "eglQueryDeviceStringEXT",
            "eglQueryDisplayAttribEXT",
        ],
    ),
    // Display extensions.
    ext("EGL_KHR_create_context", &[]),
    ext("EGL_KHR_create_context_no_error", &[]),
    ext("EGL_KHR_surfaceless_context", &[]),
    ext("EGL_KHR_no_config_context", &[]),
    ext("EGL_EXT_create_context_robustness", &[]),
    ext(
        "EGL_KHR_fence_sync",
        &[
            "eglCreateSyncKHR",
            "eglDestroySyncKHR",
            "eglClientWaitSyncKHR",
            "eglGetSyncAttribKHR",
        ],
    ),
    ext(
        "EGL_KHR_image_base",
        &["eglCreateImageKHR", "eglDestroyImageKHR"],
    ),
    ext("EGL_EXT_image_dma_buf_import", &[]),
    ext(
        "EGL_EXT_image_dma_buf_import_modifiers",
        &["eglQueryDmaBufFormatsEXT", "eglQueryDmaBufModifiersEXT"],
    ),
];

/// GL extensions and the commands their specifications define.
const GL_EXTENSION_COMMANDS: &[Extension] = &[
    ext(
        "GL_KHR_debug",
        &[
            "glDebugMessageControlKHR",
            "glDebugMessageInsertKHR",
            "glDebugMessageCallbackKHR",
            "glGetDebugMessageLogKHR",
            "glPushDebugGroupKHR",
            "glPopDebugGroupKHR",
            "glObjectLabelKHR",
            "glGetObjectLabelKHR",
            "glObjectPtrLabelKHR",
            "glGetObjectPtrLabelKHR",
            "glGetPointervKHR",
        ],
    ),
    ext("GL_EXT_texture_format_BGRA8888", &[]),
    ext("GL_EXT_read_format_bgra", &[]),
    // The `GL_ANGLE_robust_client_memory` commands Chrome/ANGLE actually calls. The extension defines
    // more; this is the subset, so a pass here is not proof of the whole extension.
    ext(
        "GL_ANGLE_robust_client_memory",
        &[
            "glGetBooleanvRobustANGLE",
            "glGetIntegervRobustANGLE",
            "glGetFloatvRobustANGLE",
            "glGetProgramivRobustANGLE",
            "glGetShaderivRobustANGLE",
            "glGetBufferParameterivRobustANGLE",
            "glGetTexParameterivRobustANGLE",
            "glGetRenderbufferParameterivRobustANGLE",
            "glGetFramebufferAttachmentParameterivRobustANGLE",
            "glGetUniformfvRobustANGLE",
            "glGetUniformivRobustANGLE",
            "glGetVertexAttribfvRobustANGLE",
            "glReadPixelsRobustANGLE",
            "glTexImage2DRobustANGLE",
            "glTexSubImage2DRobustANGLE",
        ],
    ),
    ext(
        "GL_CHROMIUM_bind_generates_resource",
        &["glBindGeneratesResourceCHROMIUM"],
    ),
    ext(
        "GL_CHROMIUM_copy_texture",
        &["glCopyTextureCHROMIUM", "glCopySubTextureCHROMIUM"],
    ),
    ext("GL_ANGLE_client_arrays", &[]),
    ext("GL_ANGLE_webgl_compatibility", &[]),
    ext(
        "GL_ANGLE_request_extension",
        &["glRequestExtensionANGLE", "glDisableExtensionANGLE"],
    ),
    ext(
        "GL_OES_EGL_image",
        &[
            "glEGLImageTargetTexture2DOES",
            "glEGLImageTargetRenderbufferStorageOES",
        ],
    ),
    ext("GL_OES_EGL_sync", &[]),
    ext("GL_OES_rgb8_rgba8", &[]),
    ext("GL_OES_depth24", &[]),
    ext(
        "GL_OES_mapbuffer",
        &[
            "glMapBufferOES",
            "glUnmapBufferOES",
            "glGetBufferPointervOES",
        ],
    ),
];

/// The names in a space-separated EGL/GL extension string.
fn names(string: &str) -> Vec<&str> {
    string.split_whitespace().collect()
}

/// Every command of every advertised extension must resolve, and every advertised name must appear in the
/// table above (a name the table does not know is a claim nobody has checked).
fn assert_resolvable(
    advertised: &[&str],
    table: &[Extension],
    resolve: &dyn Fn(&str) -> *mut c_void,
    source: &str,
) {
    let mut unmapped = Vec::new();
    let mut unresolved = Vec::new();
    for name in advertised {
        match table.iter().find(|entry| entry.name == *name) {
            None => unmapped.push(*name),
            Some(entry) => {
                for command in entry.commands {
                    if resolve(command).is_null() {
                        unresolved.push(format!("{name}:{command}"));
                    }
                }
            }
        }
    }
    assert!(
        unmapped.is_empty(),
        "{source} advertises extensions this battery has no command table for: {unmapped:?} — \
         extend the table rather than trusting the string"
    );
    assert!(
        unresolved.is_empty(),
        "{source} advertises extensions whose commands do not resolve through eglGetProcAddress: \
         {unresolved:?} — a caller resolves and calls these WITHOUT a null check"
    );
}

#[test]
fn every_advertised_egl_extension_has_resolvable_entry_points() {
    let _serial = serial();
    let egl = Egl::bring_up();
    let resolve = |name: &str| {
        let c = std::ffi::CString::new(name).unwrap();
        (egl.get_proc_address)(c.as_ptr())
    };

    let client = cstr((egl.query_string)(core::ptr::null_mut(), EGL_EXTENSIONS));
    assert!(
        !client.is_empty(),
        "eglQueryString(EGL_NO_DISPLAY, EGL_EXTENSIONS) must list the client extensions"
    );
    assert_resolvable(
        &names(&client),
        EGL_EXTENSION_COMMANDS,
        &resolve,
        "the EGL client extension string",
    );

    let display = cstr((egl.query_string)(egl.display, EGL_EXTENSIONS));
    assert!(
        !display.is_empty(),
        "the display extension string must not be empty"
    );
    assert_resolvable(
        &names(&display),
        EGL_EXTENSION_COMMANDS,
        &resolve,
        "the EGL display extension string",
    );
}

#[test]
fn every_advertised_gl_extension_has_resolvable_entry_points() {
    let _serial = serial();
    let egl = Egl::bring_up();
    let shim = Shim::get();
    let context = (egl.create_context)(
        egl.display,
        core::ptr::null_mut(),
        core::ptr::null_mut(),
        core::ptr::null(),
    );
    assert!(
        !context.is_null(),
        "a default-attribute context is required to query GL strings"
    );
    assert_eq!(
        (egl.make_current)(
            egl.display,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            context
        ),
        EGL_TRUE
    );

    let get_string = f!(
        shim.gles,
        "glGetString",
        extern "C" fn(u32) -> *const c_char
    );
    let advertised = cstr(get_string(GL_EXTENSIONS));
    assert!(
        !advertised.is_empty(),
        "glGetString(GL_EXTENSIONS) must not be empty"
    );
    let resolve = |name: &str| {
        let c = std::ffi::CString::new(name).unwrap();
        (egl.get_proc_address)(c.as_ptr())
    };
    assert_resolvable(
        &names(&advertised),
        GL_EXTENSION_COMMANDS,
        &resolve,
        "glGetString(GL_EXTENSIONS)",
    );

    (egl.make_current)(
        egl.display,
        core::ptr::null_mut(),
        core::ptr::null_mut(),
        core::ptr::null_mut(),
    );
    assert_eq!((egl.destroy_context)(egl.display, context), EGL_TRUE);
}

#[test]
fn indexed_gl_extension_enumeration_agrees_with_the_string_and_the_count() {
    let _serial = serial();
    let egl = Egl::bring_up();
    let shim = Shim::get();
    let context = (egl.create_context)(
        egl.display,
        core::ptr::null_mut(),
        core::ptr::null_mut(),
        core::ptr::null(),
    );
    assert!(!context.is_null());
    assert_eq!(
        (egl.make_current)(
            egl.display,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            context
        ),
        EGL_TRUE
    );

    let get_string = f!(
        shim.gles,
        "glGetString",
        extern "C" fn(u32) -> *const c_char
    );
    let get_stringi = f!(
        shim.gles,
        "glGetStringi",
        extern "C" fn(u32, u32) -> *const c_char
    );
    let get_integerv = f!(shim.gles, "glGetIntegerv", extern "C" fn(u32, *mut i32));
    let get_error = f!(shim.gles, "glGetError", extern "C" fn() -> u32);

    let mut count = -1;
    get_integerv(GL_NUM_EXTENSIONS, &mut count);
    let from_string: Vec<String> = cstr(get_string(GL_EXTENSIONS))
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        count as usize,
        from_string.len(),
        "GL_NUM_EXTENSIONS must equal the number of names in GL_EXTENSIONS"
    );
    let indexed: Vec<String> = (0..count as u32)
        .map(|index| cstr(get_stringi(GL_EXTENSIONS, index)))
        .collect();
    assert_eq!(
        indexed, from_string,
        "glGetStringi must enumerate exactly the names in the GL_EXTENSIONS string, in order"
    );
    assert_eq!(
        get_error(),
        GL_NO_ERROR,
        "the in-range enumeration must raise no error"
    );

    // ES 3.0 §6.1.6: an index at or beyond GL_NUM_EXTENSIONS is GL_INVALID_VALUE and returns NULL.
    assert!(get_stringi(GL_EXTENSIONS, count as u32).is_null());
    assert_eq!(
        get_error(),
        0x0501,
        "out-of-range glGetStringi index must be GL_INVALID_VALUE"
    );
    // An unrecognized name is GL_INVALID_ENUM.
    assert!(get_stringi(GL_VERSION, 0).is_null());
    assert_eq!(
        get_error(),
        0x0500,
        "glGetStringi(GL_VERSION) must be GL_INVALID_ENUM"
    );

    (egl.make_current)(
        egl.display,
        core::ptr::null_mut(),
        core::ptr::null_mut(),
        core::ptr::null_mut(),
    );
    assert_eq!((egl.destroy_context)(egl.display, context), EGL_TRUE);
}
