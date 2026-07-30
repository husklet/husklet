//! Chromium GLES extensions required by its passthrough command decoder.

use super::*;

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glRequestExtensionANGLE(_name: *const c_char) {
    // The driver exposes no dynamically requestable extensions. Its static extension inventory is the
    // complete supported set, so every request is rejected without changing context capabilities.
    GlobalState::context(|group| group.gl.set_gl_error(GL_INVALID_OPERATION));
}

/// `glDisableExtensionANGLE(name)` — `GL_ANGLE_request_extension`'s other half. The advertised inventory
/// is static, so nothing can be disabled either; rejecting matches [`glRequestExtensionANGLE`]. The
/// extension is advertised, so ANGLE resolves this name and CALLS it without a null check — leaving it
/// unimplemented made an advertised command a jump to address zero.
// Not in the ABI census (registry manifest): an extension command resolves through
// `eglGetProcAddress`, exactly like `glMapBufferOES`. Exporting it would change the pinned surface.
pub extern "C" fn glDisableExtensionANGLE(_name: *const c_char) {
    GlobalState::context(|group| group.gl.set_gl_error(GL_INVALID_OPERATION));
}

/// `glBindGeneratesResourceCHROMIUM(enabled)` — `GL_CHROMIUM_bind_generates_resource`. This model always
/// materializes a name on first bind (see `hl_gl::model::{buffer,texture,framebuffer,renderbuffer}`), so
/// the behavior is permanently enabled: `GL_TRUE` is the state the driver is already in, and a request to
/// disable it is refused rather than silently ignored. Same null-call reason as above for existing at all.
// Resolved through `eglGetProcAddress`; not part of the pinned export surface.
pub extern "C" fn glBindGeneratesResourceCHROMIUM(enabled: u8) {
    if enabled == 0 {
        GlobalState::context(|group| group.gl.set_gl_error(GL_INVALID_OPERATION));
    }
}

#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glCopyTextureCHROMIUM(
    source: u32,
    source_level: i32,
    destination_target: u32,
    destination: u32,
    destination_level: i32,
    internal_format: i32,
    destination_type: u32,
    flip_y: u8,
    premultiply: u8,
    unmultiply: u8,
) {
    if source_level != 0
        || destination_level != 0
        || destination_target != GL_TEXTURE_2D
        || !matches!(internal_format as u32, GL_RGBA | GL_RGBA8)
        || destination_type != GL_UNSIGNED_BYTE
        || source == 0
        || destination == 0
        || (premultiply != 0 && unmultiply != 0)
    {
        GlobalState::context(|group| group.gl.set_gl_error(GL_INVALID_VALUE));
        return;
    }
    let copied = GlobalState::context(|group| {
        group.gl.textures.ensure(destination);
        let copied = group.gl.textures.copy(
            source,
            destination,
            flip_y != 0,
            premultiply != 0,
            unmultiply != 0,
        );
        if copied {
            group.mark_linear_dirty(destination);
        }
        copied
    });
    if !copied {
        GlobalState::context(|group| group.gl.set_gl_error(GL_INVALID_OPERATION));
    }
}

#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glCopySubTextureCHROMIUM(
    source: u32,
    source_level: i32,
    destination_target: u32,
    destination: u32,
    destination_level: i32,
    destination_x: i32,
    destination_y: i32,
    source_x: i32,
    source_y: i32,
    width: i32,
    height: i32,
    flip_y: u8,
    premultiply: u8,
    unmultiply: u8,
) {
    if source_level != 0
        || destination_level != 0
        || destination_target != GL_TEXTURE_2D
        || source == 0
        || destination == 0
        || (premultiply != 0 && unmultiply != 0)
    {
        GlobalState::context(|group| group.gl.set_gl_error(GL_INVALID_VALUE));
        return;
    }
    let copied = GlobalState::context(|group| {
        let copied = group.gl.textures.copy_sub(
            source,
            destination,
            source_x,
            source_y,
            destination_x,
            destination_y,
            width,
            height,
            flip_y != 0,
            premultiply != 0,
            unmultiply != 0,
        );
        if copied {
            group.mark_linear_dirty(destination);
        }
        copied
    });
    if !copied {
        GlobalState::context(|group| group.gl.set_gl_error(GL_INVALID_OPERATION));
    }
}
