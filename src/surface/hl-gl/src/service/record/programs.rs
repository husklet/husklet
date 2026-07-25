use super::*;

// ---- shaders + programs --------------------------------------------------------------------------

/// `glCreateShader(kind)`.
impl GlContext {
    pub fn create_shader(&mut self, kind: u32) -> u32 {
        self.programs.create_shader(kind)
    }
}

/// `glShaderSource(shader, src)`.
pub fn shader_source(ctx: &mut GlContext, shader: u32, src: &str) {
    ctx.programs.shader_source(shader, src);
}

/// `glCompileShader(shader)`.
impl GlContext {
    pub fn compile_shader(&mut self, shader: u32) {
        self.programs.compile_shader(shader);
    }
}

/// `glCreateProgram()`.
impl GlContext {
    pub fn create_program(&mut self) -> u32 {
        self.programs.create()
    }
}

/// `glAttachShader(program, shader)`.
pub fn attach_shader(ctx: &mut GlContext, program: u32, shader: u32) {
    ctx.programs.attach(program, shader);
}

/// `glLinkProgram(program)` — translate the attached GLSL-ES pair to shader-IR + reflect the layout.
impl GlContext {
    pub fn link_program(&mut self, program: u32) -> bool {
        self.programs.link(program)
    }

    /// `glUseProgram(program)`.
    pub fn use_program(&mut self, program: u32) {
        self.cur_prog = program;
    }
}

pub const CREATE_SHADER: fn(&mut GlContext, u32) -> u32 = GlContext::create_shader;
pub const COMPILE_SHADER: fn(&mut GlContext, u32) = GlContext::compile_shader;
pub const CREATE_PROGRAM: fn(&mut GlContext) -> u32 = GlContext::create_program;
pub const LINK_PROGRAM: fn(&mut GlContext, u32) -> bool = GlContext::link_program;
pub const USE_PROGRAM: fn(&mut GlContext, u32) = GlContext::use_program;
pub use COMPILE_SHADER as compile_shader;
pub use CREATE_PROGRAM as create_program;
pub use CREATE_SHADER as create_shader;
pub use LINK_PROGRAM as link_program;
pub use USE_PROGRAM as use_program;

/// `glUniform1i(samplerLocation, unit)` — map a sampler uniform (by declaration index) to a texture
/// unit. Simplified: `sampler_index` is the sampler's position in the program's `samp_names`.
pub fn uniform_sampler(ctx: &mut GlContext, sampler_index: usize, unit: i32) {
    if let Some(p) = ctx.programs.get_mut(ctx.cur_prog) {
        if sampler_index < p.samp_units.len() {
            p.samp_units[sampler_index] = unit;
        }
    }
}

/// `glUniform*` for a data uniform — write `bytes` into the bound program's uniform-block buffer at the
/// named member's offset. Simplified name-keyed write (real GL uses integer locations).
pub fn uniform_data(ctx: &mut GlContext, name: &str, bytes: &[u8]) {
    if let Some(p) = ctx.programs.get_mut(ctx.cur_prog) {
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
    if let Some(p) = ctx.programs.get_mut(ctx.cur_prog) {
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

// ---- program-uniform DSA setters (glProgramUniform*) ---------------------------------------------

/// `glProgramUniform*` for a data uniform — write `bytes` into `program`'s uniform-block buffer at the
/// member at declaration index `location` (the DSA form of [`uniform_at`], targeting a named program
/// rather than the bound one). Out-of-range writes are truncated to the member's slot.
pub fn program_uniform_at(ctx: &mut GlContext, program: u32, location: i32, bytes: &[u8]) {
    if location < 0 {
        return;
    }
    if let Some(p) = ctx.programs.get_mut(program) {
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
    if let Some(p) = ctx.programs.get_mut(program) {
        if sampler_index < p.samp_units.len() {
            p.samp_units[sampler_index] = unit;
        }
    }
}

// ---- program / shader lifecycle (glDeleteProgram / glDeleteShader / glDetachShader) ---------------

/// `glDeleteProgram(program)` — drop the program object; clears the current-program binding if it names
/// the deleted program.
impl GlContext {
    pub fn delete_program(&mut self, program: u32) {
        if self.programs.delete(program) {
            // Retire the program's resident IR shader modules + render pipelines (queued Destroy for the next
            // frame), so a deleted Skia/GskGpu program stops holding host residency and a recycled GL program
            // name cannot collide with the dead program's cached ids. See `GlContext::retire_program`.
            self.retire_program(program);
            if self.cur_prog == program {
                self.cur_prog = 0;
            }
        }
    }

    /// `glDeleteShader(shader)` — drop the shader object (its source + compile state).
    pub fn delete_shader(&mut self, shader: u32) {
        self.programs.delete_shader(shader);
    }
}

pub const DELETE_PROGRAM: fn(&mut GlContext, u32) = GlContext::delete_program;
pub const DELETE_SHADER: fn(&mut GlContext, u32) = GlContext::delete_shader;
pub use DELETE_PROGRAM as delete_program;
pub use DELETE_SHADER as delete_shader;

/// `glDetachShader(program, shader)` — clear the matching attachment slot. Honest GL errors: an unknown
/// program or shader → `GL_INVALID_VALUE`; a shader not attached to the program → `GL_INVALID_OPERATION`.
pub fn detach_shader(ctx: &mut GlContext, program: u32, shader: u32) {
    if !ctx.programs.contains(program) || !ctx.programs.shader_exists(shader) {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    if !ctx.programs.detach(program, shader) {
        ctx.set_gl_error(GL_INVALID_OPERATION);
    }
}

/// Snapshot the currently-bound draw state into a fresh [`DrawCall`] (the immutable per-draw record).
impl GlContext {
    pub(super) fn snapshot(&self) -> DrawCall {
        let ctx = self;
        // Capture the ES3 sampler OBJECT bound to each texture unit: a bound object overrides the texture's own
        // filter/wrap at lowering time (ES 3.0 §3.8.13). `None` where no object is bound (texture params win).
        let mut samp_objs: [Option<crate::model::es3::SamplerObj>; 8] = [None; 8];
        for (unit, slot) in samp_objs.iter_mut().enumerate() {
            let name = ctx.samplers.binding(unit as u32);
            if name != 0 {
                *slot = ctx.samplers.get(name).copied();
            }
        }
        let mut d = DrawCall {
            prog: ctx.cur_prog,
            fbo: ctx.bound_fbo,
            attrs: ctx.attr,
            tex_units: ctx.tex_unit,
            samp_objs,
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
            blend_color: ctx.blend_color,
            depth: ctx.depth,
            depth_func: ctx.depth_func,
            depth_write: ctx.depth_write,
            stencil: ctx.stencil,
            stencil_func_front: ctx.stencil_func_front,
            stencil_func_back: ctx.stencil_func_back,
            stencil_fail_front: ctx.stencil_fail_front,
            stencil_zfail_front: ctx.stencil_zfail_front,
            stencil_zpass_front: ctx.stencil_zpass_front,
            stencil_fail_back: ctx.stencil_fail_back,
            stencil_zfail_back: ctx.stencil_zfail_back,
            stencil_zpass_back: ctx.stencil_zpass_back,
            stencil_ref: ctx.stencil_ref,
            stencil_read_mask: ctx.stencil_read_mask,
            stencil_write_mask: ctx.stencil_write_mask,
            cull_enabled: ctx.cull_enabled,
            cull_face: ctx.cull_face,
            front_face: ctx.front_face,
            color_mask: ctx.color_mask,
            clear: ctx.clear_color,
            elem_buf: ctx.element_buffer,
            ..DrawCall::default()
        };
        d.target = if ctx.bound_fbo == 0 {
            None
        } else {
            let texture = ctx.framebuffers.color_attachment(ctx.bound_fbo);
            ctx.textures
                .get(texture)
                .filter(|t| t.w > 0 && t.h > 0)
                .map(|t| crate::model::program::TargetSnapshot {
                    texture,
                    generation: t.gen,
                    width: t.w,
                    height: t.h,
                    format: t.ir_format,
                })
        };
        for unit in 0..d.tex_units.len() {
            d.tex_generations[unit] = ctx
                .textures
                .get(d.tex_units[unit])
                .map(|t| t.gen)
                .unwrap_or(0);
        }
        if let Some(p) = ctx.programs.program(ctx.cur_prog) {
            d.samp_units = p.samp_units;
            // Snapshot the default-block `glUniform*` bytes for THIS draw: `Program::ubuf` is mutable state,
            // so a later draw that changes a uniform must not retroactively alter this draw's bytes.
            let sz = p.ubuf_size.max(0) as usize;
            if sz > 0 {
                d.ubuf_bytes = p.ubuf[..sz.min(p.ubuf.len())].to_vec();
            }
        }
        d.ubo_bytes = self.resolve_block_ubo_bytes(ctx.cur_prog);
        let mut names: Vec<u32> = d
            .attrs
            .iter()
            .filter(|attr| attr.enabled && attr.buffer != 0)
            .map(|attr| attr.buffer)
            .collect();
        if d.elem_buf != 0 {
            names.push(d.elem_buf);
        }
        names.sort_unstable();
        names.dedup();
        d.buffers = names
            .into_iter()
            .filter_map(|name| {
                ctx.buffers
                    .get(name)
                    .map(|buffer| crate::model::program::BufferSnapshot {
                        name,
                        generation: buffer.gen,
                        data: buffer.data.clone(),
                    })
            })
            .collect();
        d
    }
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
impl GlContext {
    pub(super) fn resolve_block_ubo_bytes(&self, prog_name: u32) -> Vec<u8> {
        let ctx = self;
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
        let blocks =
            crate::adapter::glsl::StageSources::new(&prog.vs_src, &prog.fs_src).uniform_blocks();
        if blocks.len() >= 2 {
            return self.assemble_multi_block_ubo_bytes(&blocks);
        }
        // The block's binding point (see priority above).
        let bp = crate::adapter::glsl::Source::new(&prog.vs_src)
            .uniform_block_binding()
            .or_else(|| crate::adapter::glsl::Source::new(&prog.fs_src).uniform_block_binding())
            .or_else(|| {
                ctx.uniform_blocks
                    .get(&prog_name)
                    .and_then(|blocks| blocks.first())
                    .map(|b| b.binding)
            })
            .unwrap_or(0);
        hl_log::hl_debug!(
            hl_log::tag::GL,
            "[UBO_DUMP] prog={prog_name} has_uniforms=true ubuf_size={} bp={bp} indexed_keys={:?}",
            prog.ubuf_size,
            ctx.indexed_buffers.keys().collect::<Vec<_>>()
        );
        let ib = match ctx.indexed_buffers.get(&(GL_UNIFORM_BUFFER, bp)) {
            Some(ib) => *ib,
            None => return Vec::new(),
        };
        hl_log::hl_debug!(
            hl_log::tag::GL,
            "[UBO_DUMP] ib buffer={} off={} size={} bufbytes={} head={:?}",
            ib.buffer,
            ib.offset,
            ib.size,
            ctx.buffers
                .get(ib.buffer)
                .map(|buffer| buffer.data.len())
                .unwrap_or(0),
            ctx.buffers
                .get(ib.buffer)
                .map(|buffer| buffer.data.iter().take(16).copied().collect::<Vec<_>>())
                .unwrap_or_default()
        );
        let buf = match ctx.buffers.get(ib.buffer) {
            Some(b) => b,
            None => return Vec::new(),
        };
        let off = ib.offset.max(0) as usize;
        if off >= buf.data.len() {
            return Vec::new();
        }
        // `size == 0` (from `glBindBufferBase`) means the whole buffer from `offset`.
        let end = if ib.size <= 0 {
            buf.data.len()
        } else {
            (off + ib.size as usize).min(buf.data.len())
        };
        buf.data[off..end].to_vec()
    }

    /// Assemble the flattened `HlUniforms` binding-0 bytes for a MULTI-block program from each block's own
    /// `glBindBufferRange`d range, in `blocks` (declaration) order. Each block appends its bound range's std140
    /// bytes, then pads to the next 16-byte boundary so the following block starts 16-aligned (std140 for a
    /// vec4/mat4-member block). A block with no bound range contributes a zero-filled std140 span (an honest
    /// hole, not a fake). This is what routes two ranges to two distinct binding points through the single
    /// flattened block the translator emits.
    pub(super) fn assemble_multi_block_ubo_bytes(
        &self,
        blocks: &[crate::adapter::glsl::UniformBlockDecl],
    ) -> Vec<u8> {
        let ctx = self;
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
                    let end = if ib.size <= 0 {
                        buf.data.len()
                    } else {
                        (off + ib.size as usize).min(buf.data.len())
                    };
                    Some(buf.data[off..end].to_vec())
                })
                .unwrap_or_default();
            out.extend_from_slice(&bytes);
            // Pad this block's contribution up to the next 16-byte std140 boundary (each block is 16-aligned).
            while !out.len().is_multiple_of(16) {
                out.push(0);
            }
        }
        out
    }
}
