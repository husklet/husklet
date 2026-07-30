use super::*;
use std::io::Write;
use std::os::fd::AsRawFd;

const EGL_LINUX_DMA_BUF_EXT: u32 = 0x3270;
const EGL_NONE: i32 = 0x3038;
const EGL_LINUX_DRM_FOURCC_EXT: i32 = 0x3271;
const EGL_DMA_BUF_PLANE0_FD_EXT: i32 = 0x3272;
const EGL_DMA_BUF_PLANE0_OFFSET_EXT: i32 = 0x3273;
const EGL_DMA_BUF_PLANE0_PITCH_EXT: i32 = 0x3274;
const DRM_FORMAT_ARGB8888: i32 = i32::from_le_bytes(*b"AR24");

#[test]
fn oes_egl_image_resolves_and_binds_texture_and_renderbuffer_storage() {
    let _guard = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let Some(shim) = load() else { return };

    let egl_get_proc = f!(
        shim.egl,
        "eglGetProcAddress",
        extern "C" fn(*const c_char) -> *mut c_void
    );
    let resolve = |name: &str| {
        let name = std::ffi::CString::new(name).unwrap();
        egl_get_proc(name.as_ptr())
    };
    let create_image_pointer = resolve("eglCreateImageKHR");
    let destroy_image_pointer = resolve("eglDestroyImageKHR");
    let bind_texture_pointer = resolve("glEGLImageTargetTexture2DOES");
    let bind_renderbuffer_pointer = resolve("glEGLImageTargetRenderbufferStorageOES");
    assert!(!create_image_pointer.is_null());
    assert!(!destroy_image_pointer.is_null());
    assert!(!bind_texture_pointer.is_null());
    assert!(!bind_renderbuffer_pointer.is_null());
    let create_image: extern "C" fn(
        *mut c_void,
        *mut c_void,
        u32,
        *mut c_void,
        *const i32,
    ) -> *mut c_void = unsafe { core::mem::transmute(create_image_pointer) };
    let destroy_image: extern "C" fn(*mut c_void, *mut c_void) -> u32 =
        unsafe { core::mem::transmute(destroy_image_pointer) };
    let bind_texture: extern "C" fn(u32, *mut c_void) =
        unsafe { core::mem::transmute(bind_texture_pointer) };
    let bind_renderbuffer: extern "C" fn(u32, *mut c_void) =
        unsafe { core::mem::transmute(bind_renderbuffer_pointer) };

    let path = std::env::temp_dir().join(format!("hl-oes-egl-image-{}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    file.write_all(&[3, 2, 1, 0xff, 6, 5, 4, 0xff]).unwrap();
    let attributes = [
        EGL_WIDTH,
        2,
        EGL_HEIGHT,
        1,
        EGL_LINUX_DRM_FOURCC_EXT,
        DRM_FORMAT_ARGB8888,
        EGL_DMA_BUF_PLANE0_FD_EXT,
        file.as_raw_fd(),
        EGL_DMA_BUF_PLANE0_OFFSET_EXT,
        0,
        EGL_DMA_BUF_PLANE0_PITCH_EXT,
        8,
        EGL_NONE,
    ];
    let image = create_image(
        1usize as *mut c_void,
        core::ptr::null_mut(),
        EGL_LINUX_DMA_BUF_EXT,
        core::ptr::null_mut(),
        attributes.as_ptr(),
    );
    assert!(!image.is_null());

    let gl_gen_textures = f!(shim.gles, "glGenTextures", extern "C" fn(i32, *mut u32));
    let gl_bind_texture = f!(shim.gles, "glBindTexture", extern "C" fn(u32, u32));
    let gl_get_tex_level = f!(
        shim.gles,
        "glGetTexLevelParameteriv",
        extern "C" fn(u32, i32, u32, *mut i32)
    );
    let mut texture = 0;
    gl_gen_textures(1, &mut texture);
    gl_bind_texture(GL_TEXTURE_2D, texture);
    bind_texture(GL_TEXTURE_2D, image);

    let gl_gen_renderbuffers = f!(
        shim.gles,
        "glGenRenderbuffers",
        extern "C" fn(i32, *mut u32)
    );
    let gl_bind_renderbuffer = f!(shim.gles, "glBindRenderbuffer", extern "C" fn(u32, u32));
    let gl_get_renderbuffer = f!(
        shim.gles,
        "glGetRenderbufferParameteriv",
        extern "C" fn(u32, u32, *mut i32)
    );
    let mut renderbuffer = 0;
    gl_gen_renderbuffers(1, &mut renderbuffer);
    gl_bind_renderbuffer(GL_RENDERBUFFER, renderbuffer);
    bind_renderbuffer(GL_RENDERBUFFER, image);

    // EGL_KHR_image_base: destroying the opaque image handle does not destroy texture/renderbuffer
    // siblings already created from it.
    assert_eq!(destroy_image(1usize as *mut c_void, image), EGL_TRUE);
    let mut width = 0;
    gl_get_tex_level(GL_TEXTURE_2D, 0, GL_TEXTURE_WIDTH, &mut width);
    assert_eq!(width, 2);
    gl_get_renderbuffer(GL_RENDERBUFFER, GL_RENDERBUFFER_WIDTH, &mut width);
    assert_eq!(width, 2);

    drop(file);
    std::fs::remove_file(path).unwrap();
}
