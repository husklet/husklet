//! DIFFERENTIAL fuzzer: the `hl_wip-gpu` CPU oracle vs the `hl_wip-gpu-wgpu` `WgpuExecutor`.
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
//! ## Excluded op surface (oracle-unmodeled — logged, never silently skipped)
//!   * STENCIL (`Enc::SetStencilReference` + the pipeline `DepthState` stencil face/masks): the CPU oracle
//!     does NOT model the stencil test — `raster.rs` has no stencil arm and `executor.rs`'s
//!     `SetStencilReference` is an explicit no-op (validate arm ~line 747). Comparing stencil would compare
//!     the wgpu result against an un-modeled oracle, so no program here sets stencil state. Logged below.
//!   * `Enc::ResolveTexture` (multisample averaging): the wgpu backend does not advertise it (see the
//!     capability-honesty test), so a program using it can't run on wgpu at all. Excluded + logged.
//!   * sRGB colour targets: the oracle blends in linear light and encodes sRGB around the blend; matching
//!     that against hardware sRGB blend adds an encode/decode rounding surface orthogonal to this fuzzer's
//!     goal. All targets here are LINEAR `Rgba8Unorm`. Logged.
//!
//! If no wgpu adapter is reachable (no lavapipe/Vulkan ICD) the whole test skips, like the rest of the suite.

use std::collections::BTreeSet;

use hl_gpu::protocol::model::capability::shader_payload;
use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BlendState, BufferDesc, ColorAttachment, ColorTargetState,
    ComputePipelineDesc, DepthAttachment, DepthState, Extent3d, Origin3d, RenderPipelineDesc, ShaderRef,
    TextureDesc, TextureSubresource, VertexAttr, VertexLayout,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, compare, texture_usage, Filter, IndexFormat, LoadOp, TextureDim, TextureFormat, Topology,
};
use hl_gpu::protocol::model::kernel::{
    gty, Inst, KernelProgram, Op, Param, CMP_GE, KERNEL_MAGIC, SR_CTAID_X, SR_NTID_X, SR_TID_X,
};
use hl_gpu::{
    BufferId, Cmd, CommandBuffer, CpuExecutor, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session,
    ShaderPayloadKind, TextureId,
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
    TextureDesc {
        width: w,
        height: h,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm,
        usage: RT,
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
    BufferDesc { size, usage, label: String::new() }
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
    [chan(seed, 0) as f32 / 255.0, chan(seed, 1) as f32 / 255.0, chan(seed, 2) as f32 / 255.0, 1.0]
}

// The fullscreen triangle: every pixel centre is strictly interior on any correct rasterizer.
const FS_TRI: [(f32, f32); 3] = [(-1.0, -1.0), (3.0, -1.0), (-1.0, 3.0)];

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
fn gen_clear(seed: u64) -> Prog {
    let w = 3 + (seed % 6) as u32; // 3..=8
    let h = 2 + (seed % 5) as u32; // 2..=6
    let c = fcolor_opaque(seed);
    let cmds = vec![
        Cmd::CreateTexture(1, tex(w, h)),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: c, store: true }],
                    depth: None,
                },
                Enc::EndRenderPass,
            ],
            signal: None,
        }),
    ];
    Prog {
        seed,
        category: "clear",
        ops: vec!["BeginRenderPass", "EndRenderPass"],
        cmds,
        read: Read::Tex { id: 1, len: (w * h * 4) as usize },
        tol: 0,
        kernel: None,
    }
}

/// (1) Upload a base plane, then `ClearRect` a sub-rectangle (clamped). Both write the same clamped rect
/// with the same packed colour. EXACT.
fn gen_clear_rect(seed: u64) -> Prog {
    let w = 4 + (seed % 5) as u32; // 4..=8
    let h = 4 + (seed % 4) as u32; // 4..=7
    let base: Vec<u8> = (0..w * h).flat_map(|i| texel(seed ^ i as u64)).collect();
    let rx = (seed % w as u64) as u32;
    let ry = (seed % h as u64) as u32;
    let rw = 1 + (seed % 3) as u32;
    let rh = 1 + (seed % 3) as u32;
    let c = fcolor_opaque(seed.wrapping_add(9));
    let cmds = vec![
        Cmd::CreateTexture(1, tex(w, h)),
        Cmd::CreateBuffer(1, buf(base.len() as u64, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
        Cmd::WriteBuffer { id: 1, offset: 0, data: base },
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::CopyBufferToTexture { src: 1, src_offset: 0, bytes_per_row: w * 4, dst: 1, mip: 0, width: w, height: h },
                Enc::ClearRect { texture: 1, x: rx, y: ry, w: rw, h: rh, color: c },
            ],
            signal: None,
        }),
    ];
    Prog {
        seed,
        category: "clear_rect",
        ops: vec!["CopyBufferToTexture", "ClearRect"],
        cmds,
        read: Read::Tex { id: 1, len: (w * h * 4) as usize },
        tol: 0,
        kernel: None,
    }
}

/// (2) `CopyBufferToTexture` of a deterministic plane (varying size), read back tight. EXACT.
fn gen_copy_b2t(seed: u64) -> Prog {
    let w = 1 + (seed % 7) as u32; // 1..=7
    let h = 1 + (seed % 5) as u32; // 1..=5
    let data: Vec<u8> = (0..w * h).flat_map(|i| texel(seed.wrapping_add(i as u64 * 3))).collect();
    let cmds = vec![
        Cmd::CreateTexture(1, tex(w, h)),
        Cmd::CreateBuffer(1, buf(data.len() as u64, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
        Cmd::WriteBuffer { id: 1, offset: 0, data },
        Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::CopyBufferToTexture { src: 1, src_offset: 0, bytes_per_row: w * 4, dst: 1, mip: 0, width: w, height: h }],
            signal: None,
        }),
    ];
    Prog {
        seed,
        category: "copy_b2t",
        ops: vec!["CopyBufferToTexture"],
        cmds,
        read: Read::Tex { id: 1, len: (w * h * 4) as usize },
        tol: 0,
        kernel: None,
    }
}

/// (3) Seed a source texture, `CopyTextureToTexture` a sub-region into a fresh dest. EXACT.
fn gen_copy_t2t(seed: u64) -> Prog {
    let w = 4u32;
    let h = 4u32;
    let src: Vec<u8> = (0..w * h).flat_map(|i| texel(seed.wrapping_add(i as u64))).collect();
    let ew = 1 + (seed % 3) as u32; // 1..=3
    let eh = 1 + (seed % 3) as u32;
    let sx = seed as u32 % (w - ew + 1);
    let sy = (seed / 2) as u32 % (h - eh + 1);
    let dx = (seed / 3) as u32 % (w - ew + 1);
    let dy = (seed / 5) as u32 % (h - eh + 1);
    let cmds = vec![
        Cmd::CreateTexture(1, tex(w, h)),
        Cmd::CreateTexture(2, tex(w, h)),
        Cmd::CreateBuffer(1, buf(src.len() as u64, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
        Cmd::WriteBuffer { id: 1, offset: 0, data: src },
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::CopyBufferToTexture { src: 1, src_offset: 0, bytes_per_row: w * 4, dst: 1, mip: 0, width: w, height: h },
                Enc::CopyTextureToTexture {
                    src: 1,
                    src_sub: TextureSubresource::base(),
                    src_origin: Origin3d { x: sx, y: sy, z: 0 },
                    dst: 2,
                    dst_sub: TextureSubresource::base(),
                    dst_origin: Origin3d { x: dx, y: dy, z: 0 },
                    extent: Extent3d { width: ew, height: eh, depth: 1 },
                },
            ],
            signal: None,
        }),
    ];
    Prog {
        seed,
        category: "copy_t2t",
        ops: vec!["CopyBufferToTexture", "CopyTextureToTexture"],
        cmds,
        read: Read::Tex { id: 2, len: (w * h * 4) as usize },
        tol: 0,
        kernel: None,
    }
}

/// (4) `CopyBufferToBuffer`: write a src pattern, copy a sub-range into a dst, read the dst back. EXACT.
fn gen_copy_b2b(seed: u64) -> Prog {
    let n = 16u64;
    let src: Vec<u8> = (0..n).map(|i| chan(seed, i)).collect();
    let size = 4 + (seed % 9); // 4..=12
    let so = seed % (n - size + 1);
    let doo = (seed / 2) % (n - size + 1);
    let cmds = vec![
        Cmd::CreateBuffer(1, buf(n, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
        Cmd::CreateBuffer(2, buf(n, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
        Cmd::WriteBuffer { id: 1, offset: 0, data: src },
        Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::CopyBufferToBuffer { src: 1, src_offset: so, dst: 2, dst_offset: doo, size }],
            signal: None,
        }),
    ];
    Prog {
        seed,
        category: "copy_b2b",
        ops: vec!["CopyBufferToBuffer"],
        cmds,
        read: Read::Buf { id: 2, offset: 0, len: n as usize },
        tol: 0,
        kernel: None,
    }
}

/// (5) `FillBuffer`: write a base pattern, then memset a sub-range with a repeating 4-byte pattern. EXACT.
fn gen_fill_buffer(seed: u64) -> Prog {
    let n = 16u64;
    let base: Vec<u8> = (0..n).map(|i| chan(seed.wrapping_add(1), i)).collect();
    let value = (seed.wrapping_mul(2654435761) & 0xFFFF_FFFF) as u32;
    let size = 4 + (seed % 9); // 4..=12 (may be non-multiple of 4 → partial tail pattern, both tile it)
    let off = seed % (n - size + 1);
    let cmds = vec![
        Cmd::CreateBuffer(1, buf(n, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
        Cmd::WriteBuffer { id: 1, offset: 0, data: base },
        Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::FillBuffer { buffer: 1, offset: off, size, value }],
            signal: None,
        }),
    ];
    Prog {
        seed,
        category: "fill_buffer",
        ops: vec!["FillBuffer"],
        cmds,
        read: Read::Buf { id: 1, offset: 0, len: n as usize },
        tol: 0,
        kernel: None,
    }
}

/// (6) `BlitTexture` NEAREST integer upscale (NxN → kN×kN). Point sampling of an integer upscale is pure
/// texel replication on both backends → EXACT.
fn gen_blit_nearest(seed: u64) -> Prog {
    let n = 2 + (seed % 3) as u32; // 2..=4
    let k = 2 + (seed % 3) as u32; // 2..=4
    let dw = n * k;
    let dh = n * k;
    let src: Vec<u8> = (0..n * n).flat_map(|i| texel(seed.wrapping_add(i as u64 * 7))).collect();
    let cmds = blit_cmds(&src, n, n, dw, dh, Filter::Nearest);
    Prog {
        seed,
        category: "blit_nearest",
        ops: vec!["CopyBufferToTexture", "ClearRect", "BlitTexture"],
        cmds,
        read: Read::Tex { id: 2, len: (dw * dh * 4) as usize },
        tol: 0,
        kernel: None,
    }
}

/// (7) `BlitTexture` LINEAR upscale. Bilinear filtering agrees to a few unorm steps (pixel-centre +
/// clamp-to-edge, but not bit-identical between `sample_bilinear` and the hardware sampler). ±3.
fn gen_blit_linear(seed: u64) -> Prog {
    let n = 2 + (seed % 2) as u32; // 2..=3
    let dw = 3 + (seed % 4) as u32; // 3..=6
    let dh = 3 + (seed % 3) as u32; // 3..=5
    let src: Vec<u8> = (0..n * n).flat_map(|i| texel(seed.wrapping_add(i as u64 * 11))).collect();
    let cmds = blit_cmds(&src, n, n, dw, dh, Filter::Linear);
    Prog {
        seed,
        category: "blit_linear",
        ops: vec!["CopyBufferToTexture", "ClearRect", "BlitTexture"],
        cmds,
        read: Read::Tex { id: 2, len: (dw * dh * 4) as usize },
        tol: 3,
        kernel: None,
    }
}

/// Shared blit program body: upload `src` (tight) into tex 1, pre-clear tex 2 (dst) opaque black, then blit
/// the full source extent into the full destination extent with `filter`.
fn blit_cmds(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32, filter: Filter) -> Vec<Cmd> {
    vec![
        Cmd::CreateTexture(1, tex(sw, sh)),
        Cmd::CreateTexture(2, tex(dw, dh)),
        Cmd::CreateBuffer(1, buf(src.len() as u64, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
        Cmd::WriteBuffer { id: 1, offset: 0, data: src.to_vec() },
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::CopyBufferToTexture { src: 1, src_offset: 0, bytes_per_row: sw * 4, dst: 1, mip: 0, width: sw, height: sh },
                Enc::ClearRect { texture: 2, x: 0, y: 0, w: dw, h: dh, color: [0.0, 0.0, 0.0, 1.0] },
                Enc::BlitTexture {
                    src: 1,
                    src_sub: TextureSubresource::base(),
                    src_origin: Origin3d::default(),
                    src_extent: Extent3d { width: sw, height: sh, depth: 1 },
                    dst: 2,
                    dst_sub: TextureSubresource::base(),
                    dst_origin: Origin3d::default(),
                    dst_extent: Extent3d { width: dw, height: dh, depth: 1 },
                    filter,
                },
            ],
            signal: None,
        }),
    ]
}

/// (8) FLAT opaque draw: a fullscreen triangle, all three vertices the same opaque colour, blend disabled
/// (replace). Full coverage + identical unorm rounding of a constant → EXACT (±1 guard).
fn gen_draw_flat(seed: u64) -> Prog {
    let c = fcolor_opaque(seed);
    let vbytes: Vec<u8> = FS_TRI.iter().flat_map(|(x, y)| le_f32(&[*x, *y, c[0], c[1], c[2], c[3]])).collect();
    let w = 4 + (seed % 5) as u32; // 4..=8
    let h = 4 + (seed % 4) as u32;
    let spirv = wgsl_to_spirv(SEED_POS2_COLOR);
    let layout = VertexLayout {
        stride: 24,
        step_mode: 0,
        attrs: vec![
            VertexAttr { location: 0, format: vfmt(2, 0, false), offset: 0 },
            VertexAttr { location: 1, format: vfmt(4, 0, false), offset: 8 },
        ],
    };
    let cmds = draw_cmds(w, h, spirv, layout, vbytes, None, None);
    Prog {
        seed,
        category: "draw_flat",
        ops: vec!["BeginRenderPass", "SetPipeline", "SetVertexBuffer", "Draw", "EndRenderPass"],
        cmds,
        read: Read::Tex { id: 1, len: (w * h * 4) as usize },
        tol: 1,
        kernel: None,
    }
}

/// (9) GRADIENT draw: a fullscreen triangle with three DISTINCT vertex colours. Both backends
/// barycentric-interpolate in f32 then quantise; ±2 for last-ULP interpolation + rounding-rule differences.
fn gen_draw_gradient(seed: u64) -> Prog {
    let ca = fcolor_opaque(seed);
    let cb = fcolor_opaque(seed.wrapping_add(5));
    let cc = fcolor_opaque(seed.wrapping_add(11));
    let cols = [ca, cb, cc];
    let vbytes: Vec<u8> = FS_TRI
        .iter()
        .zip(cols)
        .flat_map(|((x, y), c)| le_f32(&[*x, *y, c[0], c[1], c[2], c[3]]))
        .collect();
    let w = 4 + (seed % 5) as u32;
    let h = 4 + (seed % 4) as u32;
    let spirv = wgsl_to_spirv(SEED_POS2_COLOR);
    let layout = VertexLayout {
        stride: 24,
        step_mode: 0,
        attrs: vec![
            VertexAttr { location: 0, format: vfmt(2, 0, false), offset: 0 },
            VertexAttr { location: 1, format: vfmt(4, 0, false), offset: 8 },
        ],
    };
    let cmds = draw_cmds(w, h, spirv, layout, vbytes, None, None);
    Prog {
        seed,
        category: "draw_gradient",
        ops: vec!["BeginRenderPass", "SetPipeline", "SetVertexBuffer", "Draw", "EndRenderPass"],
        cmds,
        read: Read::Tex { id: 1, len: (w * h * 4) as usize },
        tol: 2,
        kernel: None,
    }
}

/// Build a single-draw render program: clear to `[0,0,0,1]`, bind one pipeline + vertex buffer (id 1), draw
/// the 3-vertex fullscreen triangle. `blend`/`depth` opt into a blended target / a depth attachment (id 2).
fn draw_cmds(
    w: u32,
    h: u32,
    spirv: Vec<u32>,
    layout: VertexLayout,
    vbytes: Vec<u8>,
    blend: Option<BlendState>,
    depth: Option<DepthState>,
) -> Vec<Cmd> {
    let depth_att = depth.as_ref().map(|_| DepthAttachment {
        texture: 2,
        load: LoadOp::Clear,
        clear_depth: 1.0,
        clear_stencil: 0,
    });
    let mut cmds = vec![
        Cmd::CreateTexture(1, tex(w, h)),
        Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::SpirV, spirv },
        Cmd::CreateBuffer(1, buf(vbytes.len() as u64, buffer_usage::VERTEX | buffer_usage::COPY_DST)),
        Cmd::WriteBuffer { id: 1, offset: 0, data: vbytes },
    ];
    if depth.is_some() {
        cmds.push(Cmd::CreateTexture(2, depth_tex(w, h)));
    }
    cmds.push(Cmd::CreateRenderPipeline(
        1,
        RenderPipelineDesc {
            vertex: ShaderRef { module: 1, entry: "vs_main".into() },
            fragment: Some(ShaderRef { module: 1, entry: "fs_main".into() }),
            vertex_buffers: vec![layout],
            color_targets: vec![ColorTargetState { format: TextureFormat::Rgba8Unorm, blend, write_mask: 0xF }],
            depth,
            topology: Topology::TriangleList,
            cull: 0,
            front_face: 0,
            sample_count: 1,
            label: String::new(),
        },
    ));
    cmds.push(Cmd::Submit(CommandBuffer {
        encoder: vec![
            Enc::BeginRenderPass {
                color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.0, 0.0, 0.0, 1.0], store: true }],
                depth: depth_att,
            },
            Enc::SetPipeline(1),
            Enc::SetVertexBuffer { slot: 0, buffer: 1, offset: 0 },
            Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
            Enc::EndRenderPass,
        ],
        signal: None,
    }));
    cmds
}

/// (10) DEPTH-TESTED draw: two fullscreen triangles at different constant depths through one pipeline
/// (`LESS` + depth-write). The nearer fragment wins on both backends regardless of draw order → the winning
/// flat colour is EXACT (±1). We alternate which of the two is nearer by seed parity.
fn gen_draw_depth(seed: u64) -> Prog {
    let w = 4 + (seed % 4) as u32;
    let h = 4 + (seed % 3) as u32;
    let near_first = seed % 2 == 0;
    let (za, ca, zb, cb) = if near_first {
        (0.25f32, fcolor_opaque(seed), 0.75f32, fcolor_opaque(seed.wrapping_add(7)))
    } else {
        (0.75f32, fcolor_opaque(seed.wrapping_add(7)), 0.25f32, fcolor_opaque(seed))
    };
    let vbuf = |z: f32, c: [f32; 4]| -> Vec<u8> {
        FS_TRI.iter().flat_map(|(x, y)| le_f32(&[*x, *y, z, c[0], c[1], c[2], c[3]])).collect::<Vec<u8>>()
    };
    let ba = vbuf(za, ca);
    let bb = vbuf(zb, cb);
    let spirv = wgsl_to_spirv(SEED_POS3_COLOR);
    let layout = VertexLayout {
        stride: 28,
        step_mode: 0,
        attrs: vec![
            VertexAttr { location: 0, format: vfmt(3, 0, false), offset: 0 },
            VertexAttr { location: 1, format: vfmt(4, 0, false), offset: 12 },
        ],
    };
    let cmds = vec![
        Cmd::CreateTexture(1, tex(w, h)),
        Cmd::CreateTexture(2, depth_tex(w, h)),
        Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::SpirV, spirv },
        Cmd::CreateBuffer(1, buf(ba.len() as u64, buffer_usage::VERTEX | buffer_usage::COPY_DST)),
        Cmd::CreateBuffer(2, buf(bb.len() as u64, buffer_usage::VERTEX | buffer_usage::COPY_DST)),
        Cmd::WriteBuffer { id: 1, offset: 0, data: ba },
        Cmd::WriteBuffer { id: 2, offset: 0, data: bb },
        Cmd::CreateRenderPipeline(
            1,
            RenderPipelineDesc {
                vertex: ShaderRef { module: 1, entry: "vs_main".into() },
                fragment: Some(ShaderRef { module: 1, entry: "fs_main".into() }),
                vertex_buffers: vec![layout],
                color_targets: vec![ColorTargetState { format: TextureFormat::Rgba8Unorm, blend: None, write_mask: 0xF }],
                depth: Some(DepthState::depth_only(TextureFormat::Depth32Float, true, compare::LESS)),
                topology: Topology::TriangleList,
                cull: 0,
                front_face: 0,
                sample_count: 1,
                label: String::new(),
            },
        ),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.0, 0.0, 0.0, 1.0], store: true }],
                    depth: Some(DepthAttachment { texture: 2, load: LoadOp::Clear, clear_depth: 1.0, clear_stencil: 0 }),
                },
                Enc::SetPipeline(1),
                Enc::SetVertexBuffer { slot: 0, buffer: 1, offset: 0 },
                Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
                Enc::SetVertexBuffer { slot: 0, buffer: 2, offset: 0 },
                Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
                Enc::EndRenderPass,
            ],
            signal: None,
        }),
    ];
    Prog {
        seed,
        category: "draw_depth",
        ops: vec!["BeginRenderPass(depth)", "SetPipeline", "SetVertexBuffer", "Draw", "EndRenderPass"],
        cmds,
        read: Read::Tex { id: 1, len: (w * h * 4) as usize },
        tol: 1,
        kernel: None,
    }
}

/// (11) BLENDED draw: an opaque fullscreen background (blend disabled → replace) then a translucent
/// fullscreen foreground whose pipeline blend is EXACTLY the equation the CPU oracle hardcodes —
/// colour = `(SrcAlpha, OneMinusSrcAlpha, Add)`, alpha = `(One, OneMinusSrcAlpha, Add)` — so
/// `out = fg*a + bg*(1-a)` on colour and `a + bg_a*(1-a)` on alpha match on both backends. ±2.
fn gen_draw_blend(seed: u64) -> Prog {
    let w = 4 + (seed % 4) as u32;
    let h = 4 + (seed % 3) as u32;
    let bg = fcolor_opaque(seed);
    let a = [0.25f32, 0.5, 0.75][(seed % 3) as usize];
    let fg = [chan(seed, 5) as f32 / 255.0, chan(seed, 6) as f32 / 255.0, chan(seed, 7) as f32 / 255.0, a];
    let spirv = wgsl_to_spirv(SEED_POS2_COLOR);
    let layout = || VertexLayout {
        stride: 24,
        step_mode: 0,
        attrs: vec![
            VertexAttr { location: 0, format: vfmt(2, 0, false), offset: 0 },
            VertexAttr { location: 1, format: vfmt(4, 0, false), offset: 8 },
        ],
    };
    let vbuf = |c: [f32; 4]| -> Vec<u8> {
        FS_TRI.iter().flat_map(|(x, y)| le_f32(&[*x, *y, c[0], c[1], c[2], c[3]])).collect::<Vec<u8>>()
    };
    // The CPU oracle's straight-alpha-over, expressed as a protocol BlendState (wire factors: 1=One,
    // 4=SrcAlpha, 5=OneMinusSrcAlpha; op 0=Add).
    let over = BlendState { src_color: 4, dst_color: 5, op_color: 0, src_alpha: 1, dst_alpha: 5, op_alpha: 0 };
    let opaque_target = ColorTargetState { format: TextureFormat::Rgba8Unorm, blend: None, write_mask: 0xF };
    let blend_target = ColorTargetState { format: TextureFormat::Rgba8Unorm, blend: Some(over), write_mask: 0xF };
    let pipe = |id: u32, target: ColorTargetState| {
        Cmd::CreateRenderPipeline(
            id,
            RenderPipelineDesc {
                vertex: ShaderRef { module: 1, entry: "vs_main".into() },
                fragment: Some(ShaderRef { module: 1, entry: "fs_main".into() }),
                vertex_buffers: vec![layout()],
                color_targets: vec![target],
                depth: None,
                topology: Topology::TriangleList,
                cull: 0,
                front_face: 0,
                sample_count: 1,
                label: String::new(),
            },
        )
    };
    let cmds = vec![
        Cmd::CreateTexture(1, tex(w, h)),
        Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::SpirV, spirv },
        Cmd::CreateBuffer(1, buf(24 * 3, buffer_usage::VERTEX | buffer_usage::COPY_DST)),
        Cmd::CreateBuffer(2, buf(24 * 3, buffer_usage::VERTEX | buffer_usage::COPY_DST)),
        Cmd::WriteBuffer { id: 1, offset: 0, data: vbuf(bg) },
        Cmd::WriteBuffer { id: 2, offset: 0, data: vbuf(fg) },
        pipe(1, opaque_target),
        pipe(2, blend_target),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.0, 0.0, 0.0, 1.0], store: true }],
                    depth: None,
                },
                Enc::SetPipeline(1),
                Enc::SetVertexBuffer { slot: 0, buffer: 1, offset: 0 },
                Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
                Enc::SetPipeline(2),
                Enc::SetVertexBuffer { slot: 0, buffer: 2, offset: 0 },
                Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
                Enc::EndRenderPass,
            ],
            signal: None,
        }),
    ];
    Prog {
        seed,
        category: "draw_blend",
        ops: vec!["BeginRenderPass", "SetPipeline", "SetVertexBuffer", "Draw(blend)", "EndRenderPass"],
        cmds,
        read: Read::Tex { id: 1, len: (w * h * 4) as usize },
        tol: 2,
        kernel: None,
    }
}

/// (12) COMPUTE `iota`: `out[gid] = gid` for `gid < n`, driven by the SAME neutral kernel-IR
/// (`KernelProgram`) on both backends — the CPU interpreter and the wgpu WGSL-lowered compute. EXACT.
fn gen_compute_iota(seed: u64) -> Prog {
    let n = 8 + (seed % 25) as u32; // 8..=32
    let mut param = vec![0u8; 12];
    param[8..12].copy_from_slice(&n.to_le_bytes());
    let groups = n.div_ceil(8);
    let cmds = vec![
        Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::PtxKernel, spirv: vec![KERNEL_MAGIC, 0] },
        Cmd::CreateComputePipeline(1, ComputePipelineDesc { compute: ShaderRef { module: 1, entry: "iota".into() }, label: String::new() }),
        Cmd::CreateBuffer(1, buf(12, buffer_usage::STORAGE)),
        Cmd::CreateBuffer(2, buf((n * 4) as u64, buffer_usage::STORAGE | buffer_usage::COPY_SRC)),
        Cmd::WriteBuffer { id: 1, offset: 0, data: param },
        Cmd::CreateBindGroup(
            1,
            BindGroupDesc {
                set: 0,
                entries: vec![
                    BindEntry { binding: 0, resource: BindResource::Buffer { id: 1, offset: 0, size: 12 } },
                    BindEntry { binding: 1, resource: BindResource::Buffer { id: 2, offset: 0, size: (n * 4) as u64 } },
                ],
            },
        ),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::BeginComputePass,
                Enc::SetPipeline(1),
                Enc::SetBindGroup { index: 0, group: 1 },
                Enc::Dispatch { x: groups, y: 1, z: 1 },
                Enc::EndComputePass,
            ],
            signal: None,
        }),
    ];
    Prog {
        seed,
        category: "compute_iota",
        ops: vec!["BeginComputePass", "Dispatch", "EndComputePass"],
        cmds,
        read: Read::Buf { id: 2, offset: 0, len: (n * 4) as usize },
        tol: 0,
        kernel: Some((1, iota_program())),
    }
}

/// `out[gid] = gid` for `gid < n`, `gid = ctaid.x*ntid.x + tid.x` — the same neutral kernel the wgpu
/// coverage suite uses, so both backends receive an identical program.
fn iota_program() -> KernelProgram {
    KernelProgram {
        entry: "iota".into(),
        block: [8, 1, 1],
        params: vec![
            Param { width: 8, offset: 0, is_ptr: true, region: 0 },
            Param { width: 4, offset: 8, is_ptr: false, region: 0 },
        ],
        param_bytes: 12,
        num_regions: 1,
        shared_bytes: 0,
        reg_count: 10,
        insts: vec![
            Inst::LdParam { d: 0, param: 0 },
            Inst::LdParam { d: 1, param: 1 },
            Inst::MovSReg { d: 2, sreg: SR_NTID_X },
            Inst::MovSReg { d: 3, sreg: SR_CTAID_X },
            Inst::MovSReg { d: 4, sreg: SR_TID_X },
            Inst::IMad { d: 5, a: Op::Reg(3), b: Op::Reg(2), c: Op::Reg(4) },
            Inst::Setp { d: 6, a: Op::Reg(5), b: Op::Reg(1), cmp: CMP_GE, unsigned: true },
            Inst::Bra { target: 12, pred: Some((6, false)) },
            Inst::Cvta { d: 7, s: 0 },
            Inst::IMul { d: 8, a: Op::Reg(5), b: Op::ImmI(4), wide: true, unsigned: false },
            Inst::IAdd { d: 9, a: Op::Reg(7), b: Op::Reg(8), wide: true },
            Inst::StGlobal { addr: 9, off: 0, src: Op::Reg(5), ty: gty::U32 },
            Inst::Ret,
        ],
    }
}

// The generator table.
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
    gen_compute_iota,
];

// =================================================================================================
// backend runners — the SAME program bytes, two executors
// =================================================================================================

/// A wgpu session with byte-addressable copy alignment (matches the rest of the suite).
fn wgpu_session(exec: &WgpuExecutor) -> Session {
    let caps = exec.capabilities();
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    Session::new(limits, GlobalLedger::unbounded(), Box::new(FakeClock::new(0)))
}

/// A CPU-oracle session. The oracle rasterises fixed-function and treats SPIR-V/GLSL modules as opaque
/// handles, so we widen its advertised `shader_payloads` to admit them past the runtime `validate` gate —
/// this changes nothing the oracle computes, it just lets the identical program reach the executor.
fn cpu_session(exec: &CpuExecutor) -> Session {
    let mut caps = exec.capabilities();
    caps.shader_payloads |= shader_payload::SPIRV | shader_payload::GLSL;
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    Session::new(limits, GlobalLedger::unbounded(), Box::new(FakeClock::new(0)))
}

fn run_wgpu(exec: &mut WgpuExecutor, prog: &Prog) -> hl_gpu::Result<Vec<u8>> {
    if let Some((id, k)) = &prog.kernel {
        exec.define_kernel(*id, k.clone());
    }
    let mut s = wgpu_session(exec);
    hl_gpu::runtime::submit(&mut s, exec, 0, &prog.cmds)?;
    match prog.read {
        Read::Tex { id, .. } => exec.read_texture(&s.resources, id),
        Read::Buf { id, offset, len } => exec.read_buffer(&s.resources, BufferId(id), offset, len),
    }
}

fn run_cpu(prog: &Prog) -> hl_gpu::Result<Vec<u8>> {
    let mut cpu = CpuExecutor::new();
    if let Some((id, k)) = &prog.kernel {
        cpu.define_kernel(*id, k.clone());
    }
    let mut s = cpu_session(&cpu);
    hl_gpu::runtime::submit(&mut s, &mut cpu, 0, &prog.cmds)?;
    match prog.read {
        Read::Tex { id, len } => {
            let mut out = vec![0u8; len];
            cpu.read_texture(&s.resources, TextureId(id), &mut out)?;
            Ok(out)
        }
        Read::Buf { id, offset, len } => GpuExecutor::read_buffer(&cpu, &s.resources, BufferId(id), offset, len),
    }
}

/// Compare two readback planes per byte within `tol`; on the first out-of-tolerance byte, return a
/// minimised description (the offending index + both values + the max observed per-byte delta).
fn diff(cpu: &[u8], gpu: &[u8], tol: i16) -> Option<String> {
    if cpu.len() != gpu.len() {
        return Some(format!("length mismatch: cpu={} gpu={}", cpu.len(), gpu.len()));
    }
    let mut worst = 0i16;
    let mut first_bad: Option<usize> = None;
    for i in 0..cpu.len() {
        let d = (cpu[i] as i16 - gpu[i] as i16).abs();
        if d > worst {
            worst = d;
        }
        if d > tol && first_bad.is_none() {
            first_bad = Some(i);
        }
    }
    first_bad.map(|i| {
        format!("byte {i} (texel {}, chan {}) cpu={} gpu={} (tol {tol}, worst delta {worst})", i / 4, i % 4, cpu[i], gpu[i])
    })
}

// =================================================================================================
// the differential test
// =================================================================================================

#[test]
fn differential_cpu_oracle_vs_wgpu() {
    const N: u64 = 130; // 50..200 seeded programs; every generator gets 10 seeds

    let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        Err(_) => {
            eprintln!("differential: no wgpu adapter reachable (no lavapipe/Vulkan ICD) — skipping");
            return;
        }
    };

    let mut agreed = 0u32;
    let mut per_category: std::collections::BTreeMap<&'static str, (u32, u32)> = Default::default(); // (agreed, total)
    let mut ops_covered: BTreeSet<&'static str> = BTreeSet::new();
    let mut divergences: Vec<String> = Vec::new();

    for i in 0..N {
        let gen = GENERATORS[(i as usize) % GENERATORS.len()];
        let seed = i; // pure index seeding
        let prog = gen(seed);
        for op in &prog.ops {
            ops_covered.insert(op);
        }
        let entry = per_category.entry(prog.category).or_insert((0, 0));
        entry.1 += 1;

        let cpu_out = run_cpu(&prog);
        let gpu_out = run_wgpu(&mut exec, &prog);

        match (cpu_out, gpu_out) {
            (Ok(c), Ok(g)) => match diff(&c, &g, prog.tol) {
                None => {
                    agreed += 1;
                    entry.0 += 1;
                }
                Some(desc) => divergences.push(format!(
                    "DIVERGENCE [{}] seed={} ({} bytes): {}",
                    prog.category, prog.seed, c.len(), desc
                )),
            },
            (Err(ce), Ok(_)) => divergences.push(format!(
                "DIVERGENCE [{}] seed={}: CPU oracle errored ({ce:?}) but wgpu ran",
                prog.category, prog.seed
            )),
            (Ok(_), Err(ge)) => divergences.push(format!(
                "DIVERGENCE [{}] seed={}: wgpu errored ({ge:?}) but CPU oracle ran",
                prog.category, prog.seed
            )),
            (Err(ce), Err(ge)) => {
                // Both refused the program. If they refused with the same error kind that is agreement (not
                // a pixel divergence); a different error kind is itself a divergence worth reporting.
                if std::mem::discriminant(&ce) == std::mem::discriminant(&ge) {
                    agreed += 1;
                    entry.0 += 1;
                } else {
                    divergences.push(format!(
                        "DIVERGENCE [{}] seed={}: both errored but differently: cpu={ce:?} gpu={ge:?}",
                        prog.category, prog.seed
                    ));
                }
            }
        }
    }

    // ---- summary -------------------------------------------------------------------------------
    let excluded = [
        "STENCIL (SetStencilReference + DepthState stencil faces/masks) — oracle has no stencil model \
         (raster.rs no stencil arm; executor.rs SetStencilReference is a no-op)",
        "ResolveTexture (multisample averaging) — not advertised by the wgpu backend",
        "sRGB colour targets — oracle blends/encodes in linear light; orthogonal rounding surface",
    ];
    println!("======================== DIFFERENTIAL SUMMARY ========================");
    println!("programs run: {N}   agreed: {agreed}   divergences: {}", divergences.len());
    println!("per-category (agreed/total):");
    for (cat, (a, t)) in &per_category {
        println!("    {cat:<16} {a}/{t}");
    }
    println!("encoder ops covered ({}): {:?}", ops_covered.len(), ops_covered);
    println!("excluded (oracle-unmodeled / unadvertised):");
    for e in &excluded {
        println!("    - {e}");
    }
    for d in &divergences {
        println!("  {d}");
    }
    println!("======================================================================");

    assert!(
        divergences.is_empty(),
        "the CPU oracle and the wgpu executor diverged on {} of {N} programs:\n{}",
        divergences.len(),
        divergences.join("\n")
    );
    assert_eq!(agreed, N as u32, "every program must agree across both backends");
    // Guard against an accidental empty/broken generator table hiding real coverage.
    assert!(ops_covered.len() >= 15, "expected broad op coverage, got {}", ops_covered.len());
}

// The index-buffer path (DrawIndexed / IndexFormat) shares the same fixed-function raster + shader path as
// Draw; it is exercised by the coverage suite and left out of the per-pixel fuzz to avoid edge-rule
// ambiguity on partial-coverage indexed geometry. Keep the import wired without a dedicated case.
#[allow(dead_code)]
const _: Option<IndexFormat> = None;
