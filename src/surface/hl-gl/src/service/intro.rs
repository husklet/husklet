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

#[path = "intro_resource.rs"]
mod resource;

pub use resource::*;

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
    let crate::model::program::UniformLocation::Data {
        declaration,
        element,
    } = p.location(location)?
    else {
        return None;
    };
    p.unis.get(declaration)?.read_element(&p.ubuf, element)
}

#[derive(Clone, Copy)]
enum UniformScalar {
    Float,
    Int,
    UInt,
    Bool,
}

fn uniform_value(ctx: &GlContext, program: u32, location: i32) -> Option<(UniformScalar, Vec<u8>)> {
    if location < 0 {
        return None;
    }
    let p = ctx.programs.program(program)?;
    if !p.linked {
        return None;
    }
    match p.location(location)? {
        crate::model::program::UniformLocation::Data {
            declaration,
            element,
        } => {
            let uniform = p.unis.get(declaration)?;
            let scalar = if uniform.ty == "bool" || uniform.ty.starts_with("bvec") {
                UniformScalar::Bool
            } else if uniform.ty == "int" || uniform.ty.starts_with("ivec") {
                UniformScalar::Int
            } else if uniform.ty == "uint" || uniform.ty.starts_with("uvec") {
                UniformScalar::UInt
            } else {
                UniformScalar::Float
            };
            Some((scalar, uniform.read_element(&p.ubuf, element)?))
        }
        crate::model::program::UniformLocation::Sampler { element } => Some((
            UniformScalar::Int,
            p.samp_units.get(element)?.to_le_bytes().to_vec(),
        )),
    }
}

fn uniform_words(ctx: &GlContext, program: u32, location: i32) -> Option<(UniformScalar, Vec<[u8; 4]>)> {
    let (scalar, bytes) = uniform_value(ctx, program, location)?;
    let words = bytes
        .chunks_exact(4)
        .map(|word| [word[0], word[1], word[2], word[3]])
        .collect();
    Some((scalar, words))
}

/// `glGetUniformfv` conversion of the current uniform value. GL getters convert from the uniform's
/// declared scalar class; they do not reinterpret the std140 storage bits as the requested result type.
pub fn get_uniform_f32(ctx: &GlContext, program: u32, location: i32) -> Option<Vec<f32>> {
    let (scalar, words) = uniform_words(ctx, program, location)?;
    Some(
        words
            .into_iter()
            .map(|word| match scalar {
                UniformScalar::Float => f32::from_le_bytes(word),
                UniformScalar::Int => i32::from_le_bytes(word) as f32,
                UniformScalar::UInt => u32::from_le_bytes(word) as f32,
                UniformScalar::Bool => (u32::from_le_bytes(word) != 0) as u8 as f32,
            })
            .collect(),
    )
}

/// `glGetUniformiv` conversion of the current uniform value.
pub fn get_uniform_i32(ctx: &GlContext, program: u32, location: i32) -> Option<Vec<i32>> {
    let (scalar, words) = uniform_words(ctx, program, location)?;
    Some(
        words
            .into_iter()
            .map(|word| match scalar {
                UniformScalar::Float => f32::from_le_bytes(word) as i32,
                UniformScalar::Int => i32::from_le_bytes(word),
                UniformScalar::UInt => u32::from_le_bytes(word) as i32,
                UniformScalar::Bool => (u32::from_le_bytes(word) != 0) as i32,
            })
            .collect(),
    )
}

/// `glGetUniformuiv` conversion of the current uniform value.
pub fn get_uniform_u32(ctx: &GlContext, program: u32, location: i32) -> Option<Vec<u32>> {
    let (scalar, words) = uniform_words(ctx, program, location)?;
    Some(
        words
            .into_iter()
            .map(|word| match scalar {
                UniformScalar::Float => f32::from_le_bytes(word) as u32,
                UniformScalar::Int => i32::from_le_bytes(word) as u32,
                UniformScalar::UInt => u32::from_le_bytes(word),
                UniformScalar::Bool => (u32::from_le_bytes(word) != 0) as u32,
            })
            .collect(),
    )
}

/// The texture unit a sampler uniform (declaration index `location`) is bound to (`glUniform1i` writes
/// it), or `None` if `location` is not a sampler of `program`. Lets `glGetUniformiv` on a sampler report
/// its bound unit truthfully.
pub fn get_sampler_unit(ctx: &GlContext, program: u32, location: i32) -> Option<i32> {
    if location < 0 {
        return None;
    }
    let p = ctx.programs.program(program)?;
    let crate::model::program::UniformLocation::Sampler { element } = p.location(location)? else {
        return None;
    };
    p.samp_units.get(element).copied()
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
    if let Some(i) = data.iter().position(|d| uniform_name_matches(d, name)) {
        return i as u32;
    }
    let samps = glsl::StageSources::new(&p.vs_src, &p.fs_src).sampler_decls();
    if let Some(i) = samps.iter().position(|d| uniform_name_matches(d, name)) {
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
            GL_UNIFORM_SIZE => d.arr.max(1) as i32,
            GL_UNIFORM_NAME_LENGTH => active_uniform_name_len(d),
            GL_UNIFORM_BLOCK_INDEX => 0,
            GL_UNIFORM_OFFSET => off,
            GL_UNIFORM_ARRAY_STRIDE => {
                if d.arr > 0 {
                    glsl::std140_array_stride(&d.ty)
                } else {
                    0
                }
            }
            // Reported as zero for every type, matrices included. An application laying out a std140
            // block reads this to step from one column to the next, so a zero stacks every column at the
            // same address and it writes its own uniform data on top of itself — a wrong value the
            // application acts on, not merely a wrong description. The block really is std140 here (see
            // the layout rule this is derived from), so the answer for a matrix is its column stride.
            GL_UNIFORM_MATRIX_STRIDE => glsl::std140_matrix_stride(&d.ty),
            GL_UNIFORM_IS_ROW_MAJOR => 0,
            _ => 0,
        });
    }
    let samps = glsl::StageSources::new(&p.vs_src, &p.fs_src).sampler_decls();
    let d = samps.get(i - data.len())?;
    // A sampler uniform: not backed by a buffer block (offset/block-index are the "default block" -1).
    Some(match pname {
        GL_UNIFORM_TYPE => gl_type_enum(&d.ty) as i32,
        GL_UNIFORM_SIZE => d.arr.max(1) as i32,
        GL_UNIFORM_NAME_LENGTH => active_uniform_name_len(d),
        GL_UNIFORM_BLOCK_INDEX => -1,
        GL_UNIFORM_OFFSET => -1,
        GL_UNIFORM_ARRAY_STRIDE => -1,
        GL_UNIFORM_MATRIX_STRIDE => -1,
        GL_UNIFORM_IS_ROW_MAJOR => 0,
        _ => 0,
    })
}

fn uniform_name_matches(declaration: &glsl::Decl, requested: &str) -> bool {
    declaration.name == requested
        || (declaration.arr > 0
            && requested
                .strip_suffix("[0]")
                .is_some_and(|base| base == declaration.name))
}

fn active_uniform_name_len(declaration: &glsl::Decl) -> i32 {
    declaration.name.len() as i32 + if declaration.arr > 0 { 3 } else { 0 } + 1
}

#[cfg(test)]
mod uniform_reflection_tests {
    use super::*;

    #[test]
    fn array_leaf_accepts_only_base_and_zero_element_names() {
        let declaration = glsl::Decl {
            ty: "int".into(),
            name: "u_var.m2".into(),
            arr: 3,
            array_literal: true,
        };

        assert!(uniform_name_matches(&declaration, "u_var.m2"));
        assert!(uniform_name_matches(&declaration, "u_var.m2[0]"));
        assert!(!uniform_name_matches(&declaration, "u_var.m2[1]"));
        assert!(!uniform_name_matches(&declaration, "u_var.m20[0]"));
        assert_eq!(active_uniform_name_len(&declaration), 12);
    }

    #[test]
    fn scalar_leaf_does_not_accept_an_array_suffix() {
        let declaration = glsl::Decl {
            ty: "int".into(),
            name: "u_var.m0".into(),
            arr: 0,
            array_literal: false,
        };

        assert!(uniform_name_matches(&declaration, "u_var.m0"));
        assert!(!uniform_name_matches(&declaration, "u_var.m0[0]"));
        assert_eq!(active_uniform_name_len(&declaration), 9);
    }
}

// ==================================================================================================
// named uniform blocks (glGetUniformBlockIndex / glUniformBlockBinding / glGetActiveUniformBlock*)
// ==================================================================================================

/// `glGetUniformBlockIndex(program, name)` — the index of the named uniform block, or `GL_INVALID_INDEX`
/// for an unknown program or a name the program does not declare as a block.
///
/// The table is built at link from the blocks the shader declares (see `record::programs`), so an index is
/// a position in declaration order and nothing else. It used to be assigned lazily on first lookup, on top
/// of a synthetic block called `Uniforms` seeded at index 0 to stand for the DEFAULT uniform block — which
/// is not a named block at all (ES 3.0 §2.12.6: no index, excluded from `GL_ACTIVE_UNIFORM_BLOCKS`, its
/// members reporting a block index of -1, which [`uniformsiv`] already returns correctly). A non-block
/// occupying the first slot shifted every real block by one, and lazy assignment then handed out an index
/// for any string at all, so a name the program does not declare came back valid.
pub fn uniform_block_index(ctx: &mut GlContext, program: u32, name: &str) -> u32 {
    if !ctx.programs.contains(program) {
        return GL_INVALID_INDEX;
    }
    ctx.uniform_blocks
        .get(&program)
        .and_then(|blocks| blocks.iter().position(|b| b.name == name))
        .map_or(GL_INVALID_INDEX, |pos| pos as u32)
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
    // Every block answers from its OWN declaration. Index 0 used to be special-cased to report the
    // program's whole flattened uniform buffer, because index 0 was the synthetic default block — so the
    // real blocks were left reporting a size and a count of zero, and an application laying its buffer out
    // at the driver's own figures was told the block needs no space at all.
    let b = ctx
        .uniform_blocks
        .get(&program)
        .and_then(|b| b.get(block_index as usize))?;
    let (binding, name_len) = (b.binding as i32, b.name.len() as i32 + 1);
    let (data_size, active) = (b.data_size, b.members);
    Some(match pname {
        GL_UNIFORM_BLOCK_BINDING => binding,
        GL_UNIFORM_BLOCK_DATA_SIZE => data_size,
        GL_UNIFORM_BLOCK_ACTIVE_UNIFORMS => active,
        GL_UNIFORM_BLOCK_NAME_LENGTH => name_len,
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
    pub fn is_enabled(&mut self, cap: u32) -> bool {
        match cap {
            GL_DEPTH_TEST => self.local.pipeline.depth,
            GL_STENCIL_TEST => self.local.pipeline.stencil,
            GL_BLEND => self.local.pipeline.blend,
            GL_DITHER => self.local.pipeline.dither,
            GL_POLYGON_OFFSET_FILL => self.local.pipeline.polygon_offset_fill,
            GL_SAMPLE_ALPHA_TO_COVERAGE => self.local.pipeline.sample_alpha_to_coverage,
            GL_SAMPLE_COVERAGE => self.local.pipeline.sample_coverage,
            GL_CULL_FACE => self.local.pipeline.cull_enabled,
            GL_SCISSOR_TEST => self.local.pipeline.scissor_enabled,
            GL_RASTERIZER_DISCARD => self.local.pipeline.rasterizer_discard,
            GL_PRIMITIVE_RESTART_FIXED_INDEX => true,
            GL_DEBUG_OUTPUT => self.local.pipeline.debug_output,
            GL_DEBUG_OUTPUT_SYNCHRONOUS => self.local.pipeline.debug_output_synchronous,
            _ => {
                self.set_gl_error(GL_INVALID_ENUM);
                false
            }
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
    if level != 0
        || !matches!(
            target,
            GL_TEXTURE_2D
                | GL_TEXTURE_2D_ARRAY
                | GL_TEXTURE_3D
                | GL_TEXTURE_CUBE_MAP_POSITIVE_X
                | GL_TEXTURE_CUBE_MAP_NEGATIVE_X
                | GL_TEXTURE_CUBE_MAP_POSITIVE_Y
                | GL_TEXTURE_CUBE_MAP_NEGATIVE_Y
                | GL_TEXTURE_CUBE_MAP_POSITIVE_Z
                | GL_TEXTURE_CUBE_MAP_NEGATIVE_Z
        )
    {
        return 0;
    }
    let name = ctx.bound_texture_for_target(target);
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

fn renderbuffer_component_bits(format: u32) -> [i32; 6] {
    match format {
        GL_R8 | GL_R8I | GL_R8UI => [8, 0, 0, 0, 0, 0],
        GL_R16I | GL_R16UI => [16, 0, 0, 0, 0, 0],
        GL_R32I | GL_R32UI => [32, 0, 0, 0, 0, 0],
        GL_RG8 | GL_RG8I | GL_RG8UI => [8, 8, 0, 0, 0, 0],
        GL_RG16I | GL_RG16UI => [16, 16, 0, 0, 0, 0],
        GL_RG32I | GL_RG32UI => [32, 32, 0, 0, 0, 0],
        GL_RGB8 => [8, 8, 8, 0, 0, 0],
        GL_RGB565 => [5, 6, 5, 0, 0, 0],
        GL_RGBA4 => [4, 4, 4, 4, 0, 0],
        GL_RGB5_A1 => [5, 5, 5, 1, 0, 0],
        GL_RGB10_A2 | GL_RGB10_A2UI => [10, 10, 10, 2, 0, 0],
        GL_RGBA8 | GL_SRGB8_ALPHA8 | GL_RGBA8I | GL_RGBA8UI => [8, 8, 8, 8, 0, 0],
        GL_RGBA16I | GL_RGBA16UI => [16, 16, 16, 16, 0, 0],
        GL_RGBA32I | GL_RGBA32UI => [32, 32, 32, 32, 0, 0],
        GL_DEPTH_COMPONENT16 => [0, 0, 0, 0, 16, 0],
        GL_DEPTH_COMPONENT24 => [0, 0, 0, 0, 24, 0],
        GL_DEPTH_COMPONENT32F => [0, 0, 0, 0, 32, 0],
        GL_DEPTH24_STENCIL8 => [0, 0, 0, 0, 24, 8],
        GL_DEPTH32F_STENCIL8 => [0, 0, 0, 0, 32, 8],
        GL_STENCIL_INDEX8 => [0, 0, 0, 0, 0, 8],
        _ => [0; 6],
    }
}

/// `glGetRenderbufferParameteriv(target, pname)` — properties of the bound renderbuffer storage.
/// A bad target / no bound RBO reads `0`.
pub fn renderbuffer_parameter(ctx: &GlContext, target: u32, pname: u32) -> i32 {
    if target != GL_RENDERBUFFER || ctx.local.bound_rbo == 0 {
        return 0;
    }
    let Some(renderbuffer) = ctx.renderbuffers.get(ctx.local.bound_rbo) else {
        return 0;
    };
    let bits = renderbuffer_component_bits(renderbuffer.internal_format);
    match pname {
        GL_RENDERBUFFER_WIDTH => renderbuffer.width,
        GL_RENDERBUFFER_HEIGHT => renderbuffer.height,
        GL_RENDERBUFFER_INTERNAL_FORMAT => renderbuffer.internal_format as i32,
        GL_RENDERBUFFER_RED_SIZE => bits[0],
        GL_RENDERBUFFER_GREEN_SIZE => bits[1],
        GL_RENDERBUFFER_BLUE_SIZE => bits[2],
        GL_RENDERBUFFER_ALPHA_SIZE => bits[3],
        GL_RENDERBUFFER_DEPTH_SIZE => bits[4],
        GL_RENDERBUFFER_STENCIL_SIZE => bits[5],
        GL_RENDERBUFFER_SAMPLES => renderbuffer.samples,
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
        GL_READ_FRAMEBUFFER => ctx.local.read_fbo,
        _ => ctx.local.bound_fbo,
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
    // What the application attached, not what it resolved to: a renderbuffer is backed by a texture here,
    // so reading the colour table alone reported `GL_TEXTURE` and the backing texture's name for a
    // renderbuffer attachment — exactly the distinction this query exists to make (ES 3.0 §6.1.13).
    let source = ctx.local.framebuffers.color_source(fbo, 0);
    match pname {
        GL_FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE => match source {
            Some((_, true)) => GL_RENDERBUFFER as i32,
            Some((_, false)) => GL_TEXTURE as i32,
            None => GL_NONE as i32,
        },
        GL_FRAMEBUFFER_ATTACHMENT_OBJECT_NAME => source.map(|(name, _)| name).unwrap_or(0) as i32,
        _ => 0,
    }
}
