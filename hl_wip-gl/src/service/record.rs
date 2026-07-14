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

/// `glBindBuffer(target, name)`.
pub fn bind_buffer(ctx: &mut GlContext, target: u32, name: u32) {
    match target {
        GL_ARRAY_BUFFER => ctx.array_buffer = name,
        GL_ELEMENT_ARRAY_BUFFER => ctx.element_buffer = name,
        _ => {}
    }
}

/// `glBufferData(target, data, usage)` — fills the buffer currently bound to `target`.
pub fn buffer_data(ctx: &mut GlContext, target: u32, data: &[u8], usage: u32) {
    let name = bound_buffer(ctx, target);
    if name != 0 {
        ctx.buffers.set_data(name, target, data, usage);
    }
}

/// `glBufferSubData(target, offset, data)`.
pub fn buffer_sub_data(ctx: &mut GlContext, target: u32, offset: usize, data: &[u8]) {
    let name = bound_buffer(ctx, target);
    if name != 0 {
        ctx.buffers.set_sub_data(name, offset, data);
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
    match target {
        GL_ELEMENT_ARRAY_BUFFER => ctx.element_buffer,
        _ => ctx.array_buffer,
    }
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

/// `glBindFramebuffer(target, name)` — bind `name` as the current draw framebuffer (`0` = default).
pub fn bind_framebuffer(ctx: &mut GlContext, _target: u32, name: u32) {
    ctx.bound_fbo = name;
}

/// `glFramebufferTexture2D(GL_COLOR_ATTACHMENT0, tex)` — attach `tex` as the bound FBO's color target.
/// Attaches to whichever FBO is currently bound (the default framebuffer `0` has no attachable slot).
pub fn framebuffer_texture_2d(ctx: &mut GlContext, tex: u32) {
    let fbo = ctx.bound_fbo;
    ctx.framebuffers.attach_color(fbo, tex);
}

/// `glDeleteFramebuffers` (one name).
pub fn delete_framebuffer(ctx: &mut GlContext, name: u32) -> bool {
    if ctx.bound_fbo == name {
        ctx.bound_fbo = 0;
    }
    ctx.framebuffers.delete(name)
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
    ctx.draws.push(d);
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
    }
    d
}
