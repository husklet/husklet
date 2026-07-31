//! Shader / program / object introspection — the reflective half of the `gl*Get*` surface.
//!
//! `glGetShaderiv`, `glGetProgramiv`, the buffer/texture parameter reads, and the uniform/attribute
//! reflection (`glGetUniformLocation`, `glGetActiveUniform`, …) resolve against the linked program's
//! reflected tables in [`crate::model::program`]. Pure state inspection: nothing here submits.

use crate::model::context::GlContext;
use crate::model::glconst::*;

// ---- glGetShaderiv / glGetProgramiv --------------------------------------------------------------

/// `glGetShaderiv(shader, pname)` — the shader object's compile status + metadata. `GL_COMPILE_STATUS`
/// is `GL_TRUE` for a compiled shader (the model's translator accepts the GLSL-ES the render path backs);
/// `GL_INFO_LOG_LENGTH` is `0` (no diagnostics); `GL_SHADER_SOURCE_LENGTH` is `strlen(source)+1`.
/// An unknown shader name (or pname) reports `0`.
pub fn get_shaderiv(ctx: &GlContext, shader: u32, pname: u32) -> i32 {
    let sh = match ctx.programs.shader(shader) {
        Some(s) => s,
        None => return 0,
    };
    match pname {
        GL_COMPILE_STATUS => {
            if sh.compiled {
                GL_TRUE as i32
            } else {
                GL_FALSE as i32
            }
        }
        GL_INFO_LOG_LENGTH => {
            let log = ctx.programs.shader_info_log(shader);
            if log.is_empty() {
                0
            } else {
                log.len() as i32 + 1
            }
        }
        GL_SHADER_SOURCE_LENGTH => sh.src.as_ref().map(|s| s.len() as i32 + 1).unwrap_or(0),
        GL_SHADER_TYPE => sh.kind as i32,
        // ES 3.0 §7.1: GL_TRUE once glDeleteShader has flagged this shader.
        GL_DELETE_STATUS => i32::from(sh.pending_delete),
        _ => 0,
    }
}

/// The actionable diagnostic retained by the most recent refused shader compile.
pub fn shader_info_log(ctx: &GlContext, shader: u32) -> &str {
    ctx.programs.shader_info_log(shader)
}

/// The actionable diagnostic retained by the most recent failed program link.
pub fn program_info_log(ctx: &GlContext, program: u32) -> &str {
    ctx.programs
        .program(program)
        .map(|program| program.link_error.as_str())
        .unwrap_or("")
}

/// `glGetProgramiv(program, pname)` — the program object's link status + reflected counts.
/// `GL_LINK_STATUS`/`GL_VALIDATE_STATUS` are `GL_TRUE` once linked; `GL_INFO_LOG_LENGTH` is `0`;
/// `GL_ATTACHED_SHADERS`, `GL_ACTIVE_UNIFORMS`, and `GL_ACTIVE_ATTRIBUTES` come from the reflected
/// tables. An unknown program name (or pname) reports `0`.
pub fn get_programiv(ctx: &GlContext, program: u32, pname: u32) -> i32 {
    let p = match ctx.programs.program(program) {
        Some(p) => p,
        None => return 0,
    };
    match pname {
        GL_LINK_STATUS | GL_VALIDATE_STATUS => {
            if p.linked {
                GL_TRUE as i32
            } else {
                GL_FALSE as i32
            }
        }
        GL_INFO_LOG_LENGTH => {
            if p.link_error.is_empty() {
                0
            } else {
                p.link_error.len() as i32 + 1
            }
        }
        // ES 3.0 §7.3: GL_TRUE once glDeleteProgram has flagged this program.
        GL_DELETE_STATUS => i32::from(p.pending_delete),
        GL_ATTACHED_SHADERS => (p.vs != 0) as i32 + (p.fs != 0) as i32,
        GL_ACTIVE_UNIFORMS => (p.unis.len() + p.samp_names.len()) as i32,
        GL_ACTIVE_UNIFORM_MAX_LENGTH => {
            crate::adapter::glsl::StageSources::new(&p.vs_src, &p.fs_src)
                .uniform_decls()
                .into_iter()
                .chain(
                    crate::adapter::glsl::StageSources::new(&p.vs_src, &p.fs_src).sampler_decls(),
                )
                .map(|decl| {
                    decl.name.len() as i32 + if decl.arr > 0 { "[0]".len() as i32 } else { 0 } + 1
                })
                .max()
                .unwrap_or(0)
        }
        GL_ACTIVE_ATTRIBUTES => crate::adapter::glsl::Source::new(&p.vs_src)
            .vertex_attrs()
            .len() as i32,
        GL_ACTIVE_ATTRIBUTE_MAX_LENGTH => crate::adapter::glsl::Source::new(&p.vs_src)
            .vertex_attrs()
            .iter()
            .map(|decl| decl.name.len() as i32 + 1)
            .max()
            .unwrap_or(0),
        _ => 0,
    }
}

// ---- glGetBufferParameteriv / glGetTexParameteriv -------------------------------------------------

/// `glGetBufferParameteriv(target, pname)` — real size/usage of the buffer bound to `target`
/// (`gl_shim.c` parity). `GL_BUFFER_SIZE` is the byte length; `GL_BUFFER_USAGE` the stored usage hint;
/// an unknown pname / unbound buffer reads `0`.
pub fn get_buffer_parameteriv(ctx: &GlContext, target: u32, pname: u32) -> i32 {
    let name = ctx.buffer_for_target(target);
    let Some(b) = ctx.buffers.get(name) else {
        return 0;
    };
    match pname {
        GL_BUFFER_SIZE => b.data.len() as i32,
        GL_BUFFER_USAGE => b.usage as i32,
        _ => 0,
    }
}

/// `glGetTexParameteriv(target, pname)` — scalar texture state of the bound texture.
/// An unknown pname / no bound texture reads `0`.
pub fn get_tex_parameteriv(ctx: &GlContext, target: u32, pname: u32) -> i32 {
    if target != GL_TEXTURE_2D {
        return 0;
    }
    let name = ctx.local.tex_unit[ctx.local.active_texture];
    let Some(t) = ctx.textures.get(name) else {
        return 0;
    };
    match pname {
        GL_TEXTURE_MIN_FILTER => t.min_filter as i32,
        GL_TEXTURE_MAG_FILTER => t.mag_filter as i32,
        GL_TEXTURE_WRAP_S => t.wrap_s as i32,
        GL_TEXTURE_WRAP_T => t.wrap_t as i32,
        GL_TEXTURE_SWIZZLE_R => t.swizzle[0] as i32,
        GL_TEXTURE_SWIZZLE_G => t.swizzle[1] as i32,
        GL_TEXTURE_SWIZZLE_B => t.swizzle[2] as i32,
        GL_TEXTURE_SWIZZLE_A => t.swizzle[3] as i32,
        GL_TEXTURE_BASE_LEVEL => t.base_level,
        GL_TEXTURE_MAX_LEVEL => t.max_level,
        _ => 0,
    }
}

// ---- glGetUniformLocation / glGetAttribLocation --------------------------------------------------

/// `glGetUniformLocation(program, name)` — resolve `name` against the linked program's reflected uniform
/// tables (see [`crate::model::program::Program::uniform_location`]). `-1` for an unknown name / program.
pub fn uniform_location(ctx: &GlContext, program: u32, name: &str) -> i32 {
    ctx.programs
        .program(program)
        .map(|p| p.uniform_location(name))
        .unwrap_or(-1)
}

/// `glGetAttribLocation(program, name)` — the attribute's declaration-order slot in the vertex shader
/// (see [`crate::model::program::Program::attrib_location`]). `-1` for an unknown name / program.
pub fn attrib_location(ctx: &GlContext, program: u32, name: &str) -> i32 {
    ctx.programs
        .program(program)
        .map(|p| p.attrib_location(name))
        .unwrap_or(-1)
}

// ---- glGetActiveUniform / glGetActiveAttrib ------------------------------------------------------

/// One active program variable's reflection, as `glGetActiveUniform`/`glGetActiveAttrib` report it.
pub struct ActiveVar {
    /// The declared variable name.
    pub name: String,
    /// The GL type enum (`GL_FLOAT`, `GL_FLOAT_VEC3`, `GL_FLOAT_MAT4`, `GL_SAMPLER_2D`, …).
    pub gl_type: u32,
    /// The array length in GLSL elements (`1` for a scalar/vector/matrix — this model does not reflect
    /// uniform/attribute arrays).
    pub size: i32,
}

/// Map a GLSL-ES type keyword to the GL type enum `glGetActiveUniform`/`glGetActiveAttrib` report. An
/// unrecognized type falls back to `GL_FLOAT` (the safest scalar an app is likely to accept).
pub struct GlType(u32);

impl From<&str> for GlType {
    fn from(ty: &str) -> Self {
        Self(match ty {
            "float" => GL_FLOAT,
            "vec2" => GL_FLOAT_VEC2,
            "vec3" => GL_FLOAT_VEC3,
            "vec4" => GL_FLOAT_VEC4,
            "int" => GL_INT,
            "ivec2" => GL_INT_VEC2,
            "ivec3" => GL_INT_VEC3,
            "ivec4" => GL_INT_VEC4,
            "uint" => GL_UNSIGNED_INT,
            "uvec2" => GL_UNSIGNED_INT_VEC2,
            "uvec3" => GL_UNSIGNED_INT_VEC3,
            "uvec4" => GL_UNSIGNED_INT_VEC4,
            "bool" => GL_BOOL,
            "mat2" | "mat2x2" => GL_FLOAT_MAT2,
            "mat3" | "mat3x3" => GL_FLOAT_MAT3,
            "mat4" | "mat4x4" => GL_FLOAT_MAT4,
            "samplerCube" => GL_SAMPLER_CUBE,
            "sampler2D" | "sampler2DShadow" => GL_SAMPLER_2D,
            _ => GL_FLOAT,
        })
    }
}

impl From<GlType> for u32 {
    fn from(value: GlType) -> Self {
        value.0
    }
}

pub const GL_TYPE_ENUM: fn(&str) -> u32 = |ty| GlType::from(ty).into();
pub use GL_TYPE_ENUM as gl_type_enum;

/// `glGetActiveUniform(program, index)` — the reflection of the `index`-th active uniform. The uniforms
/// are enumerated data-uniforms-first then samplers, matching both `glGetProgramiv(GL_ACTIVE_UNIFORMS)`
/// (the count) and the location convention of `glGetUniformLocation` (so `index` and location agree).
/// `None` for an unknown program / unlinked program / out-of-range index.
pub fn active_uniform(ctx: &GlContext, program: u32, index: u32) -> Option<ActiveVar> {
    let p = ctx.programs.program(program)?;
    if !p.linked {
        return None;
    }
    let data = crate::adapter::glsl::StageSources::new(&p.vs_src, &p.fs_src).uniform_decls();
    let i = index as usize;
    if let Some(d) = data.get(i) {
        return Some(ActiveVar {
            name: active_name(d),
            gl_type: gl_type_enum(&d.ty),
            size: d.arr.max(1) as i32,
        });
    }
    let samps = crate::adapter::glsl::StageSources::new(&p.vs_src, &p.fs_src).sampler_decls();
    samps.get(i - data.len()).map(|d| ActiveVar {
        name: active_name(d),
        gl_type: gl_type_enum(&d.ty),
        size: d.arr.max(1) as i32,
    })
}

fn active_name(declaration: &crate::adapter::glsl::Decl) -> String {
    if declaration.arr > 0 {
        format!("{}[0]", declaration.name)
    } else {
        declaration.name.clone()
    }
}

/// `glGetActiveAttrib(program, index)` — the reflection of the `index`-th active vertex attribute, in
/// the declaration order `glGetAttribLocation` resolves against. `None` for an unknown / unlinked
/// program or an out-of-range index.
pub fn active_attrib(ctx: &GlContext, program: u32, index: u32) -> Option<ActiveVar> {
    let p = ctx.programs.program(program)?;
    if !p.linked {
        return None;
    }
    crate::adapter::glsl::Source::new(&p.vs_src)
        .vertex_attrs()
        .get(index as usize)
        .map(|d| ActiveVar {
            name: d.name.clone(),
            gl_type: gl_type_enum(&d.ty),
            size: 1,
        })
}
