use super::*;

mod extra;
pub use extra::*;
use std::sync::Arc;

/// Bind an imported `EGLImage` to the texture currently bound at `target`.
///
/// The supported dma-buf contract is deliberately narrow: linear ARGB/XRGB8888 images imported through
/// `eglCreateImageKHR`. The image owns its fd independently; this call snapshots its pixels into the GL
/// texture model so ordinary sampling follows the same IR upload path as `glTexImage2D`.
pub extern "C" fn glEGLImageTargetTexture2DOES(target: u32, image: *mut c_void) {
    let debug = std::env::var_os("HL_SHIM_DEBUG").is_some();
    if debug {
        eprintln!(
            "[hl-gl-shim] image-bind enter kind=texture target={target:#x} image={:#x}",
            image as usize
        );
    }
    if target != GL_TEXTURE_2D {
        if debug {
            eprintln!(
                "[hl-gl-shim] image-bind reject kind=texture target={target:#x} invalid_target"
            );
        }
        GlobalState::context(|state| state.gl.set_gl_error(GL_INVALID_ENUM));
        return;
    }
    if image.is_null() {
        if debug {
            eprintln!("[hl-gl-shim] image-bind reject kind=texture target={target:#x} null_image");
        }
        GlobalState::context(|state| state.gl.set_gl_error(GL_INVALID_VALUE));
        return;
    }
    let imported = GlobalState::access(|state| state.images.get(image));
    GlobalState::context(|group| {
        let image_token = image as usize;
        let texture = group.gl.bound_texture();
        if texture == 0 {
            if debug {
                eprintln!(
                    "[hl-gl-shim] image-bind reject kind=texture target={target:#x} no_bound_texture"
                );
            }
            group.gl.set_gl_error(GL_INVALID_OPERATION);
            return;
        }
        let Some(imported) = imported else {
            if debug {
                eprintln!(
                    "[hl-gl-shim] image-bind reject kind=texture target={target:#x} stale_image"
                );
            }
            group.gl.set_gl_error(GL_INVALID_VALUE);
            return;
        };
        let (Ok(width), Ok(height)) = (
            i32::try_from(imported.width),
            i32::try_from(imported.height),
        ) else {
            group.gl.set_gl_error(GL_INVALID_OPERATION);
            return;
        };
        if width > query::MAX_TEXTURE_SIZE || height > query::MAX_TEXTURE_SIZE {
            group.gl.set_gl_error(GL_INVALID_OPERATION);
            return;
        }
        if let Some(token) = imported.external_token() {
            if group
                .gl
                .validate_external_target(token, width, height)
                .is_err()
            {
                group.gl.set_gl_error(GL_INVALID_OPERATION);
                return;
            }
            group.gl.retire_texture(texture);
            group.gl.textures.external_image_2d(
                texture,
                width,
                height,
                hl_gpu::protocol::model::enums::TextureFormat::Bgra8Unorm,
            );
            let generation = group.gl.textures.get(texture).unwrap().gen;
            group.gl.bind_external_target(texture, generation, token);
            group.images.insert(
                texture,
                crate::state::ImportedImage {
                    generation,
                    image: imported,
                    shared: None,
                },
            );
        } else {
            let Ok(pixels) = imported.native_bgra() else {
                group.gl.set_gl_error(GL_INVALID_OPERATION);
                return;
            };
            let shared = match group.linear_storage(&imported, Arc::new(pixels.clone())) {
                Ok(shared) => shared,
                Err(_) => {
                    group.gl.set_gl_error(GL_INVALID_OPERATION);
                    return;
                }
            };
            group.gl.retire_texture(texture);
            record::tex_image_2d_format(
                &mut group.gl,
                width,
                height,
                &pixels,
                hl_gpu::protocol::model::enums::TextureFormat::Bgra8Unorm,
            );
            group.gl.textures.bind_shared(texture, shared.clone());
            let generation = group.gl.textures.get(texture).unwrap().gen;
            group.images.insert(
                texture,
                crate::state::ImportedImage {
                    generation,
                    image: imported,
                    shared: Some(shared),
                },
            );
        }
        if std::env::var_os("HL_SHIM_DEBUG").is_some() {
            eprintln!(
                "[hl-gl-shim] image-bind kind=texture image={image_token:#x} texture={texture} \
                 renderbuffer=0 size={width}x{height}"
            );
        }
    });
}

/// Bind an imported `EGLImage` as the storage of the currently bound renderbuffer.
///
/// Renderbuffers share the model's texture-backed color path. Associating that backing texture with the
/// image also lets `glFlush` copy completed offscreen rendering back into the imported dma-buf.
pub extern "C" fn glEGLImageTargetRenderbufferStorageOES(target: u32, image: *mut c_void) {
    let debug = std::env::var_os("HL_SHIM_DEBUG").is_some();
    if debug {
        eprintln!(
            "[hl-gl-shim] image-bind enter kind=renderbuffer target={target:#x} image={:#x}",
            image as usize
        );
    }
    if target != GL_RENDERBUFFER {
        if debug {
            eprintln!(
                "[hl-gl-shim] image-bind reject kind=renderbuffer target={target:#x} invalid_target"
            );
        }
        GlobalState::context(|state| state.gl.set_gl_error(GL_INVALID_ENUM));
        return;
    }
    if image.is_null() {
        if debug {
            eprintln!(
                "[hl-gl-shim] image-bind reject kind=renderbuffer target={target:#x} null_image"
            );
        }
        GlobalState::context(|state| state.gl.set_gl_error(GL_INVALID_VALUE));
        return;
    }
    let imported = GlobalState::access(|state| state.images.get(image));
    GlobalState::context(|group| {
        let renderbuffer = group.gl.bound_renderbuffer();
        if renderbuffer == 0 {
            if debug {
                eprintln!(
                    "[hl-gl-shim] image-bind reject kind=renderbuffer target={target:#x} \
                     no_bound_renderbuffer"
                );
            }
            group.gl.set_gl_error(GL_INVALID_OPERATION);
            return;
        }
        let Some(imported) = imported else {
            if debug {
                eprintln!(
                    "[hl-gl-shim] image-bind reject kind=renderbuffer target={target:#x} stale_image"
                );
            }
            group.gl.set_gl_error(GL_INVALID_VALUE);
            return;
        };
        let (Ok(width), Ok(height)) = (
            i32::try_from(imported.width),
            i32::try_from(imported.height),
        ) else {
            group.gl.set_gl_error(GL_INVALID_OPERATION);
            return;
        };
        if width > query::MAX_TEXTURE_SIZE || height > query::MAX_TEXTURE_SIZE {
            group.gl.set_gl_error(GL_INVALID_OPERATION);
            return;
        }
        let shared = if let Some(token) = imported.external_token() {
            if group
                .gl
                .validate_external_target(token, width, height)
                .is_err()
            {
                group.gl.set_gl_error(GL_INVALID_OPERATION);
                return;
            }
            record::renderbuffer_storage(&mut group.gl, target, GL_RGBA8, width, height);
            let texture = group.gl.renderbuffers.backing_tex(renderbuffer);
            group.gl.retire_texture(texture);
            group.gl.textures.external_image_2d(
                texture,
                width,
                height,
                hl_gpu::protocol::model::enums::TextureFormat::Bgra8Unorm,
            );
            let generation = group.gl.textures.get(texture).unwrap().gen;
            group.gl.bind_external_target(texture, generation, token);
            None
        } else {
            let Ok(pixels) = imported.native_bgra() else {
                group.gl.set_gl_error(GL_INVALID_OPERATION);
                return;
            };
            let shared = match group.linear_storage(&imported, Arc::new(pixels.clone())) {
                Ok(shared) => shared,
                Err(_) => {
                    group.gl.set_gl_error(GL_INVALID_OPERATION);
                    return;
                }
            };
            record::renderbuffer_storage(&mut group.gl, target, GL_RGBA8, width, height);
            let texture = group.gl.renderbuffers.backing_tex(renderbuffer);
            group.gl.retire_texture(texture);
            group.gl.textures.image_2d(
                texture,
                width,
                height,
                &pixels,
                hl_gpu::protocol::model::enums::TextureFormat::Bgra8Unorm,
            );
            group.gl.textures.bind_shared(texture, shared.clone());
            Some(shared)
        };
        let texture = group.gl.renderbuffers.backing_tex(renderbuffer);
        let generation = group.gl.textures.get(texture).unwrap().gen;
        let image_token = image as usize;
        group.images.insert(
            texture,
            crate::state::ImportedImage {
                generation,
                image: imported,
                shared,
            },
        );
        if std::env::var_os("HL_SHIM_DEBUG").is_some() {
            eprintln!(
                "[hl-gl-shim] image-bind kind=renderbuffer image={image_token:#x} texture={texture} \
                 renderbuffer={renderbuffer} size={width}x{height}"
            );
        }
    });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexStorage2D(
    target: u32,
    levels: i32,
    internalformat: u32,
    width: i32,
    height: i32,
) {
    GlobalState::context(|group| {
        group.redefine_texture(|ctx| {
            record::tex_storage_2d(ctx, target, levels, internalformat, width, height)
        })
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexStorage3D(
    target: u32,
    levels: i32,
    _internalformat: u32,
    width: i32,
    height: i32,
    depth: i32,
) {
    GlobalState::context(|group| {
        record::tex_storage_3d(&mut group.gl, target, levels, width, height, depth)
    });
}

#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glTexImage3D(
    target: u32,
    level: i32,
    _internalformat: i32,
    width: i32,
    height: i32,
    depth: i32,
    _border: i32,
    format: u32,
    type_: u32,
    pixels: *const c_void,
) {
    GlobalState::context(|group| {
        let rgba = unsafe { to_rgba8(&group.gl, format, type_, width, height, pixels) };
        record::tex_image_3d(&mut group.gl, target, level, width, height, depth, &rgba)
    });
}

#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glTexSubImage2D(
    target: u32,
    level: i32,
    xoffset: i32,
    yoffset: i32,
    width: i32,
    height: i32,
    format: u32,
    type_: u32,
    pixels: *const c_void,
) {
    GlobalState::context(|group| {
        // ES 3.0 §3.8.3: `glTexSubImage2D` has no "allocate without data" form — a null `pixels` is only
        // an OFFSET into a bound `GL_PIXEL_UNPACK_BUFFER`. With no such buffer it is
        // `GL_INVALID_OPERATION`, not an invitation to read address zero.
        if pixels.is_null() && group.gl.buffer_for_target(GL_PIXEL_UNPACK_BUFFER) == 0 {
            group.gl.set_gl_error(GL_INVALID_OPERATION);
            return;
        }
        let rgba = unsafe { to_rgba8(&group.gl, format, type_, width, height, pixels) };
        let texture = group.gl.bound_texture();
        let generation = group.gl.textures.get(texture).map(|texture| texture.gen);
        record::tex_sub_image_2d(
            &mut group.gl,
            target,
            level,
            xoffset,
            yoffset,
            width,
            height,
            &rgba,
        );
        if generation != group.gl.textures.get(texture).map(|texture| texture.gen) {
            group.mark_linear_dirty(texture);
        }
    });
}

#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glTexSubImage3D(
    target: u32,
    level: i32,
    xoffset: i32,
    yoffset: i32,
    zoffset: i32,
    width: i32,
    height: i32,
    depth: i32,
    format: u32,
    type_: u32,
    pixels: *const c_void,
) {
    GlobalState::context(|group| {
        let rgba = unsafe { to_rgba8(&group.gl, format, type_, width, height, pixels) };
        record::tex_sub_image_3d(
            &mut group.gl,
            target,
            level,
            xoffset,
            yoffset,
            zoffset,
            width,
            height,
            depth,
            &rgba,
        )
    });
}

#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glCopyTexSubImage2D(
    target: u32,
    level: i32,
    xoffset: i32,
    yoffset: i32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) {
    GlobalState::context(|group| {
        let texture = group.gl.bound_texture();
        let generation = group.gl.textures.get(texture).map(|texture| texture.gen);
        record::copy_tex_sub_image_2d(
            &mut group.gl,
            target,
            level,
            xoffset,
            yoffset,
            x,
            y,
            width,
            height,
        );
        if generation != group.gl.textures.get(texture).map(|texture| texture.gen) {
            group.mark_linear_dirty(texture);
        }
    });
}

/// `glCopyTexSubImage3D` — the deferred model has no materialized source color plane per layer at record
/// time (see [`record::copy_tex_sub_image_2d`]); the layer copy is a documented no-op. Params validated
/// only insofar as a bad `target` is left to the bound-texture path — an honest no-op body.
#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glCopyTexSubImage3D(
    _target: u32,
    _level: i32,
    _xoffset: i32,
    _yoffset: i32,
    _zoffset: i32,
    _x: i32,
    _y: i32,
    _width: i32,
    _height: i32,
) {
}

/// `glCopyTexImage2D` — allocate the bound texture and copy from the read framebuffer. This deferred
/// model has no materialized default-framebuffer source plane at record time (only a color-attachment
/// texture carries pixels), so the allocation of the destination extent is honored and the pixel copy is
/// the documented no-op (mirrors `glCopyTexSubImage2D`/`glBlitFramebuffer`).
#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glCopyTexImage2D(
    target: u32,
    level: i32,
    internalformat: u32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    border: i32,
) {
    let _ = (x, y);
    if target != GL_TEXTURE_2D || level != 0 || border != 0 {
        GlobalState::context(|group| group.gl.set_gl_error(GL_INVALID_VALUE));
        return;
    }
    // Allocate the destination extent so a later sample/subimage has storage (RGBA8 neutral plane).
    let ifmt = if matches!(internalformat, GL_RGB | GL_RGBA) {
        internalformat
    } else {
        GL_RGBA
    };
    let _ = ifmt;
    GlobalState::context(|group| {
        group.redefine_texture(|ctx| {
            let name = ctx.bound_texture();
            if name != 0 && width >= 0 && height >= 0 {
                ctx.textures.alloc_rgba(name, width, height);
            } else {
                ctx.set_gl_error(GL_INVALID_VALUE);
            }
        });
    });
}

// ==================================================================================================
// ES3 compressed-texture uploads — no compressed codec is modeled, so these validate + no-op honestly
// (the RGBA8 render path samples uncompressed textures only; a compressed upload materializes no pixels).
// ==================================================================================================
