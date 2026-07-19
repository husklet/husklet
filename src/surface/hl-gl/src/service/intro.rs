//! ES3 program-introspection + uniform-reflection services (the read half of the "new API surface" pass).
//!
//! Every function here resolves a `gl*` introspection query against the modeled [`GlContext`] and its
//! reflected [`crate::model::program`] tables (the same reflection [`crate::service::query`] serves
//! `glGetActiveUniform`/`glGetUniformLocation` from) — so the UBO-block / uniform-index / program-resource
//! queries report REAL values keyed on the program's declared uniforms/attributes/outputs, not fabricated
//! defaults. Pure state inspection: `&`/`&mut GlContext` in, values out, submits nothing. The C-ABI entry
//! points in the shim marshal the raw pointers; the honest values live here so they are unit-testable.

use crate::adapter::glsl;
use crate::model::context::{GlContext, UniformBlock};
use crate::model::glconst::*;
use crate::service::query::gl_type_enum;

// ==================================================================================================
// uniform readback (glGetUniform* / glGetnUniform*)
// ==================================================================================================

/// The little-endian bytes of the data uniform at declaration index `location` in `program`'s current
/// uniform-block buffer (`ubuf`) — the value a `glUniform*`/`glProgramUniform*` last wrote. `None` for an
/// unknown/unlinked program, a negative location, or a location that is not a data uniform (e.g. a
/// sampler, whose value is a texture-unit index read separately).
pub fn get_uniform_bytes(ctx: &GlContext, program: u32, location: i32) -> Option<Vec<u8>> {
    if location < 0 {
        return None;
    }
    let p = ctx.programs.program(program)?;
    if !p.linked {
        return None;
    }
    let u = p.unis.get(location as usize)?;
    let (off, sz) = (u.off as usize, u.sz as usize);
    if off + sz > p.ubuf.len() {
        return None;
    }
    Some(p.ubuf[off..off + sz].to_vec())
}

/// The texture unit a sampler uniform (declaration index `location`) is bound to (`glUniform1i` writes
/// it), or `None` if `location` is not a sampler of `program`. Lets `glGetUniformiv` on a sampler report
/// its bound unit truthfully.
pub fn get_sampler_unit(ctx: &GlContext, program: u32, location: i32) -> Option<i32> {
    if location < 0 {
        return None;
    }
    let p = ctx.programs.program(program)?;
    let i = location as usize;
    if i < p.samp_names.len() && i < p.samp_units.len() {
        Some(p.samp_units[i])
    } else {
        None
    }
}

// ==================================================================================================
// glGetUniformIndices / glGetActiveUniformsiv
// ==================================================================================================

/// `glGetUniformIndices` — resolve a uniform `name` to its active-uniform index (its declaration index in
/// the reflected table, data uniforms first then samplers — the exact index `glGetActiveUniform` reports).
/// Returns `GL_INVALID_INDEX` for an unknown name / program.
pub fn uniform_index(ctx: &GlContext, program: u32, name: &str) -> u32 {
    let Some(p) = ctx.programs.program(program) else {
        return GL_INVALID_INDEX;
    };
    if !p.linked {
        return GL_INVALID_INDEX;
    }
    let data = glsl::StageSources::new(&p.vs_src, &p.fs_src).uniform_decls();
    if let Some(i) = data.iter().position(|d| d.name == name) {
        return i as u32;
    }
    let samps = glsl::StageSources::new(&p.vs_src, &p.fs_src).sampler_decls();
    if let Some(i) = samps.iter().position(|d| d.name == name) {
        return (data.len() + i) as u32;
    }
    GL_INVALID_INDEX
}

/// `glGetActiveUniformsiv` — one reflected property of the active uniform at `index` (data-uniforms-first
/// enumeration). Reports the REAL reflected type/size/offset/name-length and block membership. `None` for
/// an out-of-range index / unknown program (the caller leaves the slot untouched).
pub fn active_uniformsiv(ctx: &GlContext, program: u32, index: u32, pname: u32) -> Option<i32> {
    let p = ctx.programs.program(program)?;
    if !p.linked {
        return None;
    }
    let data = glsl::StageSources::new(&p.vs_src, &p.fs_src).uniform_decls();
    let i = index as usize;
    if let Some(d) = data.get(i) {
        // A data uniform: it lives in the single implicit uniform block (index 0) at its layout offset.
        let off = p.unis.get(i).map(|u| u.off).unwrap_or(0);
        return Some(match pname {
            GL_UNIFORM_TYPE => gl_type_enum(&d.ty) as i32,
            GL_UNIFORM_SIZE => 1,
            GL_UNIFORM_NAME_LENGTH => d.name.len() as i32 + 1,
            GL_UNIFORM_BLOCK_INDEX => 0,
            GL_UNIFORM_OFFSET => off,
            GL_UNIFORM_ARRAY_STRIDE => 0,
            GL_UNIFORM_MATRIX_STRIDE => 0,
            GL_UNIFORM_IS_ROW_MAJOR => 0,
            _ => 0,
        });
    }
    let samps = glsl::StageSources::new(&p.vs_src, &p.fs_src).sampler_decls();
    let d = samps.get(i - data.len())?;
    // A sampler uniform: not backed by a buffer block (offset/block-index are the "default block" -1).
    Some(match pname {
        GL_UNIFORM_TYPE => gl_type_enum(&d.ty) as i32,
        GL_UNIFORM_SIZE => 1,
        GL_UNIFORM_NAME_LENGTH => d.name.len() as i32 + 1,
        GL_UNIFORM_BLOCK_INDEX => -1,
        GL_UNIFORM_OFFSET => -1,
        GL_UNIFORM_ARRAY_STRIDE => -1,
        GL_UNIFORM_MATRIX_STRIDE => -1,
        GL_UNIFORM_IS_ROW_MAJOR => 0,
        _ => 0,
    })
}

// ==================================================================================================
// named uniform blocks (glGetUniformBlockIndex / glUniformBlockBinding / glGetActiveUniformBlock*)
// ==================================================================================================

/// Ensure the program's block table has a canonical block 0 mirroring the reflected implicit block (the
/// one `glUniform*` writes into) when the program declares data uniforms. Idempotent.
impl GlContext {
    fn seed_blocks(&mut self, program: u32) {
        let has_uniforms = self
            .programs
            .program(program)
            .map(|p| p.has_uniforms())
            .unwrap_or(false);
        if !has_uniforms {
            return;
        }
        let blocks = self.uniform_blocks.entry(program).or_default();
        if blocks.is_empty() {
            // The single implicit block this model reflects. GLSL flattens the block name away at collect
            // time, so the canonical name is the MSL struct name the translator emits.
            blocks.push(UniformBlock {
                name: "Uniforms".to_string(),
                binding: 0,
            });
        }
    }
}

/// `glGetUniformBlockIndex(program, name)` — the index of the named uniform block, lazily assigning a
/// stable index the first time a name is seen (default binding 0), matching the reference shim. Returns
/// `GL_INVALID_INDEX` for an unknown program.
pub fn uniform_block_index(ctx: &mut GlContext, program: u32, name: &str) -> u32 {
    if !ctx.programs.contains(program) {
        return GL_INVALID_INDEX;
    }
    ctx.seed_blocks(program);
    let blocks = ctx.uniform_blocks.entry(program).or_default();
    if let Some(pos) = blocks.iter().position(|b| b.name == name) {
        return pos as u32;
    }
    let idx = blocks.len() as u32;
    blocks.push(UniformBlock {
        name: name.to_string(),
        binding: 0,
    });
    idx
}

/// `glUniformBlockBinding(program, blockIndex, binding)` — assign the block's binding point. Honest GL
/// errors: an unknown program or block index → `GL_INVALID_VALUE`; a binding beyond the cap →
/// `GL_INVALID_VALUE`.
pub fn uniform_block_binding(ctx: &mut GlContext, program: u32, block_index: u32, binding: u32) {
    if !ctx.programs.contains(program) {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    if binding >= MAX_UNIFORM_BUFFER_BINDINGS {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    ctx.seed_blocks(program);
    let blocks = ctx.uniform_blocks.entry(program).or_default();
    match blocks.get_mut(block_index as usize) {
        Some(b) => b.binding = binding,
        None => ctx.set_gl_error(GL_INVALID_VALUE),
    }
}

/// `glGetActiveUniformBlockName(program, blockIndex)` — the block's declared name, or `None` (out of range
/// → the caller raises `GL_INVALID_VALUE`).
pub fn active_uniform_block_name(
    ctx: &mut GlContext,
    program: u32,
    block_index: u32,
) -> Option<String> {
    ctx.seed_blocks(program);
    ctx.uniform_blocks
        .get(&program)
        .and_then(|b| b.get(block_index as usize))
        .map(|b| b.name.clone())
}

/// `glGetActiveUniformBlockiv(program, blockIndex, pname)` — the block's binding / data size / active
/// uniform count / name length. Block 0 reflects the program's real implicit block (`ubuf_size`, the
/// data-uniform count); a lazily-named block reports its binding + name only. `None` (→ `GL_INVALID_VALUE`)
/// for an out-of-range block.
pub fn active_uniform_blockiv(
    ctx: &mut GlContext,
    program: u32,
    block_index: u32,
    pname: u32,
) -> Option<i32> {
    ctx.seed_blocks(program);
    let (binding, name_len) = {
        let b = ctx
            .uniform_blocks
            .get(&program)
            .and_then(|b| b.get(block_index as usize))?;
        (b.binding as i32, b.name.len() as i32 + 1)
    };
    // Block 0 is the reflected implicit block; report its real size + active-uniform count.
    let (data_size, active) = if block_index == 0 {
        ctx.programs
            .program(program)
            .map(|p| (p.ubuf_size, p.unis.len() as i32))
            .unwrap_or((0, 0))
    } else {
        (0, 0)
    };
    Some(match pname {
        GL_UNIFORM_BLOCK_BINDING => binding,
        GL_UNIFORM_BLOCK_DATA_SIZE => data_size,
        GL_UNIFORM_BLOCK_ACTIVE_UNIFORMS => active,
        GL_UNIFORM_BLOCK_NAME_LENGTH => name_len,
        _ => 0,
    })
}

// ==================================================================================================
// program-resource introspection (glGetProgramInterfaceiv / glGetProgramResource*)
// ==================================================================================================

/// One introspected program resource (a uniform / input / output).
pub struct Resource {
    pub name: String,
    pub gl_type: u32,
    /// The GL location (`glGetProgramResourceLocation`) — a uniform's/attribute's declaration index, an
    /// output's fragment-data location. `-1` when the interface has no location namespace (uniform blocks).
    pub location: i32,
}

/// The active resources of a program `interface` in enumeration order (the order `glGetProgramResourceName`
/// indexes and `glGetProgramInterfaceiv(GL_ACTIVE_RESOURCES)` counts). Empty for an unknown/unlinked
/// program or an interface this model does not reflect.
pub fn interface_resources(ctx: &GlContext, program: u32, interface: u32) -> Vec<Resource> {
    let Some(p) = ctx.programs.program(program) else {
        return Vec::new();
    };
    if !p.linked {
        return Vec::new();
    }
    match interface {
        GL_UNIFORM => {
            let data = glsl::StageSources::new(&p.vs_src, &p.fs_src).uniform_decls();
            let samps = glsl::StageSources::new(&p.vs_src, &p.fs_src).sampler_decls();
            let n_data = data.len();
            data.into_iter()
                .chain(samps)
                .enumerate()
                .map(|(i, d)| Resource {
                    name: d.name.clone(),
                    gl_type: gl_type_enum(&d.ty),
                    // A sampler uniform has no default-block location in the value sense, but the resource
                    // location namespace mirrors `glGetUniformLocation` (declaration index within its kind).
                    location: if i < n_data {
                        i as i32
                    } else {
                        (i - n_data) as i32
                    },
                })
                .collect()
        }
        GL_PROGRAM_INPUT => glsl::Source::new(&p.vs_src)
            .vertex_attrs()
            .into_iter()
            .enumerate()
            .map(|(i, d)| Resource {
                name: d.name.clone(),
                gl_type: gl_type_enum(&d.ty),
                location: i as i32,
            })
            .collect(),
        GL_PROGRAM_OUTPUT => glsl::StageSources::new("", &p.fs_src)
            .frag_outputs()
            .into_iter()
            .enumerate()
            .map(|(i, d)| Resource {
                name: d.name.clone(),
                gl_type: gl_type_enum(&d.ty),
                location: i as i32,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// `glGetProgramInterfaceiv(program, interface, pname)` — the interface's active-resource count / the
/// longest resource name length + 1. `None` for a pname this model does not reflect.
pub fn program_interfaceiv(
    ctx: &GlContext,
    program: u32,
    interface: u32,
    pname: u32,
) -> Option<i32> {
    if interface == GL_UNIFORM_BLOCK {
        // Uniform blocks are reflected through the block table (block 0 = the implicit block).
        let n = if ctx
            .programs
            .program(program)
            .map(|p| p.has_uniforms())
            .unwrap_or(false)
        {
            1
        } else {
            0
        };
        return Some(match pname {
            GL_ACTIVE_RESOURCES => n,
            GL_MAX_NAME_LENGTH => {
                if n > 0 {
                    "Uniforms".len() as i32 + 1
                } else {
                    0
                }
            }
            _ => 0,
        });
    }
    let res = interface_resources(ctx, program, interface);
    Some(match pname {
        GL_ACTIVE_RESOURCES => res.len() as i32,
        GL_MAX_NAME_LENGTH => res
            .iter()
            .map(|r| r.name.len() as i32 + 1)
            .max()
            .unwrap_or(0),
        _ => 0,
    })
}

/// `glGetProgramResourceIndex(program, interface, name)` — the enumeration index of the named resource, or
/// `GL_INVALID_INDEX` if not found.
pub fn program_resource_index(ctx: &GlContext, program: u32, interface: u32, name: &str) -> u32 {
    interface_resources(ctx, program, interface)
        .iter()
        .position(|r| r.name == name)
        .map(|i| i as u32)
        .unwrap_or(GL_INVALID_INDEX)
}

/// `glGetProgramResourceLocation(program, interface, name)` — the GL location of the named uniform /
/// input / output, or `-1` if not found.
pub fn program_resource_location(ctx: &GlContext, program: u32, interface: u32, name: &str) -> i32 {
    interface_resources(ctx, program, interface)
        .iter()
        .find(|r| r.name == name)
        .map(|r| r.location)
        .unwrap_or(-1)
}

/// `glGetProgramResourceName(program, interface, index)` — the resource's declared name, or `None`.
pub fn program_resource_name(
    ctx: &GlContext,
    program: u32,
    interface: u32,
    index: u32,
) -> Option<String> {
    interface_resources(ctx, program, interface)
        .into_iter()
        .nth(index as usize)
        .map(|r| r.name)
}

/// `glGetProgramResourceiv(program, interface, index, prop)` — one queried property of the resource. `None`
/// for an out-of-range index (the caller writes nothing for that slot).
pub fn program_resourceiv(
    ctx: &GlContext,
    program: u32,
    interface: u32,
    index: u32,
    prop: u32,
) -> Option<i32> {
    let res = interface_resources(ctx, program, interface);
    let r = res.get(index as usize)?;
    Some(match prop {
        GL_TYPE => r.gl_type as i32,
        GL_ARRAY_SIZE => 1,
        GL_NAME_LENGTH => r.name.len() as i32 + 1,
        GL_LOCATION => r.location,
        GL_OFFSET => -1,
        GL_BLOCK_INDEX => -1,
        _ => 0,
    })
}

// ==================================================================================================
// misc reflection (frag-data location / enable state / shader source / level + attachment queries)
// ==================================================================================================

/// `glGetFragDataLocation(program, name)` — the fragment output's color-attachment index (its declaration
/// order), or `-1` for an unknown output / program. An ES2 `gl_FragColor` program declares no named output.
pub fn frag_data_location(ctx: &GlContext, program: u32, name: &str) -> i32 {
    let Some(p) = ctx.programs.program(program) else {
        return -1;
    };
    if !p.linked {
        return -1;
    }
    glsl::StageSources::new("", &p.fs_src)
        .frag_outputs()
        .iter()
        .position(|d| d.name == name)
        .map(|i| i as i32)
        .unwrap_or(-1)
}

/// `glIsEnabled(cap)` — the live enable state of a modeled fixed-function capability. An unmodeled cap
/// reads `false` (the honest answer for a capability this deferred model does not track).
impl GlContext {
    pub fn is_enabled(&self, cap: u32) -> bool {
        match cap {
            GL_DEPTH_TEST => self.depth,
            GL_STENCIL_TEST => self.stencil,
            GL_BLEND => self.blend,
            GL_CULL_FACE => self.cull_enabled,
            GL_SCISSOR_TEST => self.scissor_enabled,
            _ => false,
        }
    }
}

/// `glGetShaderSource(shader)` — the exact GLSL-ES source string last given to `glShaderSource` (empty for
/// a source-less / unknown shader).
impl GlContext {
    pub fn get_shader_source(&self, shader: u32) -> String {
        self.programs
            .shader(shader)
            .and_then(|s| s.src.clone())
            .unwrap_or_default()
    }
}

/// `glGetTexLevelParameter{i,f}v(target, level, pname)` — the bound texture's level-0 extent + internal
/// format. Only level 0 of a 2D-family target is modeled (a single mip); other levels / an unbound texture
/// read `0`.
pub fn tex_level_parameter(ctx: &GlContext, target: u32, level: i32, pname: u32) -> i32 {
    if level != 0 || !matches!(target, GL_TEXTURE_2D | GL_TEXTURE_2D_ARRAY | GL_TEXTURE_3D) {
        return 0;
    }
    let name = ctx.tex_unit[ctx.active_texture];
    let Some(t) = ctx.textures.get(name) else {
        return 0;
    };
    match pname {
        GL_TEXTURE_WIDTH => t.w,
        GL_TEXTURE_HEIGHT => t.h,
        GL_TEXTURE_INTERNAL_FORMAT => GL_RGBA8 as i32,
        _ => 0,
    }
}

/// `glGetRenderbufferParameteriv(target, pname)` — the bound renderbuffer's extent + internal format (this
/// model materializes every renderbuffer as an RGBA8 plane). A bad target / no bound RBO reads `0`.
pub fn renderbuffer_parameter(ctx: &GlContext, target: u32, pname: u32) -> i32 {
    if target != GL_RENDERBUFFER || ctx.bound_rbo == 0 {
        return 0;
    }
    let (w, h) = ctx.renderbuffers.dims(ctx.bound_rbo).unwrap_or((0, 0));
    match pname {
        GL_RENDERBUFFER_WIDTH => w,
        GL_RENDERBUFFER_HEIGHT => h,
        GL_RENDERBUFFER_INTERNAL_FORMAT => GL_RGBA8 as i32,
        _ => 0,
    }
}

/// `glGetFramebufferAttachmentParameteriv(target, attachment, pname)` — the object type + name of the bound
/// framebuffer's color attachment. The default framebuffer's color is the built-in back buffer; a user
/// FBO's is its attached texture (`GL_TEXTURE`).
pub fn framebuffer_attachment_parameter(
    ctx: &GlContext,
    target: u32,
    attachment: u32,
    pname: u32,
) -> i32 {
    let fbo = match target {
        GL_READ_FRAMEBUFFER => ctx.read_fbo,
        _ => ctx.bound_fbo,
    };
    if attachment != GL_COLOR_ATTACHMENT0 && !(fbo == 0 && attachment == GL_BACK) {
        // Only the color attachment is reflected; other attachments have no modeled object.
        return match pname {
            GL_FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE => GL_NONE as i32,
            _ => 0,
        };
    }
    if fbo == 0 {
        return match pname {
            GL_FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE => GL_FRAMEBUFFER_DEFAULT as i32,
            _ => 0,
        };
    }
    let tex = ctx.framebuffers.color_attachment(fbo);
    match pname {
        GL_FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE => {
            if tex != 0 {
                GL_TEXTURE as i32
            } else {
                GL_NONE as i32
            }
        }
        GL_FRAMEBUFFER_ATTACHMENT_OBJECT_NAME => tex as i32,
        _ => 0,
    }
}
