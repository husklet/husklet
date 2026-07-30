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
// driver advertises a GLES **3.1** identity (the profile its render + compute paths back), consistent with
// the existing `hl-gl` guest identity (`GL_VENDOR = "hl-gl"`, the EGL vendor string).

pub const IDENT_VENDOR: &[u8] = b"hl-gl\0";
pub const IDENT_RENDERER: &[u8] = b"hl-gl-metal\0";
pub const IDENT_VERSION: &[u8] = b"OpenGL ES 3.1 hl-gl\0";
pub const IDENT_GLSL_VERSION: &[u8] = b"OpenGL ES GLSL ES 3.10\0";
/// The space-separated extension inventory returned by `glGetString(GL_EXTENSIONS)`.
pub const IDENT_EXTENSIONS: &[u8] =
    b"GL_KHR_debug GL_EXT_texture_format_BGRA8888 GL_EXT_read_format_bgra GL_ANGLE_robust_client_memory GL_CHROMIUM_bind_generates_resource GL_CHROMIUM_copy_texture GL_ANGLE_client_arrays GL_ANGLE_webgl_compatibility GL_ANGLE_request_extension GL_OES_EGL_image GL_OES_EGL_sync\0";

/// The advertised extension inventory, each entry a NUL-terminated name — the single source of truth for
/// `glGetStringi` (indexed enumeration) and `GL_NUM_EXTENSIONS` (the count).
pub const EXTENSIONS: &[&[u8]] = &[
    b"GL_KHR_debug\0",
    b"GL_EXT_texture_format_BGRA8888\0",
    b"GL_EXT_read_format_bgra\0",
    b"GL_ANGLE_robust_client_memory\0",
    b"GL_CHROMIUM_bind_generates_resource\0",
    b"GL_CHROMIUM_copy_texture\0",
    b"GL_ANGLE_client_arrays\0",
    b"GL_ANGLE_webgl_compatibility\0",
    b"GL_ANGLE_request_extension\0",
    b"GL_OES_EGL_image\0",
    b"GL_OES_EGL_sync\0",
];

/// The GLES major/minor version the driver advertises (`glGetIntegerv(GL_MAJOR_VERSION/…)`), matching the
/// `glGetString(GL_VERSION)` identity above.
pub const ES_MAJOR: i32 = 3;
pub const ES_MINOR: i32 = 1;

// ---- advertised capability limits ----------------------------------------------------------------

/// The largest 2D texture / renderbuffer edge. Kept consistent with the GPU-exec backend's advertised
/// `max_texture_2d` ceiling (`hl_gpu` `Capabilities::full` = 16384) so the guest-visible limit does not
/// over- or under-promise what the executor will actually validate a texture against.
pub const MAX_TEXTURE_SIZE: i32 = 16384;
pub const MAX_3D_TEXTURE_SIZE: i32 = 2048;
pub const MAX_ARRAY_TEXTURE_LAYERS: i32 = 256;
pub const MAX_VERTEX_ATTRIBS: i32 = crate::model::program::MAX_ATTR as i32; // 16 (the modeled attr count)
pub const MAX_TEXTURE_IMAGE_UNITS: i32 = 8; // the modeled `tex_unit` bank size
pub const MAX_VERTEX_TEXTURE_IMAGE_UNITS: i32 = 4;
pub const MAX_UNIFORM_VECTORS: i32 = 256;
pub const MAX_VARYING_VECTORS: i32 = 15;
pub const MAX_UNIFORM_COMPONENTS: i32 = MAX_UNIFORM_VECTORS * 4;
pub const MAX_VARYING_COMPONENTS: i32 = MAX_VARYING_VECTORS * 4;
pub const MAX_SAMPLES: i32 = 4;
pub const VIEWPORT_DIM: i32 = 16384;
/// GLES3 MRT ceilings — the spec minimum of 4 color attachments / draw buffers this model backs.
pub const MAX_COLOR_ATTACHMENTS: i32 = 4;
pub const MAX_DRAW_BUFFERS: i32 = 4;
/// `glDrawRangeElements` batch hints (GLES3). Large enough that a toolkit never clamps its draw batches.
pub const MAX_ELEMENTS_VERTICES: i32 = 1_048_576;
pub const MAX_ELEMENTS_INDICES: i32 = 1_048_576;
/// GLES3 transform-feedback and uniform-buffer limits backed by the context's indexed binding banks.
pub const MAX_TRANSFORM_FEEDBACK_SEPARATE_COMPONENTS: i32 = 4;
pub const MAX_TRANSFORM_FEEDBACK_INTERLEAVED_COMPONENTS: i32 = 64;
pub const MAX_TRANSFORM_FEEDBACK_SEPARATE_ATTRIBS: i32 =
    crate::model::glconst::MAX_TRANSFORM_FEEDBACK_BUFFERS as i32;
pub const MAX_VERTEX_UNIFORM_BLOCKS: i32 = 12;
pub const MAX_FRAGMENT_UNIFORM_BLOCKS: i32 = 12;
pub const MAX_COMBINED_UNIFORM_BLOCKS: i32 =
    MAX_VERTEX_UNIFORM_BLOCKS + MAX_FRAGMENT_UNIFORM_BLOCKS;
pub const MAX_UNIFORM_BUFFER_BINDINGS: i32 =
    crate::model::glconst::MAX_UNIFORM_BUFFER_BINDINGS as i32;
pub const MAX_UNIFORM_BLOCK_SIZE: i32 = 16 * 1024;
pub const MAX_COMBINED_UNIFORM_COMPONENTS: i32 =
    MAX_UNIFORM_COMPONENTS + crate::adapter::glsl::MAX_COMBINED_UNIFORM_BYTES as i32 / 4;
pub const UNIFORM_BUFFER_OFFSET_ALIGNMENT: i32 = 256;
pub const SHADER_STORAGE_BUFFER_OFFSET_ALIGNMENT: i32 = 256;
pub const MAX_VERTEX_OUTPUT_COMPONENTS: i32 = 64;
pub const MAX_FRAGMENT_INPUT_COMPONENTS: i32 = 60;
pub const MIN_PROGRAM_TEXEL_OFFSET: i32 = -8;
pub const MAX_PROGRAM_TEXEL_OFFSET: i32 = 7;

/// The number of extensions advertised (`glGetIntegerv(GL_NUM_EXTENSIONS)`) — the length of the
/// [`EXTENSIONS`] inventory `glGetStringi` enumerates, so the count and the indexed query agree.
pub const fn num_extensions() -> i32 {
    EXTENSIONS.len() as i32
}

/// `glGetStringi(name, index)` — the indexed extension query (the ES3 enumeration path). Returns the
/// `index`-th extension name (NUL-terminated) when `name == GL_EXTENSIONS` and `index` is in range, or
/// `None` for a bad name / an out-of-range index. The caller returns a null pointer (never a dangling
/// one) and raises the spec error for either invalid case.
pub struct DriverIdentity;

impl DriverIdentity {
    pub fn indexed(name: u32, index: u32) -> Option<&'static [u8]> {
        if name == GL_REQUESTABLE_EXTENSIONS_ANGLE {
            return None;
        }
        if name != GL_EXTENSIONS {
            return None;
        }
        EXTENSIONS.get(index as usize).copied()
    }

    /// `glGetString(name)` — the identity strings, NUL-terminated. An unrecognized name returns the empty
    /// string (never null: a GLES app dereferences the result unconditionally).
    pub fn string(name: u32) -> &'static [u8] {
        match name {
            GL_VENDOR => IDENT_VENDOR,
            GL_RENDERER => IDENT_RENDERER,
            GL_VERSION => IDENT_VERSION,
            GL_SHADING_LANGUAGE_VERSION => IDENT_GLSL_VERSION,
            GL_EXTENSIONS => IDENT_EXTENSIONS,
            _ => b"\0",
        }
    }
}

pub const STRING_I: fn(u32, u32) -> Option<&'static [u8]> = DriverIdentity::indexed;
pub const GL_STRING: fn(u32) -> &'static [u8] = DriverIdentity::string;
pub use GL_STRING as gl_string;
pub use STRING_I as string_i;

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
        GL_MAX_3D_TEXTURE_SIZE => one(MAX_3D_TEXTURE_SIZE),
        GL_MAX_ARRAY_TEXTURE_LAYERS => one(MAX_ARRAY_TEXTURE_LAYERS),
        GL_MAX_VERTEX_ATTRIBS => one(MAX_VERTEX_ATTRIBS),
        GL_MAX_TEXTURE_IMAGE_UNITS | GL_MAX_COMBINED_TEXTURE_IMAGE_UNITS => {
            one(MAX_TEXTURE_IMAGE_UNITS)
        }
        GL_MAX_VERTEX_TEXTURE_IMAGE_UNITS => one(MAX_VERTEX_TEXTURE_IMAGE_UNITS),
        GL_MAX_FRAGMENT_UNIFORM_COMPONENTS | GL_MAX_VERTEX_UNIFORM_COMPONENTS => {
            one(MAX_UNIFORM_COMPONENTS)
        }
        GL_MAX_VARYING_COMPONENTS => one(MAX_VARYING_COMPONENTS),
        GL_MAX_FRAGMENT_UNIFORM_VECTORS | GL_MAX_VERTEX_UNIFORM_VECTORS => one(MAX_UNIFORM_VECTORS),
        GL_MAX_VARYING_VECTORS => one(MAX_VARYING_VECTORS),
        GL_MAX_SAMPLES => one(MAX_SAMPLES),
        GL_MAX_COLOR_ATTACHMENTS => one(MAX_COLOR_ATTACHMENTS),
        GL_MAX_DRAW_BUFFERS => one(MAX_DRAW_BUFFERS),
        GL_MAX_ELEMENTS_VERTICES => one(MAX_ELEMENTS_VERTICES),
        GL_MAX_ELEMENTS_INDICES => one(MAX_ELEMENTS_INDICES),
        GL_MAX_TRANSFORM_FEEDBACK_SEPARATE_COMPONENTS => {
            one(MAX_TRANSFORM_FEEDBACK_SEPARATE_COMPONENTS)
        }
        GL_MAX_TRANSFORM_FEEDBACK_INTERLEAVED_COMPONENTS => {
            one(MAX_TRANSFORM_FEEDBACK_INTERLEAVED_COMPONENTS)
        }
        GL_MAX_TRANSFORM_FEEDBACK_SEPARATE_ATTRIBS => one(MAX_TRANSFORM_FEEDBACK_SEPARATE_ATTRIBS),
        GL_MAX_VERTEX_UNIFORM_BLOCKS => one(MAX_VERTEX_UNIFORM_BLOCKS),
        GL_MAX_FRAGMENT_UNIFORM_BLOCKS => one(MAX_FRAGMENT_UNIFORM_BLOCKS),
        GL_MAX_COMBINED_UNIFORM_BLOCKS => one(MAX_COMBINED_UNIFORM_BLOCKS),
        GL_MAX_UNIFORM_BUFFER_BINDINGS => one(MAX_UNIFORM_BUFFER_BINDINGS),
        GL_MAX_UNIFORM_BLOCK_SIZE => one(MAX_UNIFORM_BLOCK_SIZE),
        GL_MAX_COMBINED_VERTEX_UNIFORM_COMPONENTS | GL_MAX_COMBINED_FRAGMENT_UNIFORM_COMPONENTS => {
            one(MAX_COMBINED_UNIFORM_COMPONENTS)
        }
        GL_UNIFORM_BUFFER_OFFSET_ALIGNMENT => one(UNIFORM_BUFFER_OFFSET_ALIGNMENT),
        GL_SHADER_STORAGE_BUFFER_OFFSET_ALIGNMENT => one(SHADER_STORAGE_BUFFER_OFFSET_ALIGNMENT),
        GL_MAX_VERTEX_OUTPUT_COMPONENTS => one(MAX_VERTEX_OUTPUT_COMPONENTS),
        GL_MAX_FRAGMENT_INPUT_COMPONENTS => one(MAX_FRAGMENT_INPUT_COMPONENTS),
        GL_MIN_PROGRAM_TEXEL_OFFSET => one(MIN_PROGRAM_TEXEL_OFFSET),
        GL_MAX_PROGRAM_TEXEL_OFFSET => one(MAX_PROGRAM_TEXEL_OFFSET),
        GL_NUM_COMPRESSED_TEXTURE_FORMATS | GL_SAMPLES => one(0),
        GL_MAJOR_VERSION => one(ctx.local.client_major),
        GL_MINOR_VERSION => one(ctx.local.client_minor),
        GL_NUM_EXTENSIONS => one(num_extensions()),
        GL_NUM_REQUESTABLE_EXTENSIONS_ANGLE => one(0),
        GL_DEPTH_BITS => one(24),
        GL_STENCIL_BITS => one(8),
        GL_RED_BITS | GL_GREEN_BITS | GL_BLUE_BITS | GL_ALPHA_BITS => one(8),
        GL_CURRENT_PROGRAM => one(ctx.local.cur_prog as i32),
        GL_ACTIVE_TEXTURE => one((GL_TEXTURE0 + ctx.local.active_texture as u32) as i32),
        GL_ARRAY_BUFFER_BINDING => one(ctx.local.array_buffer as i32),
        GL_ELEMENT_ARRAY_BUFFER_BINDING => one(ctx.local.element_buffer as i32),
        GL_TEXTURE_BINDING_2D => one(ctx.local.tex_unit[ctx.local.active_texture] as i32),
        // GL_DRAW_FRAMEBUFFER_BINDING shares GL_FRAMEBUFFER_BINDING's enum value (0x8CA6).
        GL_FRAMEBUFFER_BINDING => one(ctx.local.bound_fbo as i32),
        GL_READ_FRAMEBUFFER_BINDING => one(ctx.local.read_fbo as i32),
        GL_RENDERBUFFER_BINDING => one(ctx.local.bound_rbo as i32),
        GL_UNPACK_ALIGNMENT => one(ctx.local.pixel_store.unpack_alignment),
        GL_PACK_ALIGNMENT => one(ctx.local.pixel_store.pack_alignment),
        // Fixed-function caps read back as 1/0.
        GL_DEPTH_TEST => one(ctx.local.pipeline.depth as i32),
        GL_STENCIL_TEST => one(ctx.local.pipeline.stencil as i32),
        GL_STENCIL_CLEAR_VALUE => one(ctx.local.pipeline.clear_stencil),
        GL_BLEND => one(ctx.local.pipeline.blend as i32),
        GL_CULL_FACE => one(ctx.local.pipeline.cull_enabled as i32),
        GL_SCISSOR_TEST => one(ctx.local.pipeline.scissor_enabled as i32),
        GL_MAX_VIEWPORT_DIMS => {
            out[0] = VIEWPORT_DIM;
            out[1] = VIEWPORT_DIM;
            2
        }
        GL_VIEWPORT => {
            // GL initializes the viewport to the surface size; report that when the app has not yet set
            // one (a fresh context's stored viewport is all-zero).
            let (sw, sh) = ctx.target_wh();
            out[0] = ctx.local.pipeline.viewport[0];
            out[1] = ctx.local.pipeline.viewport[1];
            out[2] = if ctx.local.pipeline.viewport[2] != 0 {
                ctx.local.pipeline.viewport[2]
            } else {
                sw
            };
            out[3] = if ctx.local.pipeline.viewport[3] != 0 {
                ctx.local.pipeline.viewport[3]
            } else {
                sh
            };
            4
        }
        GL_SCISSOR_BOX => {
            out[..4].copy_from_slice(&ctx.local.pipeline.scissor);
            4
        }
        _ => one(0),
    }
}

/// `glGetIntegeri_v(target, index)` / `glGetInteger64i_v` / `glGetBooleani_v` — the INDEXED integer state.
/// The indexed-buffer targets (`GL_UNIFORM_BUFFER_BINDING`, `GL_SHADER_STORAGE_BUFFER_BINDING`, …) report
/// the buffer / start / size bound at `index` by `glBindBufferBase`/`glBindBufferRange` (real state); any
/// other target falls back to the non-indexed scalar value (matches the reference shim). Returns the single
/// integer for `target` at `index`.
pub fn get_integer_indexed(ctx: &GlContext, target: u32, index: u32) -> i64 {
    // Map the indexed *binding* pname to the buffer target whose indexed bindings it reads back.
    let buffer_target = match target {
        GL_UNIFORM_BUFFER_BINDING => Some(GL_UNIFORM_BUFFER),
        GL_SHADER_STORAGE_BUFFER_BINDING => Some(GL_SHADER_STORAGE_BUFFER),
        GL_TRANSFORM_FEEDBACK_BUFFER_BINDING => Some(GL_TRANSFORM_FEEDBACK_BUFFER),
        _ => None,
    };
    if let Some(bt) = buffer_target {
        return ctx
            .local
            .indexed_buffers
            .get(&(bt, index))
            .map(|b| b.buffer as i64)
            .unwrap_or(0);
    }
    let mut buf = [0i32; 4];
    let n = get_integerv(ctx, target, &mut buf);
    if n > 0 {
        buf[0] as i64
    } else {
        0
    }
}

/// `glGetFloatv(pname)` — the float-typed state a GLES app reads. Writes the value(s) into `out` and
/// returns the count. An unrecognized `pname` writes a single `0.0`.
pub fn get_floatv(ctx: &GlContext, pname: u32, out: &mut [f32; 4]) -> usize {
    match pname {
        GL_COLOR_CLEAR_VALUE => {
            out.copy_from_slice(&ctx.local.pipeline.clear_color);
            4
        }
        GL_DEPTH_CLEAR_VALUE => {
            out[0] = ctx.local.pipeline.clear_depth;
            1
        }
        GL_LINE_WIDTH => {
            out[0] = 1.0;
            1
        }
        GL_ALIASED_POINT_SIZE_RANGE | GL_ALIASED_LINE_WIDTH_RANGE => {
            out[0] = 1.0;
            out[1] = 1.0;
            2
        }
        GL_MAX_TEXTURE_LOD_BIAS => {
            out[0] = 2.0;
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
    if pname == GL_COLOR_WRITEMASK {
        for (index, out) in out.iter_mut().enumerate() {
            *out = b(ctx.local.pipeline.color_mask & (1 << index) != 0);
        }
        return 4;
    }
    out[0] = match pname {
        GL_DEPTH_TEST => b(ctx.local.pipeline.depth),
        GL_STENCIL_TEST => b(ctx.local.pipeline.stencil),
        GL_BLEND => b(ctx.local.pipeline.blend),
        GL_CULL_FACE => b(ctx.local.pipeline.cull_enabled),
        GL_SCISSOR_TEST => b(ctx.local.pipeline.scissor_enabled),
        GL_DEPTH_WRITEMASK => b(ctx.local.pipeline.depth_write),
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
        GL_DELETE_STATUS => GL_FALSE as i32,
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
