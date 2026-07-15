//! The `gl*` recording ops — the deferred-lowering front half of the driver.
//!
//! Every function here mutates [`GlContext`] and submits NOTHING: a `gl*` call records into per-context
//! state (a created object, a binding, or an appended [`DrawCall`]) exactly as `gl_shim.c` does, and the
//! IR is emitted later, at swap, by [`crate::service::frame`]. Ported from `hl-shim-gl/src/gles.rs`
//! (the state-recording bodies) — the semantics (bindings, the draw-time state snapshot) are preserved.

use crate::model::context::GlContext;
use crate::model::glconst::*;
use crate::model::program::DrawCall;
use hl_gpu::protocol::model::enums::TextureFormat;

// ---- buffers -------------------------------------------------------------------------------------

/// `glGenBuffers` (one name).
pub fn gen_buffer(ctx: &mut GlContext) -> u32 {
    ctx.buffers.gen()
}

/// `glBindBuffer(target, name)`. `GL_ARRAY_BUFFER`/`GL_ELEMENT_ARRAY_BUFFER` use their dedicated bindings;
/// every other ES3 target (UBO/SSBO/PBO/dispatch-indirect/…) records into the general binding map so
/// `glMapBufferRange`/`glDispatchComputeIndirect` can resolve it.
pub fn bind_buffer(ctx: &mut GlContext, target: u32, name: u32) {
    match target {
        GL_ARRAY_BUFFER => ctx.array_buffer = name,
        GL_ELEMENT_ARRAY_BUFFER => ctx.element_buffer = name,
        t => {
            if name == 0 {
                ctx.general_buffers.remove(&t);
            } else {
                ctx.general_buffers.insert(t, name);
            }
        }
    }
}

/// `glBufferData(target, data, usage)` — fills the buffer currently bound to `target`.
pub fn buffer_data(ctx: &mut GlContext, target: u32, data: &[u8], usage: u32) {
    let name = bound_buffer(ctx, target);
    if std::env::var("HL_UBO_DUMP").is_ok() && (name >= 1 && name <= 9) {
        eprintln!("[UBO_DUMP] glBufferData target={target:#x} name={name} len={} usage={usage:#x}", data.len());
    }
    if name != 0 {
        ctx.buffers.set_data(name, target, data, usage);
    }
}

/// `glBufferSubData(target, offset, data)`. A range that overflows or reaches beyond the bound buffer's
/// current size is `GL_INVALID_VALUE` (real GL) — this also bounds the write so a hostile `offset` can
/// never grow the buffer's `Vec` to an unbounded (or overflowing) size and panic/OOM.
pub fn buffer_sub_data(ctx: &mut GlContext, target: u32, offset: usize, data: &[u8]) {
    let name = bound_buffer(ctx, target);
    if std::env::var("HL_UBO_DUMP").is_ok() && (name >= 1 && name <= 9) {
        eprintln!("[UBO_DUMP] glBufferSubData target={target:#x} name={name} off={offset} len={}", data.len());
    }
    if name != 0 {
        let size = ctx.buffers.get(name).map(|b| b.data.len()).unwrap_or(0);
        match offset.checked_add(data.len()) {
            Some(end) if end <= size => ctx.buffers.set_sub_data(name, offset, data),
            _ => ctx.set_gl_error(GL_INVALID_VALUE),
        }
    }
}

/// `glDeleteBuffers` (one name).
pub fn delete_buffer(ctx: &mut GlContext, name: u32) -> bool {
    if ctx.array_buffer == name {
        ctx.array_buffer = 0;
    }
    if ctx.element_buffer == name {
        ctx.element_buffer = 0;
    }
    ctx.buffers.delete(name)
}

fn bound_buffer(ctx: &GlContext, target: u32) -> u32 {
    ctx.buffer_for_target(target)
}

/// `glCopyBufferSubData(readTarget, writeTarget, readOffset, writeOffset, size)` — copy `size` bytes
/// between the buffers bound to the two targets (`gl_shim.c` parity, a CPU-side byte copy). A negative
/// offset/size → `GL_INVALID_VALUE`; an out-of-range range is an honest no-op (nothing is copied).
pub fn copy_buffer_sub_data(ctx: &mut GlContext, read_target: u32, write_target: u32, read_off: isize, write_off: isize, size: isize) {
    if read_off < 0 || write_off < 0 || size < 0 {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    let rb = ctx.buffer_for_target(read_target);
    let wb = ctx.buffer_for_target(write_target);
    if std::env::var("HL_UBO_DUMP").is_ok() {
        eprintln!("[UBO_DUMP] glCopyBufferSubData rt={read_target:#x} wt={write_target:#x} rb={rb} wb={wb} ro={read_off} wo={write_off} size={size}");
    }
    let (ro, wo, n) = (read_off as usize, write_off as usize, size as usize);
    if rb == 0 || wb == 0 {
        return;
    }
    let src = match ctx.buffers.range_bytes(rb, ro, n) {
        Some(s) if s.len() == n => s,
        _ => return, // out-of-range read source: no-op
    };
    // Grow the write buffer to cover the destination range, then overwrite it.
    if ctx.buffers.get(wb).map(|b| b.data.len() < wo + n).unwrap_or(true) {
        return; // destination out of range: no-op (matches gl_shim.c's bounds guard)
    }
    ctx.buffers.set_sub_data(wb, wo, &src);
}

// ---- indexed buffer bindings (glBindBufferBase / glBindBufferRange) -------------------------------

use crate::model::context::IndexedBinding;

/// The per-index binding cap for an indexed-buffer `target`, or `None` if `target` is not a valid indexed
/// target (`glBindBufferBase`/`glBindBufferRange` raise `GL_INVALID_ENUM`).
fn indexed_target_cap(target: u32) -> Option<u32> {
    match target {
        GL_UNIFORM_BUFFER => Some(MAX_UNIFORM_BUFFER_BINDINGS),
        GL_SHADER_STORAGE_BUFFER => Some(MAX_SHADER_STORAGE_BUFFER_BINDINGS),
        GL_ATOMIC_COUNTER_BUFFER => Some(MAX_ATOMIC_COUNTER_BUFFER_BINDINGS),
        GL_TRANSFORM_FEEDBACK_BUFFER => Some(MAX_TRANSFORM_FEEDBACK_BUFFERS),
        _ => None,
    }
}

/// `glBindBufferBase(target, index, buffer)` — bind the whole `buffer` to indexed slot `index` of `target`
/// (and the generic target binding). A UBO/SSBO binding feeds a `glDispatchCompute` bind group.
pub fn bind_buffer_base(ctx: &mut GlContext, target: u32, index: u32, buffer: u32) {
    bind_buffer_range(ctx, target, index, buffer, 0, 0);
}

/// `glBindBufferRange(target, index, buffer, offset, size)` — bind `[offset, offset+size)` of `buffer` to
/// indexed slot `index` (`size == 0` from `glBindBufferBase` = the whole buffer). Honest GL errors: a
/// non-indexed `target` → `GL_INVALID_ENUM`; `index >= cap` or a non-zero `buffer` with a non-positive
/// size / negative offset → `GL_INVALID_VALUE` (first-error-wins).
pub fn bind_buffer_range(ctx: &mut GlContext, target: u32, index: u32, buffer: u32, offset: isize, size: isize) {
    let Some(cap) = indexed_target_cap(target) else {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return;
    };
    if index >= cap {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    // glBindBufferRange (a non-zero buffer) requires a positive size + non-negative offset.
    if buffer != 0 && size != 0 && (size < 0 || offset < 0) {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    if std::env::var("HL_UBO_DUMP").is_ok() && target == GL_UNIFORM_BUFFER {
        eprintln!("[UBO_DUMP] glBindBufferRange target={target:#x} index={index} buffer={buffer} offset={offset} size={size}");
    }
    // Bind the generic target too (GL binds both), so a later glBufferData(target, …) fills this buffer.
    bind_buffer(ctx, target, buffer);
    if buffer == 0 {
        ctx.indexed_buffers.remove(&(target, index));
    } else {
        ctx.indexed_buffers.insert((target, index), IndexedBinding { buffer, offset, size });
    }
}

/// The indexed-buffer binding at `(target, index)` (`glBindBufferBase`/`glBindBufferRange`), or `None` if
/// nothing is bound there. Exposed for the lowering tests + the compute bind-group builder.
pub fn indexed_buffer_binding(ctx: &GlContext, target: u32, index: u32) -> Option<IndexedBinding> {
    ctx.indexed_buffers.get(&(target, index)).copied()
}

// ---- MRT draw/read buffer selection (glDrawBuffers / glReadBuffer) --------------------------------

/// `glDrawBuffers(bufs)` — record the fragment-output color-buffer list. Each entry must be `GL_NONE`,
/// `GL_BACK` (default framebuffer), or a `GL_COLOR_ATTACHMENT{i}` (FBO) — else `GL_INVALID_ENUM`. This
/// model renders a single color target, so the list round-trips faithfully but only the first attachment
/// is materialized (an honest partial).
pub fn draw_buffers(ctx: &mut GlContext, bufs: &[u32]) {
    for &b in bufs {
        let ok = b == GL_NONE
            || b == GL_BACK
            || (GL_COLOR_ATTACHMENT0..=GL_COLOR_ATTACHMENT0 + 15).contains(&b);
        if !ok {
            ctx.set_gl_error(GL_INVALID_ENUM);
            return;
        }
    }
    ctx.draw_buffers = bufs.to_vec();
}

/// `glReadBuffer(src)` — select the color buffer subsequent `glReadPixels`/blit reads from. `src` must be
/// `GL_NONE`, `GL_BACK`, or a `GL_COLOR_ATTACHMENT{i}` (else `GL_INVALID_ENUM`).
pub fn read_buffer(ctx: &mut GlContext, src: u32) {
    let ok = src == GL_NONE
        || src == GL_BACK
        || (GL_COLOR_ATTACHMENT0..=GL_COLOR_ATTACHMENT0 + 15).contains(&src);
    if !ok {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return;
    }
    ctx.read_buffer_src = src;
}

// ---- textures ------------------------------------------------------------------------------------

/// `glGenTextures` (one name).
pub fn gen_texture(ctx: &mut GlContext) -> u32 {
    ctx.textures.gen()
}

/// `glActiveTexture(GL_TEXTURE0 + i)`.
pub fn active_texture(ctx: &mut GlContext, texture: u32) {
    let unit = texture.wrapping_sub(GL_TEXTURE0) as usize;
    if unit < ctx.tex_unit.len() {
        ctx.active_texture = unit;
    }
}

/// `glBindTexture(GL_TEXTURE_2D, name)` — binds to the active texture unit.
pub fn bind_texture(ctx: &mut GlContext, _target: u32, name: u32) {
    let unit = ctx.active_texture;
    if unit < ctx.tex_unit.len() {
        ctx.tex_unit[unit] = name;
    }
}

/// `glTexImage2D` — `pixels` is the already-RGBA8-converted image (`w*h*4`) bound to the active unit; the
/// texture lowers to the default `Rgba8Unorm` neutral format.
pub fn tex_image_2d(ctx: &mut GlContext, w: i32, h: i32, pixels: &[u8]) {
    tex_image_2d_format(ctx, w, h, pixels, TextureFormat::Rgba8Unorm);
}

/// `glTexImage2D` with an explicit neutral texel `format` selected from the GL internal format — used for
/// FBO color attachments (which are rendered into, so the format becomes the render-target + surface
/// format) and for non-RGBA8 sampled uploads (e.g. a `GL_BGRA_EXT` image → `Bgra8Unorm`).
pub fn tex_image_2d_format(ctx: &mut GlContext, w: i32, h: i32, pixels: &[u8], format: TextureFormat) {
    // A negative or over-max extent is GL_INVALID_VALUE (real GL rejects beyond GL_MAX_TEXTURE_SIZE). This
    // also bounds the zeroed-storage allocation `image_2d` does for an empty (storage-only) upload, so a
    // hostile `glTexImage2D(40000, 40000, NULL)` can never trigger a multi-GiB host allocation.
    if w < 0 || h < 0 || w > crate::service::query::MAX_TEXTURE_SIZE || h > crate::service::query::MAX_TEXTURE_SIZE {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    let name = ctx.tex_unit[ctx.active_texture];
    if name != 0 {
        ctx.textures.image_2d(name, w, h, pixels, format);
    }
}

/// `glTexParameteri(GL_TEXTURE_2D, pname, value)` on the active unit's texture.
pub fn tex_parameter(ctx: &mut GlContext, pname: u32, value: u32) {
    let name = ctx.tex_unit[ctx.active_texture];
    if name != 0 {
        ctx.textures.set_param(name, pname, value);
    }
}

/// `glGenerateMipmap(target)` — validate the request. This model samples only the base level (the
/// neutral-IR textures carry a single mip), so the mip chain is not materialized — an honest no-op on
/// the pixel data. `target` must be a 2D/cube texture target (else `GL_INVALID_ENUM`) with a texture
/// bound to the active unit (else `GL_INVALID_OPERATION`); the state is otherwise unchanged.
pub fn generate_mipmap(ctx: &mut GlContext, target: u32) {
    if target != GL_TEXTURE_2D && target != GL_TEXTURE_CUBE_MAP {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return;
    }
    if ctx.tex_unit[ctx.active_texture] == 0 {
        ctx.set_gl_error(GL_INVALID_OPERATION);
    }
}

/// `glTexStorage2D(target, levels, internalformat, w, h)` — immutable-format allocation of the bound
/// 2D texture. Mirrors `gl_shim.c`: allocate the RGBA8 base plane (so a later `glTexSubImage2D` has a
/// target), mark the texture immutable, and record `levels`. Honest GL errors: bad `target` →
/// `GL_INVALID_ENUM`; `levels != 1`, non-positive extent, or an unmodeled internalformat →
/// `GL_INVALID_VALUE`; no bound texture, unknown texture, or an already-immutable texture →
/// `GL_INVALID_OPERATION`.
pub fn tex_storage_2d(ctx: &mut GlContext, target: u32, levels: i32, internalformat: u32, w: i32, h: i32) {
    if target != GL_TEXTURE_2D {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return;
    }
    if levels != 1 || w <= 0 || h <= 0 || !matches!(internalformat, GL_RGB | GL_RGBA) {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    if w > crate::service::query::MAX_TEXTURE_SIZE || h > crate::service::query::MAX_TEXTURE_SIZE {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    let name = ctx.tex_unit[ctx.active_texture];
    match ctx.textures.get(name) {
        _ if name == 0 => {
            ctx.set_gl_error(GL_INVALID_OPERATION);
            return;
        }
        Some(t) if !t.immutable => {}
        _ => {
            ctx.set_gl_error(GL_INVALID_OPERATION);
            return;
        }
    }
    if !ctx.textures.storage_2d(name, w, h, levels, TextureFormat::Rgba8Unorm) {
        ctx.set_gl_error(GL_INVALID_VALUE);
    }
}

/// `glTexStorage3D(target, levels, internalformat, w, h, depth)` — immutable storage for a 2D-array / 3D
/// texture; `gl_shim.c` allocates the layer-0 RGBA8 plane. A non-array/3D `target` is `GL_INVALID_ENUM`;
/// a non-positive extent is `GL_INVALID_VALUE`.
pub fn tex_storage_3d(ctx: &mut GlContext, target: u32, levels: i32, w: i32, h: i32, depth: i32) {
    if target != GL_TEXTURE_2D_ARRAY && target != GL_TEXTURE_3D {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return;
    }
    if levels < 1 || w <= 0 || h <= 0 || depth <= 0 {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    let name = ctx.tex_unit[ctx.active_texture];
    if name == 0 || ctx.textures.get(name).is_none() {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    if ctx.textures.storage_2d(name, w, h, levels, TextureFormat::Rgba8Unorm) {
        // storage_2d marks immutable; that is correct for glTexStorage3D too.
    } else {
        ctx.set_gl_error(GL_INVALID_VALUE);
    }
}

/// `glTexImage3D` — 2D-array / 3D upload; `gl_shim.c` allocates RGBA8 and stores the layer-0 plane.
/// `rgba` is the already-converted RGBA8 layer-0 image (`w*h*4`) or empty (storage-only). A bad `target`
/// / `level != 0` / unknown texture is an honest no-op (matches the reference).
pub fn tex_image_3d(ctx: &mut GlContext, target: u32, level: i32, w: i32, h: i32, depth: i32, rgba: &[u8]) {
    if level != 0 || (target != GL_TEXTURE_2D_ARRAY && target != GL_TEXTURE_3D) {
        return;
    }
    let name = ctx.tex_unit[ctx.active_texture];
    if name == 0 || ctx.textures.get(name).is_none() {
        return;
    }
    if !ctx.textures.alloc_rgba(name, w, h) {
        return;
    }
    if !rgba.is_empty() && depth > 0 {
        ctx.textures.sub_image_2d(name, 0, 0, w, h, rgba);
    }
}

/// `glTexSubImage2D(target, level, xo, yo, w, h, format, type, pixels)` — overwrite a sub-rect of the
/// bound 2D texture with the already-converted `rgba` (`w*h*4`). Honest GL errors: bad `target` →
/// `GL_INVALID_ENUM`; `level != 0`, negative extent/offset, no/unknown texture, or an out-of-bounds rect
/// → `GL_INVALID_VALUE`.
pub fn tex_sub_image_2d(ctx: &mut GlContext, target: u32, level: i32, xo: i32, yo: i32, w: i32, h: i32, rgba: &[u8]) {
    if target != GL_TEXTURE_2D {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return;
    }
    let name = ctx.tex_unit[ctx.active_texture];
    if level != 0 || w < 0 || h < 0 || xo < 0 || yo < 0 || name == 0 {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    if w == 0 || h == 0 {
        return;
    }
    if !ctx.textures.sub_image_2d(name, xo, yo, w, h, rgba) {
        ctx.set_gl_error(GL_INVALID_VALUE);
    }
}

/// `glTexSubImage3D` — layer-0 sub-image update for a 2D-array / 3D texture (`gl_shim.c` parity). A bad
/// `target` / `level != 0` / `zoffset != 0` / non-positive depth / dataless texture is an honest no-op.
pub fn tex_sub_image_3d(
    ctx: &mut GlContext,
    target: u32,
    level: i32,
    xo: i32,
    yo: i32,
    zo: i32,
    w: i32,
    h: i32,
    depth: i32,
    rgba: &[u8],
) {
    if level != 0 || zo != 0 || depth <= 0 || (target != GL_TEXTURE_2D_ARRAY && target != GL_TEXTURE_3D) {
        return;
    }
    let name = ctx.tex_unit[ctx.active_texture];
    if name == 0 {
        return;
    }
    ctx.textures.sub_image_2d(name, xo, yo, w, h, rgba);
}

/// `glCopyTexSubImage2D(target, level, xo, yo, x, y, w, h)` — copy a rect of the READ framebuffer's color
/// attachment into the bound texture's sub-rect.
///
/// HONEST LIMIT: like `glBlitFramebuffer`, the deferred model has no materialized source color plane at
/// record time (a frame's pixels exist only after swap), and the default framebuffer has no backing
/// texture object. So when the read framebuffer's color attachment carries materialized pixels the rect
/// is copied CPU-side; otherwise the call validates and is a documented no-op. Honest GL errors: bad
/// `target` / `level != 0` → `GL_INVALID_ENUM` / `GL_INVALID_VALUE`; a negative extent/offset or an
/// out-of-bounds destination rect → `GL_INVALID_VALUE`.
#[allow(clippy::too_many_arguments)]
pub fn copy_tex_sub_image_2d(ctx: &mut GlContext, target: u32, level: i32, xo: i32, yo: i32, x: i32, y: i32, w: i32, h: i32) {
    if target != GL_TEXTURE_2D {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return;
    }
    let dst = ctx.tex_unit[ctx.active_texture];
    if level != 0 || w < 0 || h < 0 || xo < 0 || yo < 0 || x < 0 || y < 0 || dst == 0 {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    if w == 0 || h == 0 {
        return;
    }
    // Validate the destination rect fits the bound texture. The `+` are widened to i64 so a huge (or
    // adversarial) offset/extent can never overflow an i32 and panic — it just fails the fit and raises
    // GL_INVALID_VALUE.
    match ctx.textures.get(dst) {
        Some(t)
            if !t.data.is_empty()
                && xo as i64 + w as i64 <= t.w as i64
                && yo as i64 + h as i64 <= t.h as i64 => {}
        Some(_) => {
            ctx.set_gl_error(GL_INVALID_VALUE);
            return;
        }
        None => {
            ctx.set_gl_error(GL_INVALID_VALUE);
            return;
        }
    }
    // Source: the read framebuffer's color-attachment texture (if any). Copy the overlapping RGBA rows;
    // an absent/dataless source (default framebuffer, or an FBO not yet rendered) is the honest no-op.
    let src = ctx.framebuffers.color_attachment(ctx.read_fbo);
    let src_rows: Option<(Vec<u8>, usize)> = ctx.textures.get(src).and_then(|st| {
        if st.data.is_empty() || x as i64 + w as i64 > st.w as i64 || y as i64 + h as i64 > st.h as i64 {
            return None;
        }
        let (sw, w, h) = (st.w as usize, w as usize, h as usize);
        let (x, y) = (x as usize, y as usize);
        let mut buf = Vec::with_capacity(w * h * 4);
        for row in 0..h {
            let base = ((y + row) * sw + x) * 4;
            buf.extend_from_slice(&st.data[base..base + w * 4]);
        }
        Some((buf, w))
    });
    if let Some((buf, _)) = src_rows {
        ctx.textures.sub_image_2d(dst, xo, yo, w, h, &buf);
    }
}

/// `glDeleteTextures` (one name).
pub fn delete_texture(ctx: &mut GlContext, name: u32) -> bool {
    for u in ctx.tex_unit.iter_mut() {
        if *u == name {
            *u = 0;
        }
    }
    ctx.textures.delete(name)
}

// ---- framebuffers (offscreen render targets) -----------------------------------------------------

/// `glGenFramebuffers` (one name).
pub fn gen_framebuffer(ctx: &mut GlContext) -> u32 {
    ctx.framebuffers.gen()
}

/// `glBindFramebuffer(target, name)` — bind `name` as the draw and/or read framebuffer (`0` = default).
/// `GL_FRAMEBUFFER` binds both; the split `GL_DRAW_FRAMEBUFFER`/`GL_READ_FRAMEBUFFER` bind one. A recorded
/// draw's render target follows the draw binding; the read binding is the `glReadPixels`/blit source.
pub fn bind_framebuffer(ctx: &mut GlContext, target: u32, name: u32) {
    match target {
        GL_FRAMEBUFFER => {
            ctx.bound_fbo = name;
            ctx.read_fbo = name;
        }
        GL_DRAW_FRAMEBUFFER => ctx.bound_fbo = name,
        GL_READ_FRAMEBUFFER => ctx.read_fbo = name,
        _ => ctx.set_gl_error(GL_INVALID_ENUM),
    }
}

/// `glFramebufferTexture2D(target, attachment, textarget, tex, level)` — attach `tex` as the bound FBO's
/// color target. Only `GL_COLOR_ATTACHMENT0` of a `GL_TEXTURE_2D` at level `0` is modeled; the default
/// framebuffer `0` has no attachable slot. Honest GL errors: bad `target` → `GL_INVALID_ENUM`; an
/// unmodeled attachment/textarget/level → `GL_INVALID_VALUE`; attaching to the default framebuffer or an
/// unknown texture → `GL_INVALID_OPERATION` (all first-error-wins).
pub fn framebuffer_texture_2d(ctx: &mut GlContext, target: u32, attachment: u32, textarget: u32, tex: u32, level: i32) {
    if !matches!(target, GL_FRAMEBUFFER | GL_DRAW_FRAMEBUFFER | GL_READ_FRAMEBUFFER) {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return;
    }
    let fbo = if target == GL_READ_FRAMEBUFFER { ctx.read_fbo } else { ctx.bound_fbo };
    // GL_COLOR_ATTACHMENT0..15 are all attachable (MRT); a non-color attachment / textarget / level is
    // unmodeled. The attachment index is `attachment - GL_COLOR_ATTACHMENT0`.
    let is_color = (GL_COLOR_ATTACHMENT0..=GL_COLOR_ATTACHMENT0 + 15).contains(&attachment);
    if !is_color || textarget != GL_TEXTURE_2D || level != 0 {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    if fbo == 0 {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    if tex != 0 && ctx.textures.get(tex).is_none() {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    ctx.framebuffers.attach_color_index(fbo, attachment - GL_COLOR_ATTACHMENT0, tex);
}

/// `glDeleteFramebuffers` (one name). Deleting the bound draw/read FBO reverts that binding to the default.
pub fn delete_framebuffer(ctx: &mut GlContext, name: u32) -> bool {
    if ctx.bound_fbo == name {
        ctx.bound_fbo = 0;
    }
    if ctx.read_fbo == name {
        ctx.read_fbo = 0;
    }
    ctx.framebuffers.delete(name)
}

/// `glIsFramebuffer(name)` — true once `name` names a generated (non-default) framebuffer object.
pub fn is_framebuffer(ctx: &GlContext, name: u32) -> bool {
    ctx.framebuffers.exists(name)
}

/// `glCheckFramebufferStatus(target)` — completeness of the bound draw/read framebuffer. Returns
/// `GL_FRAMEBUFFER_COMPLETE` for the default framebuffer or a user FBO with a sized color attachment,
/// `GL_FRAMEBUFFER_INCOMPLETE_MISSING_ATTACHMENT` for a user FBO with no color attachment, and
/// `GL_FRAMEBUFFER_INCOMPLETE_ATTACHMENT` when the color attachment is unsized (e.g. a renderbuffer with
/// no `glRenderbufferStorage` yet). A bad `target` raises `GL_INVALID_ENUM` and returns `0`.
pub fn check_framebuffer_status(ctx: &mut GlContext, target: u32) -> u32 {
    let fbo = match target {
        GL_FRAMEBUFFER | GL_DRAW_FRAMEBUFFER => ctx.bound_fbo,
        GL_READ_FRAMEBUFFER => ctx.read_fbo,
        _ => {
            ctx.set_gl_error(GL_INVALID_ENUM);
            return 0;
        }
    };
    framebuffer_status(ctx, fbo)
}

/// Completeness for the color-only framebuffer subset this model renders. The default framebuffer (`0`)
/// is managed by EGL and is complete; a user FBO needs one sized, live color-texture attachment.
fn framebuffer_status(ctx: &GlContext, fbo: u32) -> u32 {
    if fbo == 0 {
        return GL_FRAMEBUFFER_COMPLETE;
    }
    if !ctx.framebuffers.exists(fbo) {
        return GL_FRAMEBUFFER_INCOMPLETE_MISSING_ATTACHMENT;
    }
    let color = ctx.framebuffers.color_attachment(fbo);
    if color == 0 {
        return GL_FRAMEBUFFER_INCOMPLETE_MISSING_ATTACHMENT;
    }
    match ctx.textures.get(color) {
        Some(t) if t.w > 0 && t.h > 0 => GL_FRAMEBUFFER_COMPLETE,
        _ => GL_FRAMEBUFFER_INCOMPLETE_ATTACHMENT,
    }
}

// ---- renderbuffers (modeled as texture-backed color attachments) ---------------------------------

/// `glGenRenderbuffers` (one name). Eagerly mints the RBO's stable backing texture (still unsized, so it
/// reads back incomplete until `glRenderbufferStorage`), so a `glFramebufferRenderbuffer` that runs before
/// the storage call still resolves to the right attachment once storage lands.
pub fn gen_renderbuffer(ctx: &mut GlContext) -> u32 {
    let name = ctx.renderbuffers.gen();
    let tex = ctx.textures.gen();
    ctx.renderbuffers.set_storage(name, tex, 0, 0);
    name
}

/// `glBindRenderbuffer(GL_RENDERBUFFER, name)` — select the target of the next `glRenderbufferStorage`.
pub fn bind_renderbuffer(ctx: &mut GlContext, target: u32, name: u32) {
    if target != GL_RENDERBUFFER {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return;
    }
    ctx.bound_rbo = name;
}

/// `glRenderbufferStorage(GL_RENDERBUFFER, internalformat, w, h)` — size the bound renderbuffer's backing
/// texture. The model materializes every renderbuffer as an RGBA8 color plane (its neutral render-target
/// format), so `internalformat` selects only the extent here. Honest GL errors: bad `target` →
/// `GL_INVALID_ENUM`; no bound renderbuffer → `GL_INVALID_OPERATION`; negative extent → `GL_INVALID_VALUE`.
pub fn renderbuffer_storage(ctx: &mut GlContext, target: u32, _internalformat: u32, w: i32, h: i32) {
    if target != GL_RENDERBUFFER {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return;
    }
    let rbo = ctx.bound_rbo;
    if rbo == 0 {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    // A negative extent, or one beyond the advertised GL_MAX_RENDERBUFFER_SIZE, is GL_INVALID_VALUE (real
    // GL rejects an oversized renderbuffer). This also bounds the backing-plane allocation to a sane size.
    if w < 0 || h < 0 || w > crate::service::query::MAX_TEXTURE_SIZE || h > crate::service::query::MAX_TEXTURE_SIZE {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    // Reuse the RBO's stable backing texture (minted at gen) so an earlier attachment stays wired.
    let tex = match ctx.renderbuffers.backing_tex(rbo) {
        0 => ctx.textures.gen(),
        t => t,
    };
    ctx.textures.image_2d(tex, w, h, &[], TextureFormat::Rgba8Unorm);
    ctx.renderbuffers.set_storage(rbo, tex, w, h);
}

/// `glDeleteRenderbuffers` (one name). Detaches the backing texture from every FBO color slot and drops
/// the backing texture. Returns `false` for an unknown / zero name.
pub fn delete_renderbuffer(ctx: &mut GlContext, name: u32) -> bool {
    if ctx.bound_rbo == name {
        ctx.bound_rbo = 0;
    }
    match ctx.renderbuffers.delete(name) {
        Some(rb) => {
            ctx.framebuffers.detach_color_texture(rb.tex);
            ctx.textures.delete(rb.tex);
            true
        }
        None => false,
    }
}

/// `glIsRenderbuffer(name)` — true once `name` names a generated (non-default) renderbuffer object.
pub fn is_renderbuffer(ctx: &GlContext, name: u32) -> bool {
    ctx.renderbuffers.is_renderbuffer(name)
}

/// `glFramebufferRenderbuffer(target, attachment, renderbuffertarget, rbo)` — attach a renderbuffer to the
/// bound FBO. The color attachment resolves to the renderbuffer's backing texture (reusing the exact
/// texture-attachment render path); depth/stencil attachments are accepted as an honest no-op (this model
/// has no depth/stencil buffer). Honest GL errors: bad `target`/`renderbuffertarget`/attachment →
/// `GL_INVALID_ENUM`; attaching to the default framebuffer or an unknown renderbuffer → `GL_INVALID_OPERATION`.
pub fn framebuffer_renderbuffer(ctx: &mut GlContext, target: u32, attachment: u32, rbtarget: u32, rbo: u32) {
    if !matches!(target, GL_FRAMEBUFFER | GL_DRAW_FRAMEBUFFER | GL_READ_FRAMEBUFFER) || rbtarget != GL_RENDERBUFFER {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return;
    }
    let fbo = if target == GL_READ_FRAMEBUFFER { ctx.read_fbo } else { ctx.bound_fbo };
    if fbo == 0 {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    if rbo != 0 && !ctx.renderbuffers.is_renderbuffer(rbo) {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    match attachment {
        GL_COLOR_ATTACHMENT0 => {
            // The renderbuffer's texture-backed storage becomes the FBO's color target (`0` detaches).
            let tex = ctx.renderbuffers.backing_tex(rbo);
            ctx.framebuffers.attach_color(fbo, tex);
        }
        // No depth/stencil buffer is modeled — accept the attach as a no-op so a guest that attaches a
        // depth/stencil renderbuffer still runs (its color attachment is what this model renders).
        GL_DEPTH_ATTACHMENT | GL_STENCIL_ATTACHMENT | GL_DEPTH_STENCIL_ATTACHMENT => {}
        _ => ctx.set_gl_error(GL_INVALID_ENUM),
    }
}

/// `glBlitFramebuffer(srcX0,srcY0,srcX1,srcY1, dstX0,dstY0,dstX1,dstY1, mask, filter)` — validate the
/// read+draw framebuffers and RECORD the color blit for the frame builder.
///
/// The deferred model applies the blit AFTER the frame's render passes: its source is the read FBO's
/// rendered color attachment and its destination is the draw FBO's, both materialized as render-target
/// textures. For the equal-size (non-scaling) case the frame lowers this to `Enc::CopyTextureToTexture`
/// (the executor implements exact texture→texture copy); a SCALING blit (source extent != destination
/// extent) lowers to `Enc::BlitTexture` carrying the resampling `filter`. A non-color `mask` is a no-op, and an incomplete
/// read or draw framebuffer raises `GL_INVALID_FRAMEBUFFER_OPERATION` (first-error-wins) — a conforming
/// driver never samples an incomplete attachment.
#[allow(clippy::too_many_arguments)]
pub fn blit_framebuffer(
    ctx: &mut GlContext,
    src_x0: i32,
    src_y0: i32,
    src_x1: i32,
    src_y1: i32,
    dst_x0: i32,
    dst_y0: i32,
    dst_x1: i32,
    dst_y1: i32,
    mask: u32,
    filter: u32,
) {
    if mask & GL_COLOR_BUFFER_BIT == 0 {
        return;
    }
    if framebuffer_status(ctx, ctx.read_fbo) != GL_FRAMEBUFFER_COMPLETE
        || framebuffer_status(ctx, ctx.bound_fbo) != GL_FRAMEBUFFER_COMPLETE
    {
        ctx.set_gl_error(GL_INVALID_FRAMEBUFFER_OPERATION);
        return;
    }
    // GL defines only GL_NEAREST and GL_LINEAR for a color blit; anything else falls back to Nearest.
    let filter = if filter == crate::model::glconst::GL_LINEAR {
        hl_gpu::protocol::model::enums::Filter::Linear
    } else {
        hl_gpu::protocol::model::enums::Filter::Nearest
    };
    ctx.blits.push(crate::model::context::BlitOp {
        read_fbo: ctx.read_fbo,
        draw_fbo: ctx.bound_fbo,
        src: [src_x0, src_y0, src_x1, src_y1],
        dst: [dst_x0, dst_y0, dst_x1, dst_y1],
        filter,
    });
}

// ---- vertex array objects ------------------------------------------------------------------------

/// `glGenVertexArrays` (one name).
pub fn gen_vertex_array(ctx: &mut GlContext) -> u32 {
    ctx.gen_vertex_array()
}

/// `glBindVertexArray(vao)` — swap the captured attrib/element-buffer state (see
/// [`GlContext::bind_vertex_array`]).
pub fn bind_vertex_array(ctx: &mut GlContext, vao: u32) {
    ctx.bind_vertex_array(vao);
}

/// `glDeleteVertexArrays` (one name).
pub fn delete_vertex_array(ctx: &mut GlContext, vao: u32) -> bool {
    ctx.delete_vertex_array(vao)
}

/// `glIsVertexArray(vao)`.
pub fn is_vertex_array(ctx: &GlContext, vao: u32) -> bool {
    ctx.is_vertex_array(vao)
}

// ---- shaders + programs --------------------------------------------------------------------------

/// `glCreateShader(kind)`.
pub fn create_shader(ctx: &mut GlContext, kind: u32) -> u32 {
    ctx.programs.create_shader(kind)
}

/// `glShaderSource(shader, src)`.
pub fn shader_source(ctx: &mut GlContext, shader: u32, src: &str) {
    ctx.programs.shader_source(shader, src);
}

/// `glCompileShader(shader)`.
pub fn compile_shader(ctx: &mut GlContext, shader: u32) {
    ctx.programs.compile_shader(shader);
}

/// `glCreateProgram()`.
pub fn create_program(ctx: &mut GlContext) -> u32 {
    ctx.programs.create_program()
}

/// `glAttachShader(program, shader)`.
pub fn attach_shader(ctx: &mut GlContext, program: u32, shader: u32) {
    ctx.programs.attach(program, shader);
}

/// `glLinkProgram(program)` — translate the attached GLSL-ES pair to shader-IR + reflect the layout.
pub fn link_program(ctx: &mut GlContext, program: u32) -> bool {
    ctx.programs.link(program)
}

/// `glUseProgram(program)`.
pub fn use_program(ctx: &mut GlContext, program: u32) {
    ctx.cur_prog = program;
}

/// `glUniform1i(samplerLocation, unit)` — map a sampler uniform (by declaration index) to a texture
/// unit. Simplified: `sampler_index` is the sampler's position in the program's `samp_names`.
pub fn uniform_sampler(ctx: &mut GlContext, sampler_index: usize, unit: i32) {
    if let Some(p) = ctx.programs.program_mut(ctx.cur_prog) {
        if sampler_index < p.samp_units.len() {
            p.samp_units[sampler_index] = unit;
        }
    }
}

/// `glUniform*` for a data uniform — write `bytes` into the bound program's uniform-block buffer at the
/// named member's offset. Simplified name-keyed write (real GL uses integer locations).
pub fn uniform_data(ctx: &mut GlContext, name: &str, bytes: &[u8]) {
    if let Some(p) = ctx.programs.program_mut(ctx.cur_prog) {
        if let Some(u) = p.unis.iter().find(|u| u.name == name) {
            let off = u.off as usize;
            let end = (off + bytes.len()).min(p.ubuf.len());
            if off < p.ubuf.len() {
                p.ubuf[off..end].copy_from_slice(&bytes[..end - off]);
            }
        }
    }
}

/// `glUniform*`/`glUniformMatrix*` — write the already-marshalled little-endian `bytes` of a data uniform
/// into the bound program's uniform-block buffer. `location` is the uniform's declaration index (its
/// position in the program's reflected `unis`), matching the sampler-location convention used by
/// [`uniform_sampler`]; the frame builder ships the resulting `ubuf` at binding 1 so the draw's shader
/// reads the value. Out-of-range writes (bad location / oversized payload) are truncated to the slot.
pub fn uniform_at(ctx: &mut GlContext, location: usize, bytes: &[u8]) {
    if let Some(p) = ctx.programs.program_mut(ctx.cur_prog) {
        let (off, sz) = match p.unis.get(location) {
            Some(u) => (u.off as usize, u.sz as usize),
            None => return,
        };
        if off >= p.ubuf.len() {
            return;
        }
        // Clamp to both the member's declared size and the block's byte length.
        let n = bytes.len().min(sz).min(p.ubuf.len() - off);
        p.ubuf[off..off + n].copy_from_slice(&bytes[..n]);
    }
}

// ---- fixed-function state ------------------------------------------------------------------------

/// `glVertexAttribPointer` + implicit `glEnableVertexAttribArray` is separate.
#[allow(clippy::too_many_arguments)]
pub fn vertex_attrib_pointer(
    ctx: &mut GlContext,
    location: usize,
    size: i32,
    kind: u32,
    normalized: bool,
    stride: i32,
    offset: usize,
) {
    if location < ctx.attr.len() {
        let a = &mut ctx.attr[location];
        a.size = size;
        a.kind = kind;
        a.normalized = normalized;
        a.integer = false;
        a.stride = stride;
        a.offset = offset;
        a.buffer = ctx.array_buffer;
    }
}

/// `glVertexAttribDivisor(index, divisor)` — set the instance-step divisor for attribute `index`
/// (`0` = per-vertex, `>0` = per-instance). Recorded per attribute; the frame builder marks a
/// vertex-buffer slot instance-stepped when its attributes carry a non-zero divisor.
pub fn vertex_attrib_divisor(ctx: &mut GlContext, index: usize, divisor: u32) {
    if index < ctx.attr.len() {
        ctx.attr[index].divisor = divisor;
    }
}

/// `glEnableVertexAttribArray(location)`.
pub fn enable_vertex_attrib(ctx: &mut GlContext, location: usize) {
    if location < ctx.attr.len() {
        ctx.attr[location].enabled = true;
    }
}

/// `glDisableVertexAttribArray(location)`.
pub fn disable_vertex_attrib(ctx: &mut GlContext, location: usize) {
    if location < ctx.attr.len() {
        ctx.attr[location].enabled = false;
    }
}

/// `glClearColor(r, g, b, a)`.
pub fn clear_color(ctx: &mut GlContext, rgba: [f32; 4]) {
    ctx.clear_color = rgba;
}

/// `glClearDepthf(d)` — recorded for completeness (no depth attachment is modeled, so it is not lowered).
pub fn clear_depth(ctx: &mut GlContext, d: f32) {
    ctx.clear_depth = d;
}

/// `glBlendFunc(src, dst)` — set the same factor pair for RGB and alpha.
pub fn blend_func(ctx: &mut GlContext, src: u32, dst: u32) {
    ctx.blend_src_rgb = src;
    ctx.blend_dst_rgb = dst;
    ctx.blend_src_alpha = src;
    ctx.blend_dst_alpha = dst;
}

/// `glBlendFuncSeparate(srcRGB, dstRGB, srcAlpha, dstAlpha)`.
pub fn blend_func_separate(ctx: &mut GlContext, src_rgb: u32, dst_rgb: u32, src_a: u32, dst_a: u32) {
    ctx.blend_src_rgb = src_rgb;
    ctx.blend_dst_rgb = dst_rgb;
    ctx.blend_src_alpha = src_a;
    ctx.blend_dst_alpha = dst_a;
}

/// `glDepthFunc(func)` — set the depth-compare function.
pub fn depth_func(ctx: &mut GlContext, func: u32) {
    ctx.depth_func = func;
}

/// `glDepthMask(flag)` — enable/disable depth writes.
pub fn depth_mask(ctx: &mut GlContext, write: bool) {
    ctx.depth_write = write;
}

/// `glCullFace(mode)` — select the culled face (`GL_FRONT` / `GL_BACK` / `GL_FRONT_AND_BACK`).
pub fn cull_face(ctx: &mut GlContext, mode: u32) {
    ctx.cull_face = mode;
}

/// `glFrontFace(mode)` — select the front-face winding (`GL_CW` / `GL_CCW`).
pub fn front_face(ctx: &mut GlContext, mode: u32) {
    ctx.front_face = mode;
}

/// `glViewport(x, y, w, h)`.
pub fn viewport(ctx: &mut GlContext, vp: [i32; 4]) {
    ctx.viewport = vp;
}

/// `glPixelStorei(pname, value)` — record a pack/unpack pixel-store parameter (affecting texture upload /
/// readback packing). Alignments accept only `{1,2,4,8}`; row-length/skip parameters must be non-negative.
/// An out-of-range value raises `GL_INVALID_VALUE` (first-error-wins) and leaves the parameter unchanged;
/// an unrecognized `pname` is ignored (the long tail of pack/unpack params this model does not track).
pub fn pixel_store(ctx: &mut GlContext, pname: u32, value: i32) {
    let ps = &mut ctx.pixel_store;
    let ok = match pname {
        GL_UNPACK_ALIGNMENT if matches!(value, 1 | 2 | 4 | 8) => {
            ps.unpack_alignment = value;
            true
        }
        GL_PACK_ALIGNMENT if matches!(value, 1 | 2 | 4 | 8) => {
            ps.pack_alignment = value;
            true
        }
        GL_UNPACK_ROW_LENGTH if value >= 0 => {
            ps.unpack_row_length = value;
            true
        }
        GL_UNPACK_SKIP_ROWS if value >= 0 => {
            ps.unpack_skip_rows = value;
            true
        }
        GL_UNPACK_SKIP_PIXELS if value >= 0 => {
            ps.unpack_skip_pixels = value;
            true
        }
        GL_PACK_ROW_LENGTH if value >= 0 => {
            ps.pack_row_length = value;
            true
        }
        GL_PACK_SKIP_ROWS if value >= 0 => {
            ps.pack_skip_rows = value;
            true
        }
        GL_PACK_SKIP_PIXELS if value >= 0 => {
            ps.pack_skip_pixels = value;
            true
        }
        // A recognized parameter with an out-of-range value is GL_INVALID_VALUE.
        GL_UNPACK_ALIGNMENT | GL_PACK_ALIGNMENT | GL_UNPACK_ROW_LENGTH | GL_UNPACK_SKIP_ROWS
        | GL_UNPACK_SKIP_PIXELS | GL_PACK_ROW_LENGTH | GL_PACK_SKIP_ROWS | GL_PACK_SKIP_PIXELS => {
            ctx.set_gl_error(GL_INVALID_VALUE);
            return;
        }
        // Unrecognized pname: an untracked pack/unpack parameter — leave state unchanged.
        _ => true,
    };
    let _ = ok;
}

/// `glScissor(x, y, w, h)`.
pub fn scissor(ctx: &mut GlContext, sc: [i32; 4]) {
    ctx.scissor = sc;
}

/// `glEnable(cap)`.
pub fn enable(ctx: &mut GlContext, cap: u32) {
    set_cap(ctx, cap, true);
}

/// `glDisable(cap)`.
pub fn disable(ctx: &mut GlContext, cap: u32) {
    set_cap(ctx, cap, false);
}

fn set_cap(ctx: &mut GlContext, cap: u32, on: bool) {
    match cap {
        GL_BLEND => ctx.blend = on,
        GL_DEPTH_TEST => ctx.depth = on,
        GL_SCISSOR_TEST => ctx.scissor_enabled = on,
        GL_CULL_FACE => ctx.cull_enabled = on,
        _ => {}
    }
}

// ---- draw + clear recording ----------------------------------------------------------------------

/// `glClear(mask)` — record a full-surface clear rect at the current clear color (color bit assumed).
pub fn clear(ctx: &mut GlContext) {
    let (w, h) = ctx.target_wh();
    let mut d = DrawCall { is_clear: true, ..snapshot(ctx) };
    d.clear_rect = [0, 0, w, h];
    ctx.draws.push(d);
}

/// Read one enabled CLIENT-side vertex attribute's bytes into a tightly-packed, de-interleaved buffer
/// spanning logical vertices `[0, vert_end)`, honoring `stride` (`0` = tightly packed by
/// `size * component_size`). The transient buffer is indexed by the draw's own `first_vertex` / index
/// values, so it must span from vertex `0` (not `first`) up to the last vertex the draw fetches.
///
/// UNSAFE: dereferences the client pointer the app passed to `glVertexAttribPointer` (an address in GUEST
/// memory). This is valid ONLY here, at draw-record time, in the guest process where that pointer lives —
/// the deferred model cannot defer the read to swap (the client memory may be reused by then).
unsafe fn read_client_attr(base: usize, size: i32, kind: u32, stride: i32, vert_end: usize) -> Vec<u8> {
    let comp = size.clamp(1, 4) as usize;
    let elem = comp * crate::model::glconst::gl_component_size(kind);
    let st = if stride > 0 { stride as usize } else { elem };
    let mut out = Vec::with_capacity(vert_end * elem);
    for v in 0..vert_end {
        let src = (base + v * st) as *const u8;
        out.extend_from_slice(std::slice::from_raw_parts(src, elem));
    }
    out
}

/// Capture every enabled attribute drawn with NO vertex buffer bound (`buffer == 0`, a client pointer) into
/// the draw's `client_vbufs`, each spanning `[0, vert_end)`. A null client pointer (`offset == 0`) is
/// skipped (nothing to read). An all-VBO draw records nothing here, so it lowers unchanged.
fn capture_client_vbufs(d: &mut DrawCall, vert_end: usize) {
    if vert_end == 0 {
        return;
    }
    let attrs = d.attrs; // `[Attr; MAX_ATTR]` is Copy — snapshot before borrowing `d` mutably below.
    for (i, a) in attrs.iter().enumerate() {
        if !a.enabled || a.buffer != 0 || a.offset == 0 {
            continue;
        }
        let data = unsafe { read_client_attr(a.offset, a.size, a.kind, a.stride, vert_end) };
        d.client_vbufs.push(crate::model::program::ClientArray {
            location: i,
            data,
            size: a.size,
            kind: a.kind,
            normalized: a.normalized,
            integer: a.integer,
            divisor: a.divisor,
        });
    }
}

/// Read `count` indices of type `index_type` from `bytes` (little-endian) at index position `k`.
fn index_at(bytes: &[u8], index_type: u32, k: usize) -> usize {
    match index_type {
        GL_UNSIGNED_BYTE => bytes.get(k).copied().unwrap_or(0) as usize,
        GL_UNSIGNED_SHORT => {
            let o = k * 2;
            u16::from_le_bytes([bytes.get(o).copied().unwrap_or(0), bytes.get(o + 1).copied().unwrap_or(0)]) as usize
        }
        _ => {
            let o = k * 4;
            u32::from_le_bytes([
                bytes.get(o).copied().unwrap_or(0),
                bytes.get(o + 1).copied().unwrap_or(0),
                bytes.get(o + 2).copied().unwrap_or(0),
                bytes.get(o + 3).copied().unwrap_or(0),
            ]) as usize
        }
    }
}

/// The indexed-draw capture: handle client-side vertex arrays and/or a client-side index array for a
/// `glDrawElements*`. Covers all four combinations (pure-VBO returns immediately, leaving the draw
/// unchanged):
/// * client index array (no element buffer bound) → read the client index bytes, promote `u8`→`u16`
///   (the index IR has no `u8` format), store them in `client_indices`, and rewrite `index_type`.
/// * client vertex arrays → scan the index range for the max index and capture each client array over
///   `[0, maxIndex + baseVertex + 1)` (the exact vertex span the fetch touches).
///
/// The index source is the client pointer when no element-array-buffer is bound, else the bound buffer's
/// bytes at `offset` (so a client-vertex + VBO-index mix still finds the right vertex span).
fn capture_indexed(ctx: &GlContext, d: &mut DrawCall, count: i32, index_type: u32, offset: usize, base_vertex: i32) {
    let has_client_vbuf = d.attrs.iter().any(|a| a.enabled && a.buffer == 0 && a.offset != 0);
    let client_index = d.elem_buf == 0;
    if !has_client_vbuf && !client_index {
        return; // pure-VBO indexed draw — the existing VBO path handles everything.
    }
    let isz = crate::model::glconst::gl_component_size(index_type);
    let need = count.max(0) as usize * isz;
    // The raw index bytes: from the client pointer, or from the bound element-array-buffer at `offset`.
    let idx_bytes: Vec<u8> = if client_index {
        if offset == 0 || count <= 0 {
            return;
        }
        unsafe { std::slice::from_raw_parts(offset as *const u8, need).to_vec() }
    } else {
        match ctx.buffers.get(d.elem_buf).and_then(|b| b.data.get(offset..(offset + need).min(b.data.len()))) {
            Some(s) => s.to_vec(),
            None => return,
        }
    };
    // A client index array is promoted (u8→u16) into the final index-buffer encoding + type rewrite.
    if client_index {
        if index_type == GL_UNSIGNED_BYTE {
            let mut promoted = Vec::with_capacity(count as usize * 2);
            for k in 0..count as usize {
                promoted.extend_from_slice(&(index_at(&idx_bytes, index_type, k) as u16).to_le_bytes());
            }
            d.client_indices = promoted;
            d.index_type = GL_UNSIGNED_SHORT;
        } else {
            d.client_indices = idx_bytes.clone();
        }
    }
    // Client vertex arrays span [0, maxIndex + baseVertex + 1) — the widest vertex the fetch reaches.
    if has_client_vbuf {
        let mut max_idx = 0usize;
        for k in 0..count.max(0) as usize {
            let i = index_at(&idx_bytes, index_type, k);
            if i > max_idx {
                max_idx = i;
            }
        }
        let vert_end = max_idx + base_vertex.max(0) as usize + 1;
        capture_client_vbufs(d, vert_end);
    }
}

/// The scissor-clipped sample footprint of one draw — its viewport rectangle intersected with the scissor
/// box (when `GL_SCISSOR_TEST` is enabled), times the instance count. Used to feed an open occlusion query
/// so `GL_ANY_SAMPLES_PASSED` reflects reality: a visible draw contributes a positive area, a draw whose
/// scissor excludes its viewport (or a zero-area scissor) contributes `0`. This is an UPPER BOUND on the
/// true rasterized sample count (it does not clip to the primitive itself), which is exactly what the
/// boolean `GL_ANY_SAMPLES_PASSED` needs — nonzero iff the draw could rasterize any sample.
fn draw_coverage(d: &crate::model::program::DrawCall) -> u64 {
    let [vx, vy, vw, vh] = d.viewport;
    let (mut x0, mut y0, mut x1, mut y1) = (vx, vy, vx.saturating_add(vw), vy.saturating_add(vh));
    if d.scissor_enabled {
        let [sx, sy, sw, sh] = d.scissor;
        x0 = x0.max(sx);
        y0 = y0.max(sy);
        x1 = x1.min(sx.saturating_add(sw));
        y1 = y1.min(sy.saturating_add(sh));
    }
    let w = (x1 - x0).max(0) as u64;
    let h = (y1 - y0).max(0) as u64;
    w * h * d.instance_count.max(1) as u64
}

/// Feed `d`'s coverage into an open occlusion query (no-op when none is armed). Called for every recorded
/// geometry draw (never for a `glClear` — clears do not affect occlusion queries).
fn accumulate_occlusion(ctx: &mut GlContext, d: &crate::model::program::DrawCall) {
    ctx.queries.accumulate(draw_coverage(d));
}

/// `glDrawArrays(mode, first, count)` — snapshot the bound state and append the draw (one instance).
pub fn draw_arrays(ctx: &mut GlContext, mode: u32, first: i32, count: i32) {
    draw_arrays_instanced(ctx, mode, first, count, 1);
}

/// `glDrawArraysInstanced(mode, first, count, instances)` — like [`draw_arrays`] with an explicit
/// instance count, recorded onto the draw so the frame builder lowers a `Draw { instance_count }`. A
/// negative instance count raises `GL_INVALID_VALUE` (first-error-wins) and records nothing; a zero
/// count (or vertex count) is a legal no-op.
pub fn draw_arrays_instanced(ctx: &mut GlContext, mode: u32, first: i32, count: i32, instances: i32) {
    if instances < 0 || count < 0 {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    if count == 0 || instances == 0 {
        return;
    }
    let mut d = snapshot(ctx);
    d.mode = mode;
    d.first = first;
    d.count = count;
    d.instance_count = instances as u32;
    // Client-side vertex arrays span [0, first+count): the transient buffer is indexed by `first_vertex`.
    capture_client_vbufs(&mut d, first.max(0) as usize + count as usize);
    accumulate_occlusion(ctx, &d);
    ctx.draws.push(d);
}

/// `glDrawElements(mode, count, index_type, offset)` — snapshot + append an indexed draw (one instance).
pub fn draw_elements(ctx: &mut GlContext, mode: u32, count: i32, index_type: u32, offset: usize) {
    draw_elements_instanced(ctx, mode, count, index_type, offset, 1);
}

/// `glDrawElementsInstanced(mode, count, index_type, offset, instances)` — like [`draw_elements`] with
/// an explicit instance count, lowered to a `DrawIndexed { instance_count }`. A negative instance count
/// raises `GL_INVALID_VALUE` and records nothing.
pub fn draw_elements_instanced(
    ctx: &mut GlContext,
    mode: u32,
    count: i32,
    index_type: u32,
    offset: usize,
    instances: i32,
) {
    if instances < 0 || count < 0 {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    if count == 0 || instances == 0 {
        return;
    }
    let mut d = snapshot(ctx);
    d.mode = mode;
    d.count = count;
    d.indexed = true;
    d.index_type = index_type;
    d.index_offset = offset;
    d.instance_count = instances as u32;
    d.elem_buf = ctx.element_buffer;
    capture_indexed(ctx, &mut d, count, index_type, offset, 0);
    accumulate_occlusion(ctx, &d);
    ctx.draws.push(d);
}

/// `glDrawElementsBaseVertex(mode, count, type, offset, basevertex)` — an indexed draw whose fetched
/// index values are all offset by `basevertex` before the vertex fetch. Recorded onto the draw's
/// `base_vertex` (the frame builder folds it into the vertex fetch).
pub fn draw_elements_base_vertex(ctx: &mut GlContext, mode: u32, count: i32, index_type: u32, offset: usize, base_vertex: i32) {
    draw_elements_instanced_base_vertex(ctx, mode, count, index_type, offset, 1, base_vertex);
}

/// `glDrawElementsInstancedBaseVertex(...)` — the instanced + base-vertex indexed draw.
pub fn draw_elements_instanced_base_vertex(
    ctx: &mut GlContext,
    mode: u32,
    count: i32,
    index_type: u32,
    offset: usize,
    instances: i32,
    base_vertex: i32,
) {
    if instances < 0 || count < 0 {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    if count == 0 || instances == 0 {
        return;
    }
    let mut d = snapshot(ctx);
    d.mode = mode;
    d.count = count;
    d.indexed = true;
    d.index_type = index_type;
    d.index_offset = offset;
    d.instance_count = instances as u32;
    d.base_vertex = base_vertex;
    d.elem_buf = ctx.element_buffer;
    capture_indexed(ctx, &mut d, count, index_type, offset, base_vertex);
    accumulate_occlusion(ctx, &d);
    ctx.draws.push(d);
}

/// `glDrawRangeElements(mode, start, end, count, type, offset)` — a bounded indexed draw. `[start, end]`
/// is a driver hint about the referenced index range (used for buffer-residency optimization); it does not
/// change what is drawn, so this records the same indexed draw as `glDrawElements`. `end < start` →
/// `GL_INVALID_VALUE`.
pub fn draw_range_elements(ctx: &mut GlContext, mode: u32, start: u32, end: u32, count: i32, index_type: u32, offset: usize, base_vertex: i32) {
    if end < start {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    draw_elements_base_vertex(ctx, mode, count, index_type, offset, base_vertex);
}

/// Read a `u32` at byte `off` from the buffer bound to `target` (little-endian), or `None` out of range.
fn read_u32_at(ctx: &GlContext, target: u32, off: usize) -> Option<u32> {
    let name = ctx.buffer_for_target(target);
    let b = ctx.buffers.get(name)?;
    let bytes = b.data.get(off..off + 4)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// `glDrawArraysIndirect(mode, indirect)` — an array draw whose `{count, instanceCount, first,
/// baseInstance}` are read from the buffer bound to `GL_DRAW_INDIRECT_BUFFER` at byte offset `indirect`.
/// This model reads the CPU-side indirect struct and records the equivalent instanced array draw. A
/// missing / too-short indirect buffer is an honest no-op (nothing drawn).
pub fn draw_arrays_indirect(ctx: &mut GlContext, mode: u32, indirect: usize) {
    let count = match read_u32_at(ctx, GL_DRAW_INDIRECT_BUFFER, indirect) {
        Some(v) => v as i32,
        None => return,
    };
    let instances = read_u32_at(ctx, GL_DRAW_INDIRECT_BUFFER, indirect + 4).unwrap_or(1) as i32;
    let first = read_u32_at(ctx, GL_DRAW_INDIRECT_BUFFER, indirect + 8).unwrap_or(0) as i32;
    draw_arrays_instanced(ctx, mode, first, count, instances);
}

/// `glDrawElementsIndirect(mode, type, indirect)` — the indexed indirect draw; reads `{count,
/// instanceCount, firstIndex, baseVertex, baseInstance}` from the bound indirect buffer.
pub fn draw_elements_indirect(ctx: &mut GlContext, mode: u32, index_type: u32, indirect: usize) {
    let count = match read_u32_at(ctx, GL_DRAW_INDIRECT_BUFFER, indirect) {
        Some(v) => v as i32,
        None => return,
    };
    let instances = read_u32_at(ctx, GL_DRAW_INDIRECT_BUFFER, indirect + 4).unwrap_or(1) as i32;
    let first_index = read_u32_at(ctx, GL_DRAW_INDIRECT_BUFFER, indirect + 8).unwrap_or(0) as usize;
    let base_vertex = read_u32_at(ctx, GL_DRAW_INDIRECT_BUFFER, indirect + 12).unwrap_or(0) as i32;
    // The element offset is a byte offset; firstIndex is in index units. Convert by the index size.
    let index_size = match index_type {
        GL_UNSIGNED_BYTE => 1,
        GL_UNSIGNED_SHORT => 2,
        _ => 4,
    };
    draw_elements_instanced_base_vertex(ctx, mode, count, index_type, first_index * index_size, instances, base_vertex);
}

// ---- clear-buffer (glClearBuffer*) ---------------------------------------------------------------

/// `glClearBufferfv(GL_COLOR, drawbuffer, value)` — clear the current render target to `rgba`. Recorded as
/// a full-surface clear at the given color (the same deferred clear `glClear` records), so the frame
/// builder lowers a pass clear-load with this color.
pub fn clear_buffer_color(ctx: &mut GlContext, rgba: [f32; 4]) {
    ctx.clear_color = rgba;
    clear(ctx);
}

// ---- blend equation (glBlendEquation*) -----------------------------------------------------------

/// `glBlendEquation(mode)` — set the same blend equation for RGB and alpha.
pub fn blend_equation(ctx: &mut GlContext, mode: u32) {
    ctx.blend_eq_rgb = mode;
    ctx.blend_eq_alpha = mode;
}

/// `glBlendEquationSeparate(modeRGB, modeAlpha)`.
pub fn blend_equation_separate(ctx: &mut GlContext, rgb: u32, alpha: u32) {
    ctx.blend_eq_rgb = rgb;
    ctx.blend_eq_alpha = alpha;
}

// ---- program-uniform DSA setters (glProgramUniform*) ---------------------------------------------

/// `glProgramUniform*` for a data uniform — write `bytes` into `program`'s uniform-block buffer at the
/// member at declaration index `location` (the DSA form of [`uniform_at`], targeting a named program
/// rather than the bound one). Out-of-range writes are truncated to the member's slot.
pub fn program_uniform_at(ctx: &mut GlContext, program: u32, location: i32, bytes: &[u8]) {
    if location < 0 {
        return;
    }
    if let Some(p) = ctx.programs.program_mut(program) {
        let (off, sz) = match p.unis.get(location as usize) {
            Some(u) => (u.off as usize, u.sz as usize),
            None => return,
        };
        if off >= p.ubuf.len() {
            return;
        }
        let n = bytes.len().min(sz).min(p.ubuf.len() - off);
        p.ubuf[off..off + n].copy_from_slice(&bytes[..n]);
    }
}

/// `glProgramUniform1i(program, samplerLocation, unit)` — map `program`'s sampler uniform (declaration
/// index) to a texture unit (the DSA form of [`uniform_sampler`]).
pub fn program_uniform_sampler(ctx: &mut GlContext, program: u32, sampler_index: usize, unit: i32) {
    if let Some(p) = ctx.programs.program_mut(program) {
        if sampler_index < p.samp_units.len() {
            p.samp_units[sampler_index] = unit;
        }
    }
}

// ---- program / shader lifecycle (glDeleteProgram / glDeleteShader / glDetachShader) ---------------

/// `glDeleteProgram(program)` — drop the program object; clears the current-program binding if it names
/// the deleted program.
pub fn delete_program(ctx: &mut GlContext, program: u32) {
    if ctx.programs.delete_program(program) && ctx.cur_prog == program {
        ctx.cur_prog = 0;
    }
}

/// `glDeleteShader(shader)` — drop the shader object (its source + compile state).
pub fn delete_shader(ctx: &mut GlContext, shader: u32) {
    ctx.programs.delete_shader(shader);
}

/// `glDetachShader(program, shader)` — clear the matching attachment slot. Honest GL errors: an unknown
/// program or shader → `GL_INVALID_VALUE`; a shader not attached to the program → `GL_INVALID_OPERATION`.
pub fn detach_shader(ctx: &mut GlContext, program: u32, shader: u32) {
    if !ctx.programs.program_exists(program) || !ctx.programs.shader_exists(shader) {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    if !ctx.programs.detach(program, shader) {
        ctx.set_gl_error(GL_INVALID_OPERATION);
    }
}

/// Snapshot the currently-bound draw state into a fresh [`DrawCall`] (the immutable per-draw record).
fn snapshot(ctx: &GlContext) -> DrawCall {
    let mut d = DrawCall {
        prog: ctx.cur_prog,
        fbo: ctx.bound_fbo,
        attrs: ctx.attr,
        tex_units: ctx.tex_unit,
        viewport: ctx.viewport,
        scissor_enabled: ctx.scissor_enabled,
        scissor: ctx.scissor,
        blend: ctx.blend,
        blend_src_rgb: ctx.blend_src_rgb,
        blend_dst_rgb: ctx.blend_dst_rgb,
        blend_src_alpha: ctx.blend_src_alpha,
        blend_dst_alpha: ctx.blend_dst_alpha,
        blend_eq_rgb: ctx.blend_eq_rgb,
        blend_eq_alpha: ctx.blend_eq_alpha,
        depth: ctx.depth,
        depth_func: ctx.depth_func,
        depth_write: ctx.depth_write,
        cull_enabled: ctx.cull_enabled,
        cull_face: ctx.cull_face,
        front_face: ctx.front_face,
        clear: ctx.clear_color,
        elem_buf: ctx.element_buffer,
        ..DrawCall::default()
    };
    if let Some(p) = ctx.programs.program(ctx.cur_prog) {
        d.samp_units = p.samp_units;
        // Snapshot the default-block `glUniform*` bytes for THIS draw: `Program::ubuf` is mutable state,
        // so a later draw that changes a uniform must not retroactively alter this draw's bytes.
        let sz = p.ubuf_size.max(0) as usize;
        if sz > 0 {
            d.ubuf_bytes = p.ubuf[..sz.min(p.ubuf.len())].to_vec();
        }
    }
    d.ubo_bytes = resolve_block_ubo_bytes(ctx, ctx.cur_prog);
    d
}

/// Resolve the app's uniform-BLOCK bytes for `prog_name` at draw time — the std140 data the shader's
/// `layout(std140, binding = 0) uniform … { … }` block reads. The chain is:
/// `glBindBufferBase(GL_UNIFORM_BUFFER, blockBinding, buffer)` bound a buffer to the block's binding point,
/// and `glBufferData`/`glBufferSubData` filled it. We locate the block's binding point, then the indexed
/// UBO binding at that point, then that buffer's bytes.
///
/// Binding-point priority: the shader's explicit `layout(binding = N)` qualifier (GskGpu/GTK4 declares
/// `binding = 0` in-shader and binds via `glBindBufferBase`), else an app-assigned `glUniformBlockBinding`
/// value, else `0`. Returns EMPTY when the program has no data uniforms, declares no block, or has no UBO
/// bound at the resolved point (the default-uniform `glUniform*` path — the caller then keeps `Program::ubuf`).
fn resolve_block_ubo_bytes(ctx: &GlContext, prog_name: u32) -> Vec<u8> {
    let prog = match ctx.programs.program(prog_name) {
        Some(p) if p.has_uniforms() => p,
        _ => return Vec::new(),
    };
    // MULTI-BLOCK program: the shader declares 2+ uniform blocks, each at its OWN binding point fed by its
    // OWN `glBindBufferRange`d range. The translator flattens every block's members into ONE `HlUniforms`
    // std140 block at IR binding 0 (declaration order — see `adapter::glsl::translate_render`), so the
    // recorded binding-0 bytes are assembled block-by-block: each block contributes its own bound range's
    // std140 bytes, 16-byte aligned to the next block (matching std140 for the vec4/mat-member blocks
    // GskGpu-style programs use). This proves each `glBindBufferRange` fed the right binding.
    let blocks = crate::adapter::glsl::uniform_blocks(&prog.vs_src, &prog.fs_src);
    if blocks.len() >= 2 {
        return assemble_multi_block_ubo_bytes(ctx, &blocks);
    }
    // The block's binding point (see priority above).
    let bp = crate::adapter::glsl::uniform_block_binding_qualifier(&prog.vs_src)
        .or_else(|| crate::adapter::glsl::uniform_block_binding_qualifier(&prog.fs_src))
        .or_else(|| ctx.uniform_blocks.get(&prog_name).and_then(|blocks| blocks.first()).map(|b| b.binding))
        .unwrap_or(0);
    if std::env::var("HL_UBO_DUMP").is_ok() {
        let keys: Vec<_> = ctx.indexed_buffers.keys().collect();
        eprintln!("[UBO_DUMP] prog={prog_name} has_uniforms=true ubuf_size={} bp={bp} indexed_keys={keys:?}", prog.ubuf_size);
    }
    let ib = match ctx.indexed_buffers.get(&(GL_UNIFORM_BUFFER, bp)) {
        Some(ib) => *ib,
        None => return Vec::new(),
    };
    if std::env::var("HL_UBO_DUMP").is_ok() {
        let sz = ctx.buffers.get(ib.buffer).map(|b| b.data.len()).unwrap_or(0);
        let head: Vec<u8> = ctx.buffers.get(ib.buffer).map(|b| b.data.iter().take(16).copied().collect()).unwrap_or_default();
        eprintln!("[UBO_DUMP]   ib buffer={} off={} size={} bufbytes={sz} head={head:?}", ib.buffer, ib.offset, ib.size);
    }
    let buf = match ctx.buffers.get(ib.buffer) {
        Some(b) => b,
        None => return Vec::new(),
    };
    let off = ib.offset.max(0) as usize;
    if off >= buf.data.len() {
        return Vec::new();
    }
    // `size == 0` (from `glBindBufferBase`) means the whole buffer from `offset`.
    let end = if ib.size <= 0 { buf.data.len() } else { (off + ib.size as usize).min(buf.data.len()) };
    buf.data[off..end].to_vec()
}

/// Assemble the flattened `HlUniforms` binding-0 bytes for a MULTI-block program from each block's own
/// `glBindBufferRange`d range, in `blocks` (declaration) order. Each block appends its bound range's std140
/// bytes, then pads to the next 16-byte boundary so the following block starts 16-aligned (std140 for a
/// vec4/mat4-member block). A block with no bound range contributes a zero-filled std140 span (an honest
/// hole, not a fake). This is what routes two ranges to two distinct binding points through the single
/// flattened block the translator emits.
fn assemble_multi_block_ubo_bytes(ctx: &GlContext, blocks: &[crate::adapter::glsl::UniformBlockDecl]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    for blk in blocks {
        let bytes = ctx
            .indexed_buffers
            .get(&(GL_UNIFORM_BUFFER, blk.binding))
            .and_then(|ib| {
                let buf = ctx.buffers.get(ib.buffer)?;
                let off = ib.offset.max(0) as usize;
                if off > buf.data.len() {
                    return Some(Vec::new());
                }
                let end = if ib.size <= 0 { buf.data.len() } else { (off + ib.size as usize).min(buf.data.len()) };
                Some(buf.data[off..end].to_vec())
            })
            .unwrap_or_default();
        out.extend_from_slice(&bytes);
        // Pad this block's contribution up to the next 16-byte std140 boundary (each block is 16-aligned).
        while out.len() % 16 != 0 {
            out.push(0);
        }
    }
    out
}
