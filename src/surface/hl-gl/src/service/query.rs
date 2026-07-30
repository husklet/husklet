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

mod limits;
mod reflection;

pub use limits::*;
pub use reflection::*;

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
    b"GL_KHR_debug GL_EXT_texture_format_BGRA8888 GL_EXT_read_format_bgra GL_ANGLE_robust_client_memory GL_CHROMIUM_bind_generates_resource GL_CHROMIUM_copy_texture GL_ANGLE_client_arrays GL_ANGLE_webgl_compatibility GL_ANGLE_request_extension GL_OES_EGL_image GL_OES_EGL_sync GL_OES_rgb8_rgba8 GL_OES_depth24\0";

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
    // Sized renderbuffer formats. `glRenderbufferStorage` accepts `GL_RGB8`/`GL_RGBA8` and allocates a
    // real RGBA8 plane, and a `GL_DEPTH_COMPONENT24` renderbuffer becomes a depth attachment of at least
    // 24 bits (`GL_DEPTH_BITS` = 24) — so both claims are ones the frame builder honours. An off-screen
    // FBO (glmark2 `--off-screen`) needs them to pick 8-bit color + 24-bit depth over RGBA4/depth16.
    b"GL_OES_rgb8_rgba8\0",
    b"GL_OES_depth24\0",
];

/// The GLES major/minor version the driver advertises (`glGetIntegerv(GL_MAJOR_VERSION/…)`), matching the
/// `glGetString(GL_VERSION)` identity above.
pub const ES_MAJOR: i32 = 3;
pub const ES_MINOR: i32 = 1;

// ---- advertised capability limits ----------------------------------------------------------------
//
// The whole table (values + their backing) lives in [`limits`] and is re-exported here, so
// `query::MAX_TEXTURE_SIZE` and friends keep resolving for the shim and the record path.

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
    if let Some(value) = limits::CapabilityLimits::scalar(pname) {
        return one(value);
    }
    match pname {
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
    if let Some(value) = limits::CapabilityLimits::indexed(target, index) {
        return i64::from(value);
    }
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
