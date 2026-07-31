//! The advertised `glGetIntegerv` capability limits — one table, each value backed by the model, the IR
//! and the host executor.
//!
//! A limit here is a promise: at or above the GLES minimum for the profile
//! [`crate::service::query::IDENT_VERSION`] advertises AND genuinely honoured by the shim, the encoded IR
//! and the executor that replays it. Where the two disagree the SMALLER number is advertised (the host
//! ceiling wins), because an over-reported limit becomes a submit-time rejection the app cannot see
//! coming. `tests/gles_limits.rs` asserts the whole table against the specification minima.

use crate::model::glconst::*;

// ES3.1-only `glGetIntegerv` pnames. They live here rather than in `glconst` because nothing outside the
// limit table reads them.
pub const GL_SUBPIXEL_BITS: u32 = 0x0D50;
pub const GL_MAX_COMPUTE_SHARED_MEMORY_SIZE: u32 = 0x8262;
pub const GL_MAX_COMPUTE_UNIFORM_COMPONENTS: u32 = 0x8263;
pub const GL_MAX_COMPUTE_ATOMIC_COUNTERS: u32 = 0x8265;
pub const GL_MAX_COMBINED_COMPUTE_UNIFORM_COMPONENTS: u32 = 0x8266;
pub const GL_MAX_UNIFORM_LOCATIONS: u32 = 0x826E;
pub const GL_MAX_VERTEX_ATTRIB_RELATIVE_OFFSET: u32 = 0x82D9;
pub const GL_MAX_VERTEX_ATTRIB_BINDINGS: u32 = 0x82DA;
pub const GL_MAX_VERTEX_ATTRIB_STRIDE: u32 = 0x82E5;
pub const GL_MAX_ELEMENT_INDEX: u32 = 0x8D6B;
pub const GL_MAX_SAMPLE_MASK_WORDS: u32 = 0x8E59;
pub const GL_MAX_IMAGE_UNITS: u32 = 0x8F38;
pub const GL_MAX_COMBINED_SHADER_OUTPUT_RESOURCES: u32 = 0x8F39;
pub const GL_MAX_COMBINED_IMAGE_UNIFORMS: u32 = 0x90CF;
pub const GL_MAX_VERTEX_SHADER_STORAGE_BLOCKS: u32 = 0x90D6;
pub const GL_MAX_FRAGMENT_SHADER_STORAGE_BLOCKS: u32 = 0x90DA;
pub const GL_MAX_COMPUTE_SHADER_STORAGE_BLOCKS: u32 = 0x90DB;
pub const GL_MAX_COMBINED_SHADER_STORAGE_BLOCKS: u32 = 0x90DC;
pub const GL_MAX_SHADER_STORAGE_BUFFER_BINDINGS: u32 = 0x90DD;
pub const GL_MAX_SHADER_STORAGE_BLOCK_SIZE: u32 = 0x90DE;
pub const GL_MAX_COMPUTE_WORK_GROUP_INVOCATIONS: u32 = 0x90EB;
pub const GL_MAX_COLOR_TEXTURE_SAMPLES: u32 = 0x910E;
pub const GL_MAX_DEPTH_TEXTURE_SAMPLES: u32 = 0x910F;
pub const GL_MAX_INTEGER_SAMPLES: u32 = 0x9110;
pub const GL_MAX_COMPUTE_UNIFORM_BLOCKS: u32 = 0x91BB;
pub const GL_MAX_COMPUTE_TEXTURE_IMAGE_UNITS: u32 = 0x91BC;
pub const GL_MAX_COMPUTE_IMAGE_UNIFORMS: u32 = 0x91BD;
pub const GL_MAX_COMPUTE_WORK_GROUP_COUNT: u32 = 0x91BE;
pub const GL_MAX_COMPUTE_WORK_GROUP_SIZE: u32 = 0x91BF;
pub const GL_MAX_ATOMIC_COUNTER_BUFFER_SIZE: u32 = 0x92D8;
pub const GL_MAX_ATOMIC_COUNTER_BUFFER_BINDINGS: u32 = 0x92DC;
pub const GL_MAX_FRAMEBUFFER_WIDTH: u32 = 0x9315;
pub const GL_MAX_FRAMEBUFFER_HEIGHT: u32 = 0x9316;
pub const GL_MAX_FRAMEBUFFER_SAMPLES: u32 = 0x9318;

// ---- 2D / 3D / array texture edges ---------------------------------------------------------------

/// The largest 2D texture / cube face / renderbuffer edge. Bounded by the HOST: both real executors
/// advertise `hl_gpu` `Capabilities::max_texture_2d == 8192` and
/// `hl_gpu::runtime::service::validate` REJECTS a larger `CreateTexture`, so the older 16384 claim was a
/// promise the backend would refuse at submit time.
pub const MAX_TEXTURE_SIZE: i32 = 8192;
/// wgpu's `max_texture_dimension_3d` ceiling, shared by 3D and array textures.
pub const MAX_3D_TEXTURE_SIZE: i32 = 2048;
pub const MAX_ARRAY_TEXTURE_LAYERS: i32 = 256;
pub const VIEWPORT_DIM: i32 = MAX_TEXTURE_SIZE;
pub const MAX_FRAMEBUFFER_DIM: i32 = MAX_TEXTURE_SIZE;

// ---- vertex input --------------------------------------------------------------------------------

pub const MAX_VERTEX_ATTRIBS: i32 = crate::model::program::MAX_ATTR as i32;
/// One separate-format binding per attribute location (the modelled `vertex_bindings` bank).
pub const MAX_VERTEX_ATTRIB_BINDINGS: i32 = MAX_VERTEX_ATTRIBS;
/// wgpu's `max_vertex_buffer_array_stride`.
pub const MAX_VERTEX_ATTRIB_STRIDE: i32 = 2048;
pub const MAX_VERTEX_ATTRIB_RELATIVE_OFFSET: i32 = MAX_VERTEX_ATTRIB_STRIDE - 1;
/// `IndexFormat::U32` carries the full 32-bit range; reported as the largest value this query's `GLint`
/// form can express.
pub const MAX_ELEMENT_INDEX: i32 = i32::MAX;

// ---- texture image units -------------------------------------------------------------------------

/// Per-stage sampler ceiling. Backed by [`crate::adapter::glsl::MAX_FRAGMENT_SAMPLERS`] (the link-time
/// check) and, on the host, by wgpu's guaranteed 16 sampled-textures + 16 samplers per shader stage —
/// which is also Metal's 16-sampler-per-stage argument limit, so this cannot be raised further.
pub const MAX_TEXTURE_IMAGE_UNITS: i32 = crate::adapter::glsl::MAX_FRAGMENT_SAMPLERS as i32;
pub const MAX_VERTEX_TEXTURE_IMAGE_UNITS: i32 = crate::adapter::glsl::MAX_VERTEX_SAMPLERS as i32;
/// The texture-unit bank size (`LocalState::tex_unit` / `DrawCall::tex_units`) — the units
/// `glActiveTexture` accepts and the frame builder can bind from.
pub const MAX_COMBINED_TEXTURE_IMAGE_UNITS: i32 = MAX_TEXTURE_UNITS as i32;

// ---- program uniforms / varyings -----------------------------------------------------------------

/// Default-block uniform ceiling for the VERTEX and FRAGMENT stages, in `vec4`s. The two stages are
/// flattened into ONE std140 block ([`crate::adapter::glsl::MAX_COMBINED_UNIFORM_BYTES`]), so their
/// ceilings are genuinely identical and both are enforced at link.
///
/// Backed end to end, and NOT the old ES3.0 floor of 256:
///   * storage is a heap `Vec<u8>` sized by the linker (`Program::ubuf`), not a fixed array;
///   * the block is uploaded as an ordinary `CreateBuffer`+`WriteBuffer` — `hl_gpu`'s only gate is the
///     generic `max_buffer_bytes` / `max_frame_bytes` (256 MiB each), thousands of times larger;
///   * wgpu on Metal reports `max_uniform_buffer_binding_size == max_buffer_size` (GiBs), and the device
///     is created with `adapter.limits()`, so the binding size is not the constraint either.
/// The honest bound is therefore OUR OWN budget: 256 KiB for the combined block, which every advertised
/// program fits even in the worst case (a scalar array costs a full 16-byte std140 stride per component,
/// so `2 stages * 8192 components * 16 B` is exactly that budget). A program above it is refused at
/// `glLinkProgram` with `UniformError::StageComponents` / `BlockBytes`, never silently truncated.
pub const MAX_UNIFORM_VECTORS: i32 = 2048;
pub const MAX_UNIFORM_COMPONENTS: i32 = MAX_UNIFORM_VECTORS * 4;
/// COMPUTE has no default-block path at all: `Program::link` takes an early return for a compute source
/// and never reflects a uniform layout, and `service::compute` binds ONLY the `glBindBufferBase`d
/// UBO/SSBO bindings. A `glUniform*` on a compute program would go nowhere, so the honest ceiling is
/// zero — DELIBERATELY BELOW the ES3.1 minimum of 1024, which this driver's 3.1 profile already does not
/// meet (see the `eglCreateContext` default-version note on images/atomics). A compute shader that
/// declares a default-block uniform is refused at link (`UniformError::ComputeDefaultBlock`) rather than
/// reading zeros. Named uniform BLOCKS work in compute and keep their real limits below.
pub const MAX_COMPUTE_UNIFORM_COMPONENTS: i32 = 0;
pub const MAX_VARYING_VECTORS: i32 = 15;
pub const MAX_VARYING_COMPONENTS: i32 = MAX_VARYING_VECTORS * 4;
pub const MAX_VERTEX_OUTPUT_COMPONENTS: i32 = 64;
pub const MAX_FRAGMENT_INPUT_COMPONENTS: i32 = 60;
/// Uniform locations are DENSE indices into the linked program's uniform list (one per array element,
/// then one per sampler element — `Program::uniform_location`), so the largest location a link can hand
/// out is bounded by the combined component budget: every element costs at least one component.
pub const MAX_UNIFORM_LOCATIONS: i32 = 2 * MAX_UNIFORM_COMPONENTS;
pub const MIN_PROGRAM_TEXEL_OFFSET: i32 = -8;
pub const MAX_PROGRAM_TEXEL_OFFSET: i32 = 7;

// ---- uniform blocks ------------------------------------------------------------------------------

pub const MAX_VERTEX_UNIFORM_BLOCKS: i32 = 12;
pub const MAX_FRAGMENT_UNIFORM_BLOCKS: i32 = 12;
pub const MAX_COMPUTE_UNIFORM_BLOCKS: i32 = 12;
pub const MAX_COMBINED_UNIFORM_BLOCKS: i32 =
    MAX_VERTEX_UNIFORM_BLOCKS + MAX_FRAGMENT_UNIFORM_BLOCKS;
pub const MAX_UNIFORM_BUFFER_BINDINGS: i32 =
    crate::model::glconst::MAX_UNIFORM_BUFFER_BINDINGS as i32;
/// A named block is uploaded verbatim as its own buffer, so the ceiling is the smallest binding size any
/// backend we run on guarantees: wgpu's `Limits::default().max_uniform_buffer_binding_size`, 64 KiB.
/// (Metal reports far more — `max_buffer_size` — but 64 KiB is the number that holds on every adapter.)
pub const MAX_UNIFORM_BLOCK_SIZE: i32 = 64 * 1024;
/// The default block plus every bindable uniform block, per the spec's own composition rule.
pub const MAX_COMBINED_UNIFORM_COMPONENTS: i32 =
    MAX_UNIFORM_COMPONENTS + MAX_VERTEX_UNIFORM_BLOCKS * (MAX_UNIFORM_BLOCK_SIZE / 4);
/// Same composition, but the compute stage contributes no default block.
pub const MAX_COMBINED_COMPUTE_UNIFORM_COMPONENTS: i32 =
    MAX_COMPUTE_UNIFORM_COMPONENTS + MAX_COMPUTE_UNIFORM_BLOCKS * (MAX_UNIFORM_BLOCK_SIZE / 4);
pub const UNIFORM_BUFFER_OFFSET_ALIGNMENT: i32 = 256;

// ---- shader storage (ES3.1) ----------------------------------------------------------------------

/// The indexed SSBO binding bank the compute path lowers into a bind group.
pub const MAX_SHADER_STORAGE_BUFFER_BINDINGS: i32 =
    crate::model::glconst::MAX_SHADER_STORAGE_BUFFER_BINDINGS as i32;
/// SSBOs are lowered by the COMPUTE path only; the render path's bind group carries a uniform block and
/// sampled textures, so the per-stage graphics counts are honestly zero (the ES3.1 minimum for them).
pub const MAX_VERTEX_SHADER_STORAGE_BLOCKS: i32 = 0;
pub const MAX_FRAGMENT_SHADER_STORAGE_BLOCKS: i32 = 0;
pub const MAX_COMPUTE_SHADER_STORAGE_BLOCKS: i32 = 4;
pub const MAX_COMBINED_SHADER_STORAGE_BLOCKS: i32 = MAX_COMPUTE_SHADER_STORAGE_BLOCKS;
/// wgpu's `max_storage_buffer_binding_size` guarantee (128 MiB), under the executor's
/// `max_buffer_bytes`.
pub const MAX_SHADER_STORAGE_BLOCK_SIZE: i32 = 1 << 27;
pub const SHADER_STORAGE_BUFFER_OFFSET_ALIGNMENT: i32 = 256;
/// Fragment outputs plus writable resources: the draw buffers, since neither images nor fragment SSBOs
/// are lowered.
pub const MAX_COMBINED_SHADER_OUTPUT_RESOURCES: i32 = MAX_DRAW_BUFFERS;

// ---- compute (ES3.1) -----------------------------------------------------------------------------

/// Per-dimension work-group count — the exact bound [`crate::service::compute`] validates against.
pub const MAX_COMPUTE_WORK_GROUP_COUNT: i32 = 65535;
/// Per-dimension work-group size, x/y/z (wgpu's `max_compute_workgroup_size_*`).
pub const MAX_COMPUTE_WORK_GROUP_SIZE: [i32; 3] = [256, 256, 64];
pub const MAX_COMPUTE_WORK_GROUP_INVOCATIONS: i32 = 256;
/// wgpu's `max_compute_workgroup_storage_size`.
pub const MAX_COMPUTE_SHARED_MEMORY_SIZE: i32 = 16384;
pub const MAX_COMPUTE_TEXTURE_IMAGE_UNITS: i32 = MAX_TEXTURE_IMAGE_UNITS;

// ---- framebuffer / multisample -------------------------------------------------------------------

pub const MAX_COLOR_ATTACHMENTS: i32 = 4;
pub const MAX_DRAW_BUFFERS: i32 = 4;
pub const MAX_SAMPLES: i32 = 4;
/// Multisample TEXTURES are not modelled, so a multisampled texture supports exactly one sample — the
/// ES3.1 minimum for these three queries.
pub const MAX_TEXTURE_SAMPLES: i32 = 1;
pub const MAX_SAMPLE_MASK_WORDS: i32 = 1;
/// Rasterization runs on the host GPU, whose subpixel precision exceeds the 4-bit ES3 minimum; reported
/// as the minimum rather than a number this driver cannot measure.
pub const SUBPIXEL_BITS: i32 = 4;

// ---- transform feedback + draw hints -------------------------------------------------------------

pub const MAX_TRANSFORM_FEEDBACK_SEPARATE_COMPONENTS: i32 = 4;
pub const MAX_TRANSFORM_FEEDBACK_INTERLEAVED_COMPONENTS: i32 = 64;
pub const MAX_TRANSFORM_FEEDBACK_SEPARATE_ATTRIBS: i32 =
    crate::model::glconst::MAX_TRANSFORM_FEEDBACK_BUFFERS as i32;
pub const MAX_ELEMENTS_VERTICES: i32 = 1_048_576;
pub const MAX_ELEMENTS_INDICES: i32 = 1_048_576;

/// The advertised capability table, keyed by `glGetIntegerv` pname.
pub struct CapabilityLimits;

impl CapabilityLimits {
    /// The advertised value for a capability `pname`, or `None` when `pname` is not a capability limit (a
    /// bound-object / fixed-function query the caller answers from live context state).
    ///
    /// KNOWN GAP, deliberately absent: `GL_MAX_IMAGE_UNITS`, `GL_MAX_*_IMAGE_UNIFORMS` and the
    /// atomic-counter limits. `glBindImageTexture` is an honest no-op and the translator rejects
    /// `atomic_uint`, so no truthful non-zero value exists; they fall through to the unknown-pname `0`.
    #[allow(clippy::too_many_lines)]
    pub fn scalar(pname: u32) -> Option<i32> {
        let value = match pname {
            GL_MAX_TEXTURE_SIZE | GL_MAX_CUBE_MAP_TEXTURE_SIZE | GL_MAX_RENDERBUFFER_SIZE => {
                MAX_TEXTURE_SIZE
            }
            GL_MAX_3D_TEXTURE_SIZE => MAX_3D_TEXTURE_SIZE,
            GL_MAX_ARRAY_TEXTURE_LAYERS => MAX_ARRAY_TEXTURE_LAYERS,
            GL_MAX_FRAMEBUFFER_WIDTH | GL_MAX_FRAMEBUFFER_HEIGHT => MAX_FRAMEBUFFER_DIM,
            GL_MAX_VERTEX_ATTRIBS => MAX_VERTEX_ATTRIBS,
            GL_MAX_VERTEX_ATTRIB_BINDINGS => MAX_VERTEX_ATTRIB_BINDINGS,
            GL_MAX_VERTEX_ATTRIB_STRIDE => MAX_VERTEX_ATTRIB_STRIDE,
            GL_MAX_VERTEX_ATTRIB_RELATIVE_OFFSET => MAX_VERTEX_ATTRIB_RELATIVE_OFFSET,
            GL_MAX_ELEMENT_INDEX => MAX_ELEMENT_INDEX,
            GL_MAX_TEXTURE_IMAGE_UNITS => MAX_TEXTURE_IMAGE_UNITS,
            GL_MAX_VERTEX_TEXTURE_IMAGE_UNITS => MAX_VERTEX_TEXTURE_IMAGE_UNITS,
            GL_MAX_COMPUTE_TEXTURE_IMAGE_UNITS => MAX_COMPUTE_TEXTURE_IMAGE_UNITS,
            GL_MAX_COMBINED_TEXTURE_IMAGE_UNITS => MAX_COMBINED_TEXTURE_IMAGE_UNITS,
            GL_MAX_FRAGMENT_UNIFORM_COMPONENTS | GL_MAX_VERTEX_UNIFORM_COMPONENTS => {
                MAX_UNIFORM_COMPONENTS
            }
            GL_MAX_COMPUTE_UNIFORM_COMPONENTS => MAX_COMPUTE_UNIFORM_COMPONENTS,
            GL_MAX_FRAGMENT_UNIFORM_VECTORS | GL_MAX_VERTEX_UNIFORM_VECTORS => MAX_UNIFORM_VECTORS,
            GL_MAX_VARYING_COMPONENTS => MAX_VARYING_COMPONENTS,
            GL_MAX_VARYING_VECTORS => MAX_VARYING_VECTORS,
            GL_MAX_VERTEX_OUTPUT_COMPONENTS => MAX_VERTEX_OUTPUT_COMPONENTS,
            GL_MAX_FRAGMENT_INPUT_COMPONENTS => MAX_FRAGMENT_INPUT_COMPONENTS,
            GL_MAX_UNIFORM_LOCATIONS => MAX_UNIFORM_LOCATIONS,
            GL_MIN_PROGRAM_TEXEL_OFFSET => MIN_PROGRAM_TEXEL_OFFSET,
            GL_MAX_PROGRAM_TEXEL_OFFSET => MAX_PROGRAM_TEXEL_OFFSET,
            GL_MAX_VERTEX_UNIFORM_BLOCKS => MAX_VERTEX_UNIFORM_BLOCKS,
            GL_MAX_FRAGMENT_UNIFORM_BLOCKS => MAX_FRAGMENT_UNIFORM_BLOCKS,
            GL_MAX_COMPUTE_UNIFORM_BLOCKS => MAX_COMPUTE_UNIFORM_BLOCKS,
            GL_MAX_COMBINED_UNIFORM_BLOCKS => MAX_COMBINED_UNIFORM_BLOCKS,
            GL_MAX_UNIFORM_BUFFER_BINDINGS => MAX_UNIFORM_BUFFER_BINDINGS,
            GL_MAX_UNIFORM_BLOCK_SIZE => MAX_UNIFORM_BLOCK_SIZE,
            GL_MAX_COMBINED_VERTEX_UNIFORM_COMPONENTS
            | GL_MAX_COMBINED_FRAGMENT_UNIFORM_COMPONENTS => MAX_COMBINED_UNIFORM_COMPONENTS,
            GL_MAX_COMBINED_COMPUTE_UNIFORM_COMPONENTS => MAX_COMBINED_COMPUTE_UNIFORM_COMPONENTS,
            GL_UNIFORM_BUFFER_OFFSET_ALIGNMENT => UNIFORM_BUFFER_OFFSET_ALIGNMENT,
            GL_MAX_SHADER_STORAGE_BUFFER_BINDINGS => MAX_SHADER_STORAGE_BUFFER_BINDINGS,
            GL_MAX_VERTEX_SHADER_STORAGE_BLOCKS => MAX_VERTEX_SHADER_STORAGE_BLOCKS,
            GL_MAX_FRAGMENT_SHADER_STORAGE_BLOCKS => MAX_FRAGMENT_SHADER_STORAGE_BLOCKS,
            GL_MAX_COMPUTE_SHADER_STORAGE_BLOCKS => MAX_COMPUTE_SHADER_STORAGE_BLOCKS,
            GL_MAX_COMBINED_SHADER_STORAGE_BLOCKS => MAX_COMBINED_SHADER_STORAGE_BLOCKS,
            GL_MAX_SHADER_STORAGE_BLOCK_SIZE => MAX_SHADER_STORAGE_BLOCK_SIZE,
            GL_SHADER_STORAGE_BUFFER_OFFSET_ALIGNMENT => SHADER_STORAGE_BUFFER_OFFSET_ALIGNMENT,
            GL_MAX_COMBINED_SHADER_OUTPUT_RESOURCES => MAX_COMBINED_SHADER_OUTPUT_RESOURCES,
            GL_MAX_COMPUTE_WORK_GROUP_COUNT => MAX_COMPUTE_WORK_GROUP_COUNT,
            // Scalar form of an INDEXED query: report the smallest per-dimension size, which holds for any
            // index the app may go on to read (see [`capability_indexed`]).
            GL_MAX_COMPUTE_WORK_GROUP_SIZE => MAX_COMPUTE_WORK_GROUP_SIZE[2],
            GL_MAX_COMPUTE_WORK_GROUP_INVOCATIONS => MAX_COMPUTE_WORK_GROUP_INVOCATIONS,
            GL_MAX_COMPUTE_SHARED_MEMORY_SIZE => MAX_COMPUTE_SHARED_MEMORY_SIZE,
            GL_MAX_COLOR_ATTACHMENTS => MAX_COLOR_ATTACHMENTS,
            GL_MAX_DRAW_BUFFERS => MAX_DRAW_BUFFERS,
            GL_MAX_SAMPLES | GL_MAX_FRAMEBUFFER_SAMPLES => MAX_SAMPLES,
            GL_MAX_COLOR_TEXTURE_SAMPLES
            | GL_MAX_DEPTH_TEXTURE_SAMPLES
            | GL_MAX_INTEGER_SAMPLES => MAX_TEXTURE_SAMPLES,
            GL_MAX_SAMPLE_MASK_WORDS => MAX_SAMPLE_MASK_WORDS,
            GL_SUBPIXEL_BITS => SUBPIXEL_BITS,
            GL_MAX_ELEMENTS_VERTICES => MAX_ELEMENTS_VERTICES,
            GL_MAX_ELEMENTS_INDICES => MAX_ELEMENTS_INDICES,
            GL_MAX_TRANSFORM_FEEDBACK_SEPARATE_COMPONENTS => {
                MAX_TRANSFORM_FEEDBACK_SEPARATE_COMPONENTS
            }
            GL_MAX_TRANSFORM_FEEDBACK_INTERLEAVED_COMPONENTS => {
                MAX_TRANSFORM_FEEDBACK_INTERLEAVED_COMPONENTS
            }
            GL_MAX_TRANSFORM_FEEDBACK_SEPARATE_ATTRIBS => MAX_TRANSFORM_FEEDBACK_SEPARATE_ATTRIBS,
            _ => return None,
        };
        Some(value)
    }

    /// The advertised value for an INDEXED capability limit (`glGetIntegeri_v`), or `None` when `pname` is
    /// not one. Only the compute work-group limits are per-dimension.
    pub fn indexed(pname: u32, index: u32) -> Option<i32> {
        let dimension = index as usize;
        match pname {
            GL_MAX_COMPUTE_WORK_GROUP_COUNT => {
                (dimension < 3).then_some(MAX_COMPUTE_WORK_GROUP_COUNT)
            }
            GL_MAX_COMPUTE_WORK_GROUP_SIZE => MAX_COMPUTE_WORK_GROUP_SIZE.get(dimension).copied(),
            _ => None,
        }
    }
}
