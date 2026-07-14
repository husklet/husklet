//! The `gl*` query / introspection ops — the read-only front half a real GLES app polls constantly.
//!
//! Every `gl*Get*` a GLES3 app calls during init and each frame (identity strings, capability limits,
//! bound-object state, shader/program compile+link status, and uniform/attribute reflection) resolves
//! here against the modeled [`GlContext`] and its reflected [`crate::model::program`] tables. Like
//! [`crate::service::record`] these submit NOTHING — a query is pure state inspection. The shim's C-ABI
//! entry points marshal the raw pointers; the honest values live here so they are unit-testable without
//! the guest cdylib. Ported from `hl-shim-gl/src/gles.rs` (`glGetString`/`glGetIntegerv`/`glGetShaderiv`/
//! `glGetUniformLocation`/…), keeping the same advertised values.

use crate::model::context::GlContext;
use crate::model::glconst::*;

// ---- identity strings (glGetString) --------------------------------------------------------------
//
// NUL-terminated so the shim hands the guest a `const GLubyte *` straight from the byte slice. The
// driver advertises a GLES **3.0** identity (the profile its real render path backs), consistent with
// the existing `hl-gl` guest identity (`GL_VENDOR = "hl-gl"`, the EGL vendor string).

pub const IDENT_VENDOR: &[u8] = b"hl-gl\0";
pub const IDENT_RENDERER: &[u8] = b"hl-gl-metal\0";
pub const IDENT_VERSION: &[u8] = b"OpenGL ES 3.0 hl-gl\0";
pub const IDENT_GLSL_VERSION: &[u8] = b"OpenGL ES GLSL ES 3.00\0";
/// No non-core extensions are advertised: the indexed query `glGetStringi` (the ES3 enumeration path) is
/// not implemented, so `GL_NUM_EXTENSIONS` is kept at `0` to stay consistent (an app that trusted a
/// non-zero count would then `glGetStringi` a null pointer). An empty list is the honest answer.
pub const IDENT_EXTENSIONS: &[u8] = b"\0";

/// The GLES major/minor version the driver advertises (`glGetIntegerv(GL_MAJOR_VERSION/…)`), matching the
/// `glGetString(GL_VERSION)` identity above.
pub const ES_MAJOR: i32 = 3;
pub const ES_MINOR: i32 = 0;

// ---- advertised capability limits ----------------------------------------------------------------

pub const MAX_TEXTURE_SIZE: i32 = 4096;
pub const MAX_VERTEX_ATTRIBS: i32 = crate::model::program::MAX_ATTR as i32; // 16 (the modeled attr count)
pub const MAX_TEXTURE_IMAGE_UNITS: i32 = 8; // the modeled `tex_unit` bank size
pub const MAX_VERTEX_TEXTURE_IMAGE_UNITS: i32 = 4;
pub const MAX_UNIFORM_VECTORS: i32 = 256;
pub const MAX_VARYING_VECTORS: i32 = 15;
pub const MAX_SAMPLES: i32 = 4;
pub const VIEWPORT_DIM: i32 = 4096;

/// The number of extensions advertised by `glGetString(GL_EXTENSIONS)` — see [`IDENT_EXTENSIONS`].
pub const fn num_extensions() -> i32 {
    0
}

/// `glGetString(name)` — the identity strings, NUL-terminated. An unrecognized name returns the empty
/// string (never null: a GLES app dereferences the result unconditionally).
pub fn gl_string(name: u32) -> &'static [u8] {
    match name {
        GL_VENDOR => IDENT_VENDOR,
        GL_RENDERER => IDENT_RENDERER,
        GL_VERSION => IDENT_VERSION,
        GL_SHADING_LANGUAGE_VERSION => IDENT_GLSL_VERSION,
        GL_EXTENSIONS => IDENT_EXTENSIONS,
        _ => b"\0",
    }
}

// ---- glGetIntegerv / glGetFloatv / glGetBooleanv -------------------------------------------------

/// `glGetIntegerv(pname)` — write the queried value(s) into `out` and return how many slots were written
/// (`1` for a scalar, `2`/`4` for `GL_MAX_VIEWPORT_DIMS`/`GL_VIEWPORT`/`GL_SCISSOR_BOX`). Capability
/// limits are the advertised constants; bound-object and fixed-function queries read live `ctx` state.
/// An unrecognized `pname` writes a single `0` (GL's benign fallback — matches the reference shim).
pub fn get_integerv(ctx: &GlContext, pname: u32, out: &mut [i32; 4]) -> usize {
    // Scalar helper: write one value, report a count of 1.
    let mut one = |v: i32| {
        out[0] = v;
        1
    };
    match pname {
        GL_MAX_TEXTURE_SIZE | GL_MAX_CUBE_MAP_TEXTURE_SIZE | GL_MAX_RENDERBUFFER_SIZE => {
            one(MAX_TEXTURE_SIZE)
        }
        GL_MAX_VERTEX_ATTRIBS => one(MAX_VERTEX_ATTRIBS),
        GL_MAX_TEXTURE_IMAGE_UNITS | GL_MAX_COMBINED_TEXTURE_IMAGE_UNITS => one(MAX_TEXTURE_IMAGE_UNITS),
        GL_MAX_VERTEX_TEXTURE_IMAGE_UNITS => one(MAX_VERTEX_TEXTURE_IMAGE_UNITS),
        GL_MAX_FRAGMENT_UNIFORM_VECTORS | GL_MAX_VERTEX_UNIFORM_VECTORS => one(MAX_UNIFORM_VECTORS),
        GL_MAX_VARYING_VECTORS => one(MAX_VARYING_VECTORS),
        GL_MAX_SAMPLES => one(MAX_SAMPLES),
        GL_NUM_COMPRESSED_TEXTURE_FORMATS | GL_SAMPLES => one(0),
        GL_MAJOR_VERSION => one(ES_MAJOR),
        GL_MINOR_VERSION => one(ES_MINOR),
        GL_NUM_EXTENSIONS => one(num_extensions()),
        GL_DEPTH_BITS => one(24),
        GL_STENCIL_BITS => one(8),
        GL_RED_BITS | GL_GREEN_BITS | GL_BLUE_BITS | GL_ALPHA_BITS => one(8),
        GL_CURRENT_PROGRAM => one(ctx.cur_prog as i32),
        GL_ACTIVE_TEXTURE => one((GL_TEXTURE0 + ctx.active_texture as u32) as i32),
        GL_ARRAY_BUFFER_BINDING => one(ctx.array_buffer as i32),
        GL_ELEMENT_ARRAY_BUFFER_BINDING => one(ctx.element_buffer as i32),
        GL_TEXTURE_BINDING_2D => one(ctx.tex_unit[ctx.active_texture] as i32),
        GL_FRAMEBUFFER_BINDING => one(ctx.bound_fbo as i32),
        GL_UNPACK_ALIGNMENT => one(ctx.pixel_store.unpack_alignment),
        GL_PACK_ALIGNMENT => one(ctx.pixel_store.pack_alignment),
        // Fixed-function caps read back as 1/0.
        GL_DEPTH_TEST => one(ctx.depth as i32),
        GL_BLEND => one(ctx.blend as i32),
        GL_CULL_FACE => one(ctx.cull_enabled as i32),
        GL_SCISSOR_TEST => one(ctx.scissor_enabled as i32),
        GL_MAX_VIEWPORT_DIMS => {
            out[0] = VIEWPORT_DIM;
            out[1] = VIEWPORT_DIM;
            2
        }
        GL_VIEWPORT => {
            // GL initializes the viewport to the surface size; report that when the app has not yet set
            // one (a fresh context's stored viewport is all-zero).
            let (sw, sh) = ctx.target_wh();
            out[0] = ctx.viewport[0];
            out[1] = ctx.viewport[1];
            out[2] = if ctx.viewport[2] != 0 { ctx.viewport[2] } else { sw };
            out[3] = if ctx.viewport[3] != 0 { ctx.viewport[3] } else { sh };
            4
        }
        GL_SCISSOR_BOX => {
            out[..4].copy_from_slice(&ctx.scissor);
            4
        }
        _ => one(0),
    }
}

/// `glGetFloatv(pname)` — the float-typed state a GLES app reads. Writes the value(s) into `out` and
/// returns the count. An unrecognized `pname` writes a single `0.0`.
pub fn get_floatv(ctx: &GlContext, pname: u32, out: &mut [f32; 4]) -> usize {
    match pname {
        GL_COLOR_CLEAR_VALUE => {
            out.copy_from_slice(&ctx.clear_color);
            4
        }
        GL_DEPTH_CLEAR_VALUE => {
            out[0] = ctx.clear_depth;
            1
        }
        GL_LINE_WIDTH => {
            out[0] = 1.0;
            1
        }
        _ => {
            out[0] = 0.0;
            1
        }
    }
}

/// `glGetBooleanv(pname)` — the boolean-typed state (fixed-function enables + the depth write mask).
/// Writes `GL_TRUE`/`GL_FALSE` (`1`/`0`) into `out` and returns the count; unknown `pname` writes `0`.
pub fn get_booleanv(ctx: &GlContext, pname: u32, out: &mut [u8; 4]) -> usize {
    let b = |on: bool| if on { GL_TRUE as u8 } else { GL_FALSE as u8 };
    out[0] = match pname {
        GL_DEPTH_TEST => b(ctx.depth),
        GL_BLEND => b(ctx.blend),
        GL_CULL_FACE => b(ctx.cull_enabled),
        GL_SCISSOR_TEST => b(ctx.scissor_enabled),
        GL_DEPTH_WRITEMASK => b(ctx.depth_write),
        _ => 0,
    };
    1
}

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
        GL_INFO_LOG_LENGTH => 0,
        GL_SHADER_SOURCE_LENGTH => sh.src.as_ref().map(|s| s.len() as i32 + 1).unwrap_or(0),
        GL_SHADER_TYPE => sh.kind as i32,
        GL_DELETE_STATUS => GL_FALSE as i32,
        _ => 0,
    }
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
        GL_INFO_LOG_LENGTH => 0,
        GL_DELETE_STATUS => GL_FALSE as i32,
        GL_ATTACHED_SHADERS => (p.vs != 0) as i32 + (p.fs != 0) as i32,
        GL_ACTIVE_UNIFORMS => (p.unis.len() + p.samp_names.len()) as i32,
        GL_ACTIVE_ATTRIBUTES => crate::adapter::glsl::collect_vertex_attrs(&p.vs_src).len() as i32,
        _ => 0,
    }
}

// ---- glGetUniformLocation / glGetAttribLocation --------------------------------------------------

/// `glGetUniformLocation(program, name)` — resolve `name` against the linked program's reflected uniform
/// tables (see [`crate::model::program::Program::uniform_location`]). `-1` for an unknown name / program.
pub fn uniform_location(ctx: &GlContext, program: u32, name: &str) -> i32 {
    ctx.programs.program(program).map(|p| p.uniform_location(name)).unwrap_or(-1)
}

/// `glGetAttribLocation(program, name)` — the attribute's declaration-order slot in the vertex shader
/// (see [`crate::model::program::Program::attrib_location`]). `-1` for an unknown name / program.
pub fn attrib_location(ctx: &GlContext, program: u32, name: &str) -> i32 {
    ctx.programs.program(program).map(|p| p.attrib_location(name)).unwrap_or(-1)
}
