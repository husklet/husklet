//! DIFFERENTIAL fuzzer: the `hl-gpu` CPU oracle vs the `hl-gpu-wgpu` `WgpuExecutor`.
//!
//! The idea: mint MANY deterministic IR programs (seeded purely by a loop index — no RNG, no clock), run
//! each one through BOTH backends over the SAME runtime pipeline (validate → account → dispatch → execute),
//! read back the SAME target, and assert the two backends agree. A disagreement anywhere in the covered op
//! surface is a real executor/oracle divergence. This complements the frozen `conformance.rs` mirror (which
//! pins each backend to a hand-authored golden) by cross-checking the two backends against EACH OTHER across
//! a broad, index-derived program space, so a bug that shifts BOTH away from a golden — or one that only
//! shows up on an odd size/color/rect the goldens never tried — still surfaces.
//!
//! ## How the same program drives two very different backends
//! The CPU oracle is a FIXED-FUNCTION rasterizer: it never runs the shader module, it reads vertex bytes by
//! a stride convention (pos at 0/4[/8], color after) and composites with a hardcoded straight-alpha-over
//! equation (`src/cpu/service/raster.rs`). The wgpu backend runs REAL SPIR-V shaders on the device. To make
//! one IR program mean the same thing to both, every draw here uses vertex bytes that satisfy BOTH the CPU
//! stride layout AND a `VertexLayout` whose attributes point at the same offsets, plus a SPIR-V shader
//! (minted from a WGSL seed via naga) that simply forwards `pos`/`color` — i.e. it reproduces the oracle's
//! fixed function. Draw geometry is always a FULLSCREEN triangle `(-1,-1),(3,-1),(-1,3)`: every pixel
//! centre is strictly interior on any correct rasterizer, so coverage is identical and the only per-pixel
//! difference is last-ULP interpolation/quantisation rounding.
//!
//! The CPU oracle advertises only the KERNEL shader payload, so its runtime `validate` gate would reject a
//! `CreateShader{SpirV}` on capability grounds — yet the oracle's `create_shader` accepts SPIR-V as an
//! opaque handle (it rasterises from the pipeline, not the module). We therefore widen the CPU *session's*
//! advertised `shader_payloads` to admit SPIR-V/GLSL (see `cpu_session`); this changes nothing about what
//! the oracle computes, it just lets the identical program reach the executor on both sides.
//!
//! ## Tolerances (documented + justified)
//!   * EXACT (0): integer/replace ops — clears, `ClearRect`, buffer/texture copies, `FillBuffer`, nearest
//!     integer-upscale blits, and the winner of a depth test. These are pure byte moves or exact-integer
//!     writes on both backends.
//!   * ±1: flat opaque (replace) draws — a constant colour rounds identically on both, ±1 is a guard band.
//!   * ±2: interpolated-colour (gradient) draws and alpha-blended draws. Both backends interpolate in f32
//!     and quantise to unorm8, but the CPU rounds half-up (`v*255+0.5` truncated) while the GPU rounds
//!     half-to-even, and the barycentric weights differ at the last ULP — a per-channel error bounded by a
//!     couple of unorm steps.
//!   * ±3: linear (bilinear) scaled blits — the CPU's `sample_bilinear` and llvmpipe's hardware sampler use
//!     the same pixel-centre + clamp-to-edge convention but not bit-identical filtering; a few unorm steps.
//!
//! ## Newly-COVERED op surface (previously excluded; now folded into the differential)
//!   * STENCIL (`Enc::SetStencilReference` + the pipeline `DepthState` stencil faces/masks): the CPU oracle
//!     now MODELS the stencil test + ops (`raster.rs`'s depth path runs the stencil test/op against an
//!     8-bit stencil plane; `executor.rs` applies `SetStencilReference`). A two-pass mark-then-test program
//!     (`gen_stencil_equal` / `gen_stencil_greater`) is compared oracle-vs-executor EXACTLY (tol 0).
//!   * sRGB colour targets (`Rgba8Srgb`): the oracle now gamma-ENCODES on a clear / replace draw into an
//!     sRGB target (`cpu/format.rs::clear_texel`), matching the hardware ROP (linear 0.5 → 188, not 128).
//!     `gen_clear_srgb` / `gen_draw_srgb` are compared oracle-vs-executor within ±2 (the encode's last-ULP
//!     rounding — lavapipe's shader-write path rounds linear-0.5 to 187 where the clear path rounds 188).
//!
//! ## ANALYTIC-only op surface (executor-vs-hand-computed, NOT oracle-compared — documented)
//!   * MSAA + `Enc::ResolveTexture`: the wgpu backend now advertises + implements the resolve, but the CPU
//!     oracle has NO multisample-RENDER concept (its `validate` rejects a `sample_count > 1` colour
//!     attachment — it can average existing samples but cannot PRODUCE coverage-antialiased samples). So we
//!     do NOT fake an oracle result; instead the executor's 4× MSAA render + resolve is asserted against a
//!     HAND-COMPUTED analytic expectation (full-coverage → the flat draw colour exactly; a diagonal
//!     half-cover → exact fg interior, exact bg exterior, and averaged-gray edge pixels). See
//!     `analytic_msaa_resolve` in the test body; these are counted + reported separately from the
//!     oracle-compared programs.
//!
//! If no wgpu adapter is reachable (no lavapipe/Vulkan ICD) the whole test skips, like the rest of the suite.

use std::collections::BTreeSet;

use hl_gpu::protocol::model::capability::shader_payload;
use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BlendState, BufferDesc, ColorAttachment,
    ColorTargetState, ComputePipelineDesc, DepthAttachment, DepthState, Extent3d, Origin3d,
    RenderPipelineDesc, ShaderRef, StencilFaceState, TextureDesc, TextureSubresource, VertexAttr,
    VertexLayout,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, compare, stencil_op, texture_usage, Filter, LoadOp, TextureDim, TextureFormat,
    Topology,
};
use hl_gpu::protocol::model::kernel::{
    gty, Inst, KernelProgram, Op, Param, CMP_GE, KERNEL_MAGIC, SR_CTAID_X, SR_NTID_X, SR_TID_X,
};
use hl_gpu::{
    BufferId, Cmd, CommandBuffer, CpuExecutor, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits,
    Session, ShaderPayloadKind, TextureId,
};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

// =================================================================================================
// tiny deterministic helpers — everything is a pure arithmetic function of the seed index
// =================================================================================================

const RT: u32 = texture_usage::RENDER_TARGET | texture_usage::COPY_SRC | texture_usage::COPY_DST;

fn le_f32(vals: &[f32]) -> Vec<u8> {
    vals.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn tex(w: u32, h: u32) -> TextureDesc {
    tex_fmt(w, h, TextureFormat::Rgba8Unorm)
}

/// A copyable `RENDER_TARGET | COPY_SRC | COPY_DST` colour texture in an arbitrary colour `format` (used by
/// the sRGB programs, which render into `Rgba8Srgb`).
fn tex_fmt(w: u32, h: u32, format: TextureFormat) -> TextureDesc {
    TextureDesc {
        width: w,
        height: h,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format,
        usage: RT,
        label: String::new(),
    }
}

/// A `Depth24PlusStencil8` render target — the depth+stencil attachment a stencil-testing pipeline requires
/// (wgpu rejects a stencil pipeline paired with a depth-only attachment).
fn ds_tex(w: u32, h: u32) -> TextureDesc {
    TextureDesc {
        width: w,
        height: h,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: TextureFormat::Depth24PlusStencil8,
        usage: texture_usage::RENDER_TARGET,
        label: String::new(),
    }
}

fn depth_tex(w: u32, h: u32) -> TextureDesc {
    TextureDesc {
        width: w,
        height: h,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: TextureFormat::Depth32Float,
        usage: texture_usage::RENDER_TARGET,
        label: String::new(),
    }
}

fn buf(size: u64, usage: u32) -> BufferDesc {
    BufferDesc {
        size,
        usage,
        label: String::new(),
    }
}

/// A deterministic byte in a comfortable mid-range (avoids 0/255 clamp corners) from a seed + channel.
fn chan(seed: u64, k: u64) -> u8 {
    (16 + (seed.wrapping_mul(37).wrapping_add(k.wrapping_mul(61))) % 216) as u8
}

/// A deterministic RGBA8 texel from a seed.
fn texel(seed: u64) -> [u8; 4] {
    [chan(seed, 0), chan(seed, 1), chan(seed, 2), chan(seed, 3)]
}

/// A deterministic straight-alpha float colour (opaque) from a seed.
fn fcolor_opaque(seed: u64) -> [f32; 4] {
    [
        chan(seed, 0) as f32 / 255.0,
        chan(seed, 1) as f32 / 255.0,
        chan(seed, 2) as f32 / 255.0,
        1.0,
    ]
}

/// A deterministic straight-alpha float colour whose ALPHA is also a distinct mid-range value (never 1.0)
/// — used by the write-mask programs so masking the alpha channel is observable (a fully-opaque colour
/// would make an alpha-masked-vs-unmasked write indistinguishable).
fn fcolor4(seed: u64) -> [f32; 4] {
    [
        chan(seed, 0) as f32 / 255.0,
        chan(seed, 1) as f32 / 255.0,
        chan(seed, 2) as f32 / 255.0,
        chan(seed, 3) as f32 / 255.0,
    ]
}

// The fullscreen triangle: every pixel centre is strictly interior on any correct rasterizer. Its NDC
// winding is counter-clockwise (positive NDC signed area); through the y-down viewport transform the wgpu
// executor + the oracle both rasterize in, that maps to a NEGATIVE framebuffer-space signed area.
const FS_TRI: [(f32, f32); 3] = [(-1.0, -1.0), (3.0, -1.0), (-1.0, 3.0)];

// The SAME fullscreen triangle with two vertices swapped — identical coverage, OPPOSITE winding
// (clockwise in NDC / positive framebuffer-space signed area). Paired with `FS_TRI`, this lets the cull
// programs prove the facing decision is winding-driven, not a constant.
const FS_TRI_REV: [(f32, f32); 3] = [(-1.0, -1.0), (-1.0, 3.0), (3.0, -1.0)];

// -------------------------------------------------------------------------------------------------
// SPIR-V seeds (minted once via naga) — the wgpu backend executes these; the CPU oracle ignores them
// and rasterises fixed-function, so they only need to forward pos/colour.
// -------------------------------------------------------------------------------------------------

/// pos = Float32x2 @loc0 (bytes 0..8), colour = Float32x4 @loc1 (bytes 8..24), stride 24 — matches the CPU
/// oracle's `stride >= 24` vertex arm (pos at 0/4, colour at 8/12/16/20).
const SEED_POS2_COLOR: &str = r#"
    struct VOut { @builtin(position) pos: vec4<f32>, @location(0) color: vec4<f32> };
    @vertex fn vs_main(@location(0) p: vec2<f32>, @location(1) c: vec4<f32>) -> VOut {
        return VOut(vec4<f32>(p, 0.0, 1.0), c);
    }
    @fragment fn fs_main(v: VOut) -> @location(0) vec4<f32> { return v.color; }
"#;

/// pos = Float32x3 @loc0 (bytes 0..12), colour = Float32x4 @loc1 (bytes 12..28), stride 28 — matches the
/// CPU oracle's `stride >= 28` arm (pos+z at 0/4/8, colour at 12/16/20/24) for the depth-tested draws.
const SEED_POS3_COLOR: &str = r#"
    struct VOut { @builtin(position) pos: vec4<f32>, @location(0) color: vec4<f32> };
    @vertex fn vs_main(@location(0) p: vec3<f32>, @location(1) c: vec4<f32>) -> VOut {
        return VOut(vec4<f32>(p, 1.0), c);
    }
    @fragment fn fs_main(v: VOut) -> @location(0) vec4<f32> { return v.color; }
"#;

fn wgsl_to_spirv(src: &str) -> Vec<u32> {
    let module = naga::front::wgsl::parse_str(src).expect("seed wgsl parses");
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("seed wgsl validates");
    naga::back::spv::write_vec(&module, &info, &naga::back::spv::Options::default(), None)
        .expect("emit spir-v")
}

/// `vfmt` packs a vertex-attribute format the way the GL driver's `vertex_format_wire` does:
/// `comps | (kind<<8) | (normalized<<16)`; kind 0 = f32.
fn vfmt(comps: u32, kind: u32, normalized: bool) -> u32 {
    comps | (kind << 8) | ((normalized as u32) << 16)
}

// =================================================================================================
// program model
// =================================================================================================

/// What target to read back and compare after running a program.
#[derive(Clone)]
enum Read {
    /// Tight level-0 colour plane of a texture (`len` bytes).
    Tex { id: u32, len: usize },
    /// A buffer slice.
    Buf { id: u32, offset: u64, len: usize },
}

/// One minted differential program.
struct Prog {
    seed: u64,
    category: &'static str,
    /// Encoder-op names this program exercises (for the coverage report).
    ops: Vec<&'static str>,
    cmds: Vec<Cmd>,
    read: Read,
    /// Per-channel byte tolerance (0 = exact).
    tol: i16,
    /// Optional kernel to register (compute programs) under the given shader id.
    kernel: Option<(u32, KernelProgram)>,
}

// =================================================================================================
// generators — each returns a Prog whose bytes/sizes/colours are pure functions of `seed`
// =================================================================================================

/// (0) Render-pass `LoadOp::Clear` on a colour target — no draw. Both backends fill every texel with the
/// exact packed clear colour. EXACT.

fn pos2_color_layout() -> VertexLayout {
    VertexLayout {
        stride: 24,
        step_mode: 0,
        attrs: vec![
            VertexAttr {
                location: 0,
                format: vfmt(2, 0, false),
                offset: 0,
            },
            VertexAttr {
                location: 1,
                format: vfmt(4, 0, false),
                offset: 8,
            },
        ],
    }
}

#[path = "differential/color.rs"]
mod color;
#[path = "differential/compute.rs"]
mod compute;
#[path = "differential/draw.rs"]
mod draw;
#[path = "differential/msaa.rs"]
mod msaa;
#[path = "differential/render.rs"]
mod render;
#[path = "differential/runners.rs"]
mod runners;
#[path = "differential/state.rs"]
mod state;
#[path = "differential/stencil.rs"]
mod stencil;
#[path = "differential/test.rs"]
mod test;
#[path = "differential/transfer.rs"]
mod transfer;

use color::*;
use compute::*;
use draw::*;
use render::*;
use state::*;
use stencil::*;
use transfer::*;

const GENERATORS: &[fn(u64) -> Prog] = &[
    gen_clear,
    gen_clear_rect,
    gen_copy_b2t,
    gen_copy_t2t,
    gen_copy_b2b,
    gen_fill_buffer,
    gen_blit_nearest,
    gen_blit_linear,
    gen_draw_flat,
    gen_draw_gradient,
    gen_draw_depth,
    gen_draw_blend,
    gen_draw_mask_rgb,
    gen_draw_mask_alpha,
    gen_cull_ccw_back,
    gen_cull_ccw_front,
    gen_cull_cw_front,
    gen_cull_rev_ccw_front,
    gen_compute_iota,
    gen_clear_srgb,
    gen_draw_srgb,
    gen_stencil_equal,
    gen_stencil_greater,
];

// =================================================================================================
// backend runners — the SAME program bytes, two executors
// =================================================================================================
