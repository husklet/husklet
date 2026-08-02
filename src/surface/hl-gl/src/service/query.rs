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
    b"GL_KHR_debug GL_EXT_texture_format_BGRA8888 GL_EXT_read_format_bgra GL_ANGLE_robust_client_memory GL_CHROMIUM_bind_generates_resource GL_CHROMIUM_copy_texture GL_ANGLE_client_arrays GL_ANGLE_webgl_compatibility GL_ANGLE_request_extension GL_OES_EGL_image GL_OES_EGL_sync GL_OES_rgb8_rgba8 GL_OES_depth24 GL_OES_mapbuffer GL_EXT_color_buffer_float\0";

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
    // The GLES 2 buffer-mapping extension. `glMapBufferOES` is expressed as `glMapBufferRange` over
    // `[0, size)` with `GL_MAP_WRITE_BIT` and accepts only `GL_WRITE_ONLY_OES`; `glUnmapBufferOES` shares
    // the ES 3 `glUnmapBuffer` flush; `glGetBufferPointerv` reports the live mapping. Its two `*OES` entry
    // points resolve through `eglGetProcAddress` only (they are not exported from the `.so`), which is how
    // the GLES spec requires extension functions to be obtained.
    b"GL_OES_mapbuffer\0",
    // Float colour buffers: the seven formats `EXT_color_buffer_float` names are colour-renderable
    // (`record::framebuffers::colour_renderable`). Every path that a newly-complete float framebuffer
    // reaches was checked before this string was added, because an extension string is a promise to every
    // application and not only to the one whose failure prompted it — allocation by the plane's own texel,
    // an upload that emits that texel rather than narrowing first, a clear on both executors including
    // the clear-only frame that lowers to a rectangle fill, and a readback at the plane's own stride
    // through both `GL_UNSIGNED_BYTE` and the `GL_RGBA`/`GL_FLOAT` pair the specification requires.
    b"GL_EXT_color_buffer_float\0",
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
        // The default framebuffer's depth/stencil AS THE CONTEXT'S `EGLConfig` advertises them, so this
        // and `eglGetConfigAttrib` can never disagree (see `LocalState::depth_bits`).
        GL_DEPTH_BITS => one(ctx.local.depth_bits),
        GL_STENCIL_BITS => one(ctx.local.stencil_bits),
        GL_RED_BITS | GL_GREEN_BITS | GL_BLUE_BITS | GL_ALPHA_BITS => one(8),
        // ES 3.0 §4.3.2: the second format/type pair glReadPixels accepts, which the specification defines
        // against the CURRENTLY BOUND READ COLOUR BUFFER — not against a constant. Answering
        // GL_UNSIGNED_BYTE for every buffer told an application reading a float framebuffer to pack its
        // pixels into bytes, which silently discards exactly the range that made the buffer float. The
        // required pair for such a buffer (GL_RGBA / GL_FLOAT) is accepted regardless of what is reported
        // here; this reports the pair that can actually carry the buffer's values.
        GL_IMPLEMENTATION_COLOR_READ_FORMAT => one(GL_RGBA as i32),
        GL_IMPLEMENTATION_COLOR_READ_TYPE => one(if ctx.read_colour_buffer_is_float() {
            GL_FLOAT as i32
        } else {
            GL_UNSIGNED_BYTE as i32
        }),
        // ES 2.0 §6.1.5: GL_TRUE on any implementation that accepts shader SOURCE, which this driver does.
        GL_SHADER_COMPILER => one(GL_TRUE as i32),
        GL_CURRENT_PROGRAM => one(ctx.local.cur_prog as i32),
        // Blend state readback. Every embedded toolkit saves and restores this around its own drawing, so
        // a `0` here is not a harmless unknown — it is the value the app then installs.
        GL_BLEND_SRC_RGB => one(ctx.local.pipeline.blend_src_rgb as i32),
        GL_BLEND_DST_RGB => one(ctx.local.pipeline.blend_dst_rgb as i32),
        GL_BLEND_SRC_ALPHA_STATE => one(ctx.local.pipeline.blend_src_alpha as i32),
        GL_BLEND_DST_ALPHA => one(ctx.local.pipeline.blend_dst_alpha as i32),
        GL_BLEND_EQUATION_RGB => one(ctx.local.pipeline.blend_eq_rgb as i32),
        GL_BLEND_EQUATION_ALPHA => one(ctx.local.pipeline.blend_eq_alpha as i32),
        GL_VERTEX_ARRAY_BINDING => one(ctx.current_vertex_array() as i32),
        GL_ACTIVE_TEXTURE => one((GL_TEXTURE0 + ctx.local.active_texture as u32) as i32),
        GL_ARRAY_BUFFER_BINDING => one(ctx.local.array_buffer as i32),
        GL_ELEMENT_ARRAY_BUFFER_BINDING => one(ctx.local.element_buffer as i32),
        GL_TEXTURE_BINDING_2D => one(ctx.local.tex_unit[ctx.local.active_texture] as i32),
        // GL_DRAW_FRAMEBUFFER_BINDING shares GL_FRAMEBUFFER_BINDING's enum value (0x8CA6).
        GL_FRAMEBUFFER_BINDING => one(ctx.local.bound_fbo as i32),
        GL_READ_FRAMEBUFFER_BINDING => one(ctx.local.read_fbo as i32),
        GL_RENDERBUFFER_BINDING => one(ctx.local.bound_rbo as i32),
        // Pixel-store readback. Every one of these is honoured on the upload/readback path already, but
        // only the two alignments could be READ BACK — the rest reported 0 whatever the app had set. A
        // toolkit that saves this state, draws, and restores it therefore installed 0 for row length and
        // both skips, silently undoing its own `glPixelStorei`. ES 3.0 §2.2.2 requires every state value
        // to read back through Get*, and a write-only state is worse than an unimplemented one.
        GL_UNPACK_ALIGNMENT => one(ctx.local.pixel_store.unpack_alignment),
        GL_PACK_ALIGNMENT => one(ctx.local.pixel_store.pack_alignment),
        GL_UNPACK_ROW_LENGTH => one(ctx.local.pixel_store.unpack_row_length),
        GL_UNPACK_SKIP_ROWS => one(ctx.local.pixel_store.unpack_skip_rows),
        GL_UNPACK_SKIP_PIXELS => one(ctx.local.pixel_store.unpack_skip_pixels),
        GL_PACK_ROW_LENGTH => one(ctx.local.pixel_store.pack_row_length),
        GL_PACK_SKIP_ROWS => one(ctx.local.pixel_store.pack_skip_rows),
        GL_PACK_SKIP_PIXELS => one(ctx.local.pixel_store.pack_skip_pixels),
        // Fixed-function caps read back as 1/0.
        GL_DEPTH_TEST => one(ctx.local.pipeline.depth as i32),
        GL_STENCIL_TEST => one(ctx.local.pipeline.stencil as i32),
        GL_STENCIL_CLEAR_VALUE => one(ctx.local.pipeline.clear_stencil),
        GL_BLEND => one(ctx.local.pipeline.blend as i32),
        GL_CULL_FACE => one(ctx.local.pipeline.cull_enabled as i32),
        // The culled face and the front-face winding themselves, not just the enable. Both were absent
        // from this table and fell through to `0`, which is not even a legal enum — an application that
        // saves and restores them installed `0` for the mode it had set, while the RENDERING was correct,
        // so nothing downstream flagged it.
        GL_CULL_FACE_MODE => one(ctx.local.pipeline.cull_face as i32),
        GL_FRONT_FACE => one(ctx.local.pipeline.front_face as i32),
        GL_SCISSOR_TEST => one(ctx.local.pipeline.scissor_enabled as i32),
        GL_RASTERIZER_DISCARD => one(ctx.local.pipeline.rasterizer_discard as i32),
        // ES 3.0 §2.2.2: every state value must read back the same through each Get* variant.
        GL_DEPTH_WRITEMASK => one(ctx.local.pipeline.depth_write as i32),
        GL_DEPTH_RANGE => {
            out[0] = ctx.local.pipeline.depth_range[0] as i32;
            out[1] = ctx.local.pipeline.depth_range[1] as i32;
            2
        }
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

/// `glGetVertexAttrib{i,f}v(index, pname)` — one enabled/format/stride/binding value of vertex attribute
/// array `index`, as [`crate::model::program::Attr`] records it. `None` for an out-of-range index or a
/// `pname` this model does not track, so the caller can raise the spec error rather than write a value.
///
/// The attribute arrays are VAO state (`glBindVertexArray` swaps them in and out), so this reads the LIVE
/// array set — which is what makes the answer differ between two bound VAOs. Reporting a constant `0` here
/// made every save-and-restore an embedded toolkit performs reinstall a disabled, unbound attribute.
pub fn get_vertex_attrib(ctx: &GlContext, index: u32, pname: u32) -> Option<i32> {
    let attr = ctx.attributes().get(index as usize)?;
    Some(match pname {
        GL_VERTEX_ATTRIB_ARRAY_ENABLED => attr.enabled as i32,
        GL_VERTEX_ATTRIB_ARRAY_SIZE => attr.size,
        GL_VERTEX_ATTRIB_ARRAY_STRIDE => attr.stride,
        GL_VERTEX_ATTRIB_ARRAY_TYPE => attr.kind as i32,
        GL_VERTEX_ATTRIB_ARRAY_NORMALIZED => attr.normalized as i32,
        GL_VERTEX_ATTRIB_ARRAY_INTEGER => attr.integer as i32,
        GL_VERTEX_ATTRIB_ARRAY_BUFFER_BINDING => attr.buffer as i32,
        GL_VERTEX_ATTRIB_ARRAY_DIVISOR => attr.divisor as i32,
        _ => return None,
    })
}

/// The four components of the generic (disabled-array) vertex attribute `index`
/// (`glGetVertexAttribfv(GL_CURRENT_VERTEX_ATTRIB)`). `None` for an out-of-range index.
pub fn get_current_vertex_attrib(ctx: &GlContext, index: u32) -> Option<[f32; 4]> {
    ctx.current_vertex_attributes().get(index as usize).copied()
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
        // Both ranges are `[1, 1]`, and that is HONEST rather than unambitious. Granting only unity is
        // legal (ES 3.0 §3.4/§3.5 fix no minimum above it), but it makes every wide-point and wide-line
        // conformance case unsatisfiable by construction, so the reason is recorded here rather than
        // re-investigated:
        //
        // * LINE WIDTH cannot be widened at all. WebGPU removed wide lines: `wgpu::PrimitiveState` carries
        //   topology, strip index format, winding, cull mode, unclipped depth and polygon mode, and no
        //   width. The neutral IR has no field for one either, so there is nothing between this driver and
        //   the rasterizer that could carry it. This range is final.
        // * POINT SIZE is not as clear-cut and is deliberately left at unity for now. WGSL has no point
        //   size, but the Metal path underneath does: `wgpu-hal` sets naga's `allow_and_force_point_size`
        //   whenever the topology class is Point, and naga passes a shader-declared `PointSize` through
        //   when it is set. So the capability may exist on that path — but assigning `gl_PointSize` is
        //   currently what silently destroys the context, and advertising a wider range before that is
        //   fixed would turn "unsatisfiable" into "wedges", which is strictly worse. Revisit once it is.
        GL_ALIASED_POINT_SIZE_RANGE | GL_ALIASED_LINE_WIDTH_RANGE => {
            out[0] = 1.0;
            out[1] = 1.0;
            2
        }
        GL_MAX_TEXTURE_LOD_BIAS => {
            out[0] = 2.0;
            1
        }
        GL_BLEND_COLOR => {
            out.copy_from_slice(&ctx.local.pipeline.blend_color);
            4
        }
        // ES 2.0 table 6.19: two floats — the range `glDepthRangef` set. This reported a permanent
        // `[0, 1]` alongside a comment asserting the call was a no-op; it is not, and the viewport
        // transform now applies it.
        GL_DEPTH_RANGE => {
            out[0] = ctx.local.pipeline.depth_range[0];
            out[1] = ctx.local.pipeline.depth_range[1];
            2
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
        GL_RASTERIZER_DISCARD => b(ctx.local.pipeline.rasterizer_discard),
        GL_DEPTH_WRITEMASK => b(ctx.local.pipeline.depth_write),
        GL_SHADER_COMPILER => b(true),
        _ => 0,
    };
    1
}
