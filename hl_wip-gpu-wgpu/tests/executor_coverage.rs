//! Adversarial + coverage suite for the wgpu executor, complementing the frozen `conformance.rs` mirror.
//!
//! Everything here drives the SAME runtime pipeline (validate → account → dispatch → execute) against a
//! `WgpuExecutor` on the headless software Vulkan device (lavapipe / `llvmpipe`), but pushes far past the
//! frozen suite: odd-width / multi-format texture readback repack, sub-region + tight (`bytes_per_row==0`)
//! copies, multi-workgroup + atomic compute, real vertex-buffer draws (multiple attributes, indexed,
//! per-instance step mode), viewport/scissor, a depth-tested draw, GLSL execution, and a wall of error /
//! capability-honesty paths that must return a clean `Err` (never panic) or match the advertisement.

use std::sync::{Mutex, MutexGuard, OnceLock};

use hl_gpu::protocol::model::capability::{shader_payload, PresentKind};
use hl_gpu::protocol::model::command::etag;
use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, ColorTargetState,
    ComputePipelineDesc, DepthAttachment, DepthState, Extent3d, Origin3d, RenderPipelineDesc,
    ShaderRef, TextureDesc, TextureSubresource, VertexAttr, VertexLayout,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, compare, texture_usage, Filter, LoadOp, TextureDim, TextureFormat, Topology,
};
use hl_gpu::protocol::model::kernel::{
    glsl_stage, gty, GlslDescriptor, Inst, KernelProgram, Op, Param, ATOM_ADD, CMP_GE, KERNEL_MAGIC,
    SPIRV_MAGIC, SR_CTAID_X, SR_NTID_X, SR_TID_X,
};
use hl_gpu::{
    BufferId, Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session,
    ShaderPayloadKind,
};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

// -------------------------------------------------------------------------------------------------
// shared device + runtime-pipeline harness (own EXEC — a test binary is its own process)
// -------------------------------------------------------------------------------------------------

static EXEC: OnceLock<Mutex<WgpuExecutor>> = OnceLock::new();

fn exec() -> MutexGuard<'static, WgpuExecutor> {
    EXEC.get_or_init(|| {
        Mutex::new(
            WgpuExecutor::new(DeviceConfig::default())
                .expect("acquire a wgpu adapter (is a Vulkan ICD / lavapipe reachable?)"),
        )
    })
    .lock()
    .unwrap_or_else(|e| e.into_inner())
}

/// Run `cmds` through the whole runtime pipeline, returning the `Result` (so error paths can assert a
/// clean `Err` with no panic). Byte-addressable copy alignment (1) matches the oracle harness.
fn try_batch(exec: &mut WgpuExecutor, cmds: &[Cmd]) -> hl_gpu::Result<Session> {
    let caps = exec.capabilities();
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    let mut s = Session::new(limits, GlobalLedger::unbounded(), Box::new(FakeClock::new(0)));
    hl_gpu::runtime::submit(&mut s, exec, 0, cmds)?;
    Ok(s)
}

fn run_batch(exec: &mut WgpuExecutor, cmds: &[Cmd]) -> Session {
    try_batch(exec, cmds).expect("batch must run cleanly")
}

fn buf(size: u64, usage: u32) -> BufferDesc {
    BufferDesc { size, usage, label: String::new() }
}

fn tex(w: u32, h: u32, fmt: TextureFormat, usage: u32) -> TextureDesc {
    TextureDesc {
        width: w,
        height: h,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: fmt,
        usage,
        label: String::new(),
    }
}

const RT: u32 = texture_usage::RENDER_TARGET | texture_usage::COPY_SRC | texture_usage::COPY_DST;

/// Pack a vertex-attribute format the way the GL driver's `vertex_format_wire` does:
/// `comps | (kind<<8) | (normalized<<16)`. kinds: 0=f32 1=u8 2=i8 3=u16 4=i16 5=u32 6=i32 7=f16.
fn vfmt(comps: u32, kind: u32, normalized: bool) -> u32 {
    comps | (kind << 8) | ((normalized as u32) << 16)
}

/// Mint real SPIR-V (all entry points) from a WGSL seed via naga — the guest SPIR-V ABI round trip.
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

fn glsl_words(stage: u32, entry: &str, source: &str) -> Vec<u32> {
    GlslDescriptor { stage, entry: entry.into(), source: source.into() }.to_words()
}

fn kernel_words() -> Vec<u32> {
    vec![KERNEL_MAGIC, 0]
}

// =================================================================================================
// (1) COPY BUG REGRESSIONS — sub-region source stride + tight (bytes_per_row == 0)
// =================================================================================================

/// Seed a 2x2 RGBA8 texture with 4 distinct texels via a tight buffer→texture upload.
fn seed_2x2(id: u32, texels: [[u8; 4]; 4]) -> Vec<Cmd> {
    let mut data = Vec::new();
    for t in texels {
        data.extend_from_slice(&t);
    }
    vec![
        Cmd::CreateTexture(id, tex(2, 2, TextureFormat::Rgba8Unorm, RT)),
        Cmd::CreateBuffer(200, buf(16, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
        Cmd::WriteBuffer { id: 200, offset: 0, data },
        Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::CopyBufferToTexture {
                src: 200,
                src_offset: 0,
                bytes_per_row: 8, // tight: 2px * 4bpt
                dst: id,
                mip: 0,
                width: 2,
                height: 2,
            }],
            signal: None,
        }),
    ]
}

#[test]
fn copy_texture_to_buffer_subregion_uses_texture_stride() {
    // Left column (width=1, height=2) of a 2x2 texture must read texel (0,0) then (0,1) — advancing by the
    // TEXTURE row stride, not the copy width. The pre-fix code stepped by copy-width and returned (0,0),(1,0).
    let texels =
        [[1, 2, 3, 4], [5, 6, 7, 8], [9, 10, 11, 12], [13, 14, 15, 16]];
    let mut g = exec();
    let mut cmds = seed_2x2(1, texels);
    cmds.push(Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_DST)));
    cmds.push(Cmd::Submit(CommandBuffer {
        encoder: vec![Enc::CopyTextureToBuffer {
            src: 1,
            mip: 0,
            width: 1,
            height: 2,
            dst: 1,
            dst_offset: 0,
            bytes_per_row: 4,
        }],
        signal: None,
    }));
    let s = run_batch(&mut g, &cmds);
    let out = g.read_buffer(&s.resources, BufferId(1), 0, 8).unwrap();
    assert_eq!(out, [1, 2, 3, 4, 9, 10, 11, 12], "sub-region copy must use the texture row stride");
}

#[test]
fn copy_texture_to_buffer_tight_bytes_per_row_zero() {
    // bytes_per_row == 0 means "tightly packed" on the destination. Pre-fix, a zero stride collapsed every
    // row onto dst_offset (last row wins); the full 2x2 plane must land intact.
    let texels =
        [[1, 2, 3, 4], [5, 6, 7, 8], [9, 10, 11, 12], [13, 14, 15, 16]];
    let mut g = exec();
    let mut cmds = seed_2x2(1, texels);
    cmds.push(Cmd::CreateBuffer(1, buf(16, buffer_usage::COPY_DST)));
    cmds.push(Cmd::Submit(CommandBuffer {
        encoder: vec![Enc::CopyTextureToBuffer {
            src: 1,
            mip: 0,
            width: 2,
            height: 2,
            dst: 1,
            dst_offset: 0,
            bytes_per_row: 0, // tight
        }],
        signal: None,
    }));
    let s = run_batch(&mut g, &cmds);
    let out = g.read_buffer(&s.resources, BufferId(1), 0, 16).unwrap();
    assert_eq!(out, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);
}

#[test]
fn copy_buffer_to_texture_tight_bytes_per_row_zero() {
    // Upload a 2x2 plane with bytes_per_row == 0 (tight). Pre-fix, a zero source stride re-read row 0 for
    // every row; the texture must contain the distinct rows.
    let src: Vec<u8> = (1..=16).collect();
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateTexture(1, tex(2, 2, TextureFormat::Rgba8Unorm, RT)),
            Cmd::CreateBuffer(1, buf(16, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
            Cmd::WriteBuffer { id: 1, offset: 0, data: src.clone() },
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::CopyBufferToTexture {
                    src: 1,
                    src_offset: 0,
                    bytes_per_row: 0, // tight
                    dst: 1,
                    mip: 0,
                    width: 2,
                    height: 2,
                }],
                signal: None,
            }),
        ],
    );
    let px = g.read_texture(&s.resources, 1).unwrap();
    assert_eq!(px, src);
}

#[test]
fn copy_texture_to_texture_subregion() {
    // Copy the 1x1 texel at (1,1) of a seeded 2x2 source into (0,0) of a fresh 2x2 dest. Only dest (0,0)
    // changes; the rest stays zero. Exercises the newly-implemented (previously silently-dropped) T2T op.
    let texels = [[1, 2, 3, 4], [5, 6, 7, 8], [9, 10, 11, 12], [13, 14, 15, 16]];
    let mut g = exec();
    let mut cmds = seed_2x2(1, texels);
    cmds.push(Cmd::CreateTexture(2, tex(2, 2, TextureFormat::Rgba8Unorm, RT)));
    cmds.push(Cmd::Submit(CommandBuffer {
        encoder: vec![Enc::CopyTextureToTexture {
            src: 1,
            src_sub: TextureSubresource::base(),
            src_origin: Origin3d { x: 1, y: 1, z: 0 },
            dst: 2,
            dst_sub: TextureSubresource::base(),
            dst_origin: Origin3d { x: 0, y: 0, z: 0 },
            extent: Extent3d { width: 1, height: 1, depth: 1 },
        }],
        signal: None,
    }));
    let s = run_batch(&mut g, &cmds);
    let px = g.read_texture(&s.resources, 2).unwrap();
    assert_eq!(&px[0..4], &[13, 14, 15, 16], "texel (1,1) of src lands at (0,0) of dst");
    assert_eq!(&px[4..16], &[0u8; 12], "the rest of dst is untouched");
}

#[test]
fn blit_and_resolve_are_not_advertised_and_rejected() {
    // These two ops need image resampling this backend does not implement; advertising them (as the old
    // ALL_COMMANDS did) while silently no-op'ing them was a capability lie. They must be un-advertised AND
    // rejected when submitted, never silently dropped.
    let mut g = exec();
    let caps = g.capabilities();
    assert!(!caps.supports_command(etag::BLIT_TEXTURE), "BlitTexture must not be advertised");
    assert!(!caps.supports_command(etag::RESOLVE_TEXTURE), "ResolveTexture must not be advertised");
    assert!(caps.supports_command(etag::COPY_T2T), "CopyTextureToTexture IS implemented + advertised");

    let sub = TextureSubresource::base();
    let o = Origin3d::default();
    let e = Extent3d { width: 1, height: 1, depth: 1 };
    let r = try_batch(
        &mut g,
        &[
            Cmd::CreateTexture(1, tex(2, 2, TextureFormat::Rgba8Unorm, RT)),
            Cmd::CreateTexture(2, tex(2, 2, TextureFormat::Rgba8Unorm, RT)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::BlitTexture {
                    src: 1,
                    src_sub: sub,
                    src_origin: o,
                    src_extent: e,
                    dst: 2,
                    dst_sub: sub,
                    dst_origin: o,
                    dst_extent: e,
                    filter: Filter::Linear,
                }],
                signal: None,
            }),
        ],
    );
    assert!(r.is_err(), "a submitted (un-advertised) blit must be rejected, not silently dropped");
}

// =================================================================================================
// (2) READBACK REPACK — odd widths, strides, every supported color format (upload → tight readback)
// =================================================================================================

/// Upload `data` tightly into a fresh `w x h` texture of `fmt`, then read the tight plane back and assert
/// it round-trips byte-for-byte (exercises the padded→tight repack for the format's texel size + width).
fn roundtrip(fmt: TextureFormat, w: u32, h: u32, bpt: u32, data: Vec<u8>) {
    assert_eq!(data.len() as u32, w * h * bpt);
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateTexture(1, tex(w, h, fmt, RT)),
            Cmd::CreateBuffer(1, buf(data.len() as u64, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
            Cmd::WriteBuffer { id: 1, offset: 0, data: data.clone() },
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::CopyBufferToTexture {
                    src: 1,
                    src_offset: 0,
                    bytes_per_row: w * bpt,
                    dst: 1,
                    mip: 0,
                    width: w,
                    height: h,
                }],
                signal: None,
            }),
        ],
    );
    let px = g.read_texture(&s.resources, 1).unwrap();
    assert_eq!(px, data, "{fmt:?} {w}x{h} tight readback must round-trip");
}

#[test]
fn readback_rgba8_odd_width_3x2() {
    roundtrip(TextureFormat::Rgba8Unorm, 3, 2, 4, (0..24u8).collect());
}

#[test]
fn readback_rgba8_1x1() {
    roundtrip(TextureFormat::Rgba8Unorm, 1, 1, 4, vec![9, 8, 7, 6]);
}

#[test]
fn readback_rgba8_wide_100x1_crosses_256_stride() {
    roundtrip(TextureFormat::Rgba8Unorm, 100, 1, 4, (0..400u32).map(|i| (i % 251) as u8).collect());
}

#[test]
fn readback_r8_width5() {
    roundtrip(TextureFormat::R8Unorm, 5, 1, 1, vec![10, 20, 30, 40, 50]);
}

#[test]
fn readback_rg8_3x1() {
    roundtrip(TextureFormat::Rg8Unorm, 3, 1, 2, vec![1, 2, 3, 4, 5, 6]);
}

#[test]
fn readback_r32float_2x1() {
    let mut d = Vec::new();
    d.extend_from_slice(&0.25f32.to_le_bytes());
    d.extend_from_slice(&(-3.5f32).to_le_bytes());
    roundtrip(TextureFormat::R32Float, 2, 1, 4, d);
}

#[test]
fn readback_rgba16float_1x1() {
    // half-float bytes for (1.0, 0.0, 0.5, 1.0): 0x3C00, 0x0000, 0x3800, 0x3C00 (little-endian).
    roundtrip(TextureFormat::Rgba16Float, 1, 1, 8, vec![0x00, 0x3C, 0x00, 0x00, 0x00, 0x38, 0x00, 0x3C]);
}

#[test]
fn readback_rgba32float_1x1() {
    let mut d = Vec::new();
    for v in [0.25f32, 0.5, 0.75, 1.0] {
        d.extend_from_slice(&v.to_le_bytes());
    }
    roundtrip(TextureFormat::Rgba32Float, 1, 1, 16, d);
}

// =================================================================================================
// (3) COMPUTE — multi-workgroup index math, boundary size, atomics, zero dispatch
// =================================================================================================

/// `out[gid] = gid` for `gid < n`, with `gid = ctaid.x*ntid.x + tid.x`. Exercises SR_CTAID/NTID/TID,
/// a predicated early-out, pointer arithmetic, and a U32 global store across multiple workgroups.
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
            Inst::LdParam { d: 0, param: 0 },              // 0: out ptr
            Inst::LdParam { d: 1, param: 1 },              // 1: n
            Inst::MovSReg { d: 2, sreg: SR_NTID_X },       // 2
            Inst::MovSReg { d: 3, sreg: SR_CTAID_X },      // 3
            Inst::MovSReg { d: 4, sreg: SR_TID_X },        // 4
            Inst::IMad { d: 5, a: Op::Reg(3), b: Op::Reg(2), c: Op::Reg(4) }, // 5: gid
            Inst::Setp { d: 6, a: Op::Reg(5), b: Op::Reg(1), cmp: CMP_GE, unsigned: true }, // 6: gid>=n
            Inst::Bra { target: 12, pred: Some((6, false)) }, // 7: if true → Ret
            Inst::Cvta { d: 7, s: 0 },                     // 8: base
            Inst::IMul { d: 8, a: Op::Reg(5), b: Op::ImmI(4), wide: true, unsigned: false }, // 9: gid*4
            Inst::IAdd { d: 9, a: Op::Reg(7), b: Op::Reg(8), wide: true }, // 10: addr
            Inst::StGlobal { addr: 9, off: 0, src: Op::Reg(5), ty: gty::U32 }, // 11
            Inst::Ret,                                     // 12
        ],
    }
}

#[test]
fn compute_iota_multi_workgroup_boundary() {
    let n = 20u32; // not a multiple of the block size (8) → last workgroup partially active
    let mut param = vec![0u8; 12];
    param[8..12].copy_from_slice(&n.to_le_bytes());

    let mut g = exec();
    g.define_kernel(1, iota_program());
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::PtxKernel, spirv: kernel_words() },
            Cmd::CreateComputePipeline(
                1,
                ComputePipelineDesc { compute: ShaderRef { module: 1, entry: "iota".into() }, label: String::new() },
            ),
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
                    Enc::Dispatch { x: 3, y: 1, z: 1 }, // ceil(20/8) = 3 workgroups
                    Enc::EndComputePass,
                ],
                signal: None,
            }),
        ],
    );
    let out = g.read_buffer(&s.resources, BufferId(2), 0, (n * 4) as usize).unwrap();
    let got: Vec<u32> = out.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
    assert_eq!(got, (0..n).collect::<Vec<_>>(), "each thread writes its global index; out-of-range guarded");
}

/// Every thread atomically increments counter[0]; the total must equal the launched thread count.
fn atomic_counter_program() -> KernelProgram {
    KernelProgram {
        entry: "counter".into(),
        block: [8, 1, 1],
        params: vec![Param { width: 8, offset: 0, is_ptr: true, region: 0 }],
        param_bytes: 8,
        num_regions: 1,
        shared_bytes: 0,
        reg_count: 2,
        insts: vec![
            Inst::LdParam { d: 0, param: 0 },
            Inst::Cvta { d: 1, s: 0 },
            Inst::AtomGlobal {
                d: None,
                addr: 1,
                off: 0,
                op: ATOM_ADD,
                cmp: Op::ImmI(0),
                val: Op::ImmI(1),
                unsigned: true,
            },
            Inst::Ret,
        ],
    }
}

#[test]
fn compute_atomic_add_counter() {
    let mut g = exec();
    g.define_kernel(1, atomic_counter_program());
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::PtxKernel, spirv: kernel_words() },
            Cmd::CreateComputePipeline(
                1,
                ComputePipelineDesc { compute: ShaderRef { module: 1, entry: "counter".into() }, label: String::new() },
            ),
            Cmd::CreateBuffer(1, buf(8, buffer_usage::STORAGE)),
            Cmd::CreateBuffer(2, buf(4, buffer_usage::STORAGE | buffer_usage::COPY_SRC)),
            Cmd::CreateBindGroup(
                1,
                BindGroupDesc {
                    set: 0,
                    entries: vec![
                        BindEntry { binding: 0, resource: BindResource::Buffer { id: 1, offset: 0, size: 8 } },
                        BindEntry { binding: 1, resource: BindResource::Buffer { id: 2, offset: 0, size: 4 } },
                    ],
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginComputePass,
                    Enc::SetPipeline(1),
                    Enc::SetBindGroup { index: 0, group: 1 },
                    Enc::Dispatch { x: 4, y: 1, z: 1 }, // 4 workgroups * 8 threads = 32
                    Enc::EndComputePass,
                ],
                signal: None,
            }),
        ],
    );
    let out = g.read_buffer(&s.resources, BufferId(2), 0, 4).unwrap();
    assert_eq!(u32::from_le_bytes(out.try_into().unwrap()), 32, "32 atomic increments");
}

#[test]
fn compute_zero_dispatch_is_noop_not_panic() {
    let mut g = exec();
    g.define_kernel(1, atomic_counter_program());
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::PtxKernel, spirv: kernel_words() },
            Cmd::CreateComputePipeline(
                1,
                ComputePipelineDesc { compute: ShaderRef { module: 1, entry: "counter".into() }, label: String::new() },
            ),
            Cmd::CreateBuffer(1, buf(8, buffer_usage::STORAGE)),
            Cmd::CreateBuffer(2, buf(4, buffer_usage::STORAGE | buffer_usage::COPY_SRC)),
            Cmd::CreateBindGroup(
                1,
                BindGroupDesc {
                    set: 0,
                    entries: vec![
                        BindEntry { binding: 0, resource: BindResource::Buffer { id: 1, offset: 0, size: 8 } },
                        BindEntry { binding: 1, resource: BindResource::Buffer { id: 2, offset: 0, size: 4 } },
                    ],
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginComputePass,
                    Enc::SetPipeline(1),
                    Enc::SetBindGroup { index: 0, group: 1 },
                    Enc::Dispatch { x: 0, y: 1, z: 1 }, // zero workgroups
                    Enc::EndComputePass,
                ],
                signal: None,
            }),
        ],
    );
    let out = g.read_buffer(&s.resources, BufferId(2), 0, 4).unwrap();
    assert_eq!(u32::from_le_bytes(out.try_into().unwrap()), 0, "no workgroups → counter untouched");
}

// =================================================================================================
// (4) GRAPHICS — real vertex buffers: multiple attributes, indexed, per-instance step mode
// =================================================================================================

const SEED_POS2_COLOR: &str = r#"
    struct VOut { @builtin(position) pos: vec4<f32>, @location(0) color: vec4<f32> };
    @vertex fn vs_main(@location(0) p: vec2<f32>, @location(1) c: vec4<f32>) -> VOut {
        return VOut(vec4<f32>(p, 0.0, 1.0), c);
    }
    @fragment fn fs_main(v: VOut) -> @location(0) vec4<f32> { return v.color; }
"#;

const SEED_POS2_GREEN: &str = r#"
    @vertex fn vs_main(@location(0) p: vec2<f32>) -> @builtin(position) vec4<f32> {
        return vec4<f32>(p, 0.0, 1.0);
    }
    @fragment fn fs_main() -> @location(0) vec4<f32> { return vec4<f32>(0.0, 1.0, 0.0, 1.0); }
"#;

const SEED_VINDEX_GREEN: &str = r#"
    @vertex fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
        var p = array<vec2<f32>, 3>(vec2<f32>(-1.0,-1.0), vec2<f32>(3.0,-1.0), vec2<f32>(-1.0,3.0));
        return vec4<f32>(p[vi], 0.0, 1.0);
    }
    @fragment fn fs_main() -> @location(0) vec4<f32> { return vec4<f32>(0.0, 1.0, 0.0, 1.0); }
"#;

const SEED_POS3_COLOR: &str = r#"
    struct VOut { @builtin(position) pos: vec4<f32>, @location(0) color: vec4<f32> };
    @vertex fn vs_main(@location(0) p: vec3<f32>, @location(1) c: vec4<f32>) -> VOut {
        return VOut(vec4<f32>(p, 1.0), c);
    }
    @fragment fn fs_main(v: VOut) -> @location(0) vec4<f32> { return v.color; }
"#;

fn all_texels_eq(px: &[u8], expect: [u8; 4]) {
    for (i, t) in px.chunks_exact(4).enumerate() {
        assert_eq!(t, expect, "pixel {i} mismatch");
    }
}

#[test]
fn vertex_buffer_two_attributes_float_and_unorm8() {
    // A fullscreen triangle: pos = Float32x2 (@loc0), color = Unorm8x4 (@loc1). Uniform blue at every
    // vertex, so every pixel reads back exactly [0,0,255,255] regardless of interpolation.
    let mut vbytes = Vec::new();
    for (px, py) in [(-1.0f32, -1.0f32), (3.0, -1.0), (-1.0, 3.0)] {
        vbytes.extend_from_slice(&px.to_le_bytes());
        vbytes.extend_from_slice(&py.to_le_bytes());
        vbytes.extend_from_slice(&[0, 0, 255, 255]); // Unorm8x4 RGBA
    }
    let spirv = wgsl_to_spirv(SEED_POS2_COLOR);
    let layout = VertexLayout {
        stride: 12,
        step_mode: 0,
        attrs: vec![
            VertexAttr { location: 0, format: vfmt(2, 0, false), offset: 0 },
            VertexAttr { location: 1, format: vfmt(4, 1, true), offset: 8 },
        ],
    };
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::SpirV, spirv },
            Cmd::CreateBuffer(1, buf(vbytes.len() as u64, buffer_usage::VERTEX | buffer_usage::COPY_DST)),
            Cmd::WriteBuffer { id: 1, offset: 0, data: vbytes },
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef { module: 1, entry: "vs_main".into() },
                    fragment: Some(ShaderRef { module: 1, entry: "fs_main".into() }),
                    vertex_buffers: vec![layout],
                    color_targets: vec![ColorTargetState { format: TextureFormat::Rgba8Unorm, blend: None, write_mask: 0xF }],
                    depth: None,
                    topology: Topology::TriangleList,
                    cull: 0,
                    front_face: 0,
                    label: String::new(),
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [1.0, 0.0, 0.0, 1.0], store: true }],
                        depth: None,
                    },
                    Enc::SetPipeline(1),
                    Enc::SetVertexBuffer { slot: 0, buffer: 1, offset: 0 },
                    Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ],
    );
    all_texels_eq(&g.read_texture(&s.resources, 1).unwrap(), [0, 0, 255, 255]);
}

#[test]
fn indexed_quad_covers_target() {
    // Two triangles from 4 vertices + a U16 index buffer, covering the whole target with green.
    let mut vbytes = Vec::new();
    for (px, py) in [(-1.0f32, -1.0f32), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
        vbytes.extend_from_slice(&px.to_le_bytes());
        vbytes.extend_from_slice(&py.to_le_bytes());
    }
    let indices: [u16; 6] = [0, 1, 2, 2, 1, 3];
    let ibytes: Vec<u8> = indices.iter().flat_map(|i| i.to_le_bytes()).collect();
    let spirv = wgsl_to_spirv(SEED_POS2_GREEN);
    let layout = VertexLayout {
        stride: 8,
        step_mode: 0,
        attrs: vec![VertexAttr { location: 0, format: vfmt(2, 0, false), offset: 0 }],
    };
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::SpirV, spirv },
            Cmd::CreateBuffer(1, buf(vbytes.len() as u64, buffer_usage::VERTEX | buffer_usage::COPY_DST)),
            Cmd::CreateBuffer(2, buf(ibytes.len() as u64, buffer_usage::INDEX | buffer_usage::COPY_DST)),
            Cmd::WriteBuffer { id: 1, offset: 0, data: vbytes },
            Cmd::WriteBuffer { id: 2, offset: 0, data: ibytes },
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef { module: 1, entry: "vs_main".into() },
                    fragment: Some(ShaderRef { module: 1, entry: "fs_main".into() }),
                    vertex_buffers: vec![layout],
                    color_targets: vec![ColorTargetState { format: TextureFormat::Rgba8Unorm, blend: None, write_mask: 0xF }],
                    depth: None,
                    topology: Topology::TriangleList,
                    cull: 0,
                    front_face: 0,
                    label: String::new(),
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [1.0, 0.0, 0.0, 1.0], store: true }],
                        depth: None,
                    },
                    Enc::SetPipeline(1),
                    Enc::SetVertexBuffer { slot: 0, buffer: 1, offset: 0 },
                    Enc::SetIndexBuffer { buffer: 2, offset: 0, format: hl_gpu::protocol::model::enums::IndexFormat::U16 },
                    Enc::DrawIndexed { index_count: 6, instance_count: 1, first_index: 0, base_vertex: 0, first_instance: 0 },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ],
    );
    all_texels_eq(&g.read_texture(&s.resources, 1).unwrap(), [0, 255, 0, 255]);
}

#[test]
fn instanced_per_instance_step_mode_advances_attribute() {
    // slot0 = per-vertex position (fullscreen triangle). slot1 = PER-INSTANCE Unorm8x4 color, stride 4.
    // Two instances draw the same triangle; the second (green) is last so it wins the color target.
    // If the step mode were wrongly per-vertex, the color attribute would index by vertex → red/green/blue
    // interpolation, NOT a uniform green — so an all-green readback proves per-instance stepping works.
    let mut posb = Vec::new();
    for (px, py) in [(-1.0f32, -1.0f32), (3.0, -1.0), (-1.0, 3.0)] {
        posb.extend_from_slice(&px.to_le_bytes());
        posb.extend_from_slice(&py.to_le_bytes());
    }
    // 4 colors so a (wrong) per-vertex read of vertex index up to 2 stays in-bounds: red, green, blue, white.
    let colorb: Vec<u8> = vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255];
    let spirv = wgsl_to_spirv(SEED_POS2_COLOR);
    let pos_layout = VertexLayout {
        stride: 8,
        step_mode: 0,
        attrs: vec![VertexAttr { location: 0, format: vfmt(2, 0, false), offset: 0 }],
    };
    let color_layout = VertexLayout {
        stride: 4,
        step_mode: 1, // per-instance
        attrs: vec![VertexAttr { location: 1, format: vfmt(4, 1, true), offset: 0 }],
    };
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::SpirV, spirv },
            Cmd::CreateBuffer(1, buf(posb.len() as u64, buffer_usage::VERTEX | buffer_usage::COPY_DST)),
            Cmd::CreateBuffer(2, buf(colorb.len() as u64, buffer_usage::VERTEX | buffer_usage::COPY_DST)),
            Cmd::WriteBuffer { id: 1, offset: 0, data: posb },
            Cmd::WriteBuffer { id: 2, offset: 0, data: colorb },
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef { module: 1, entry: "vs_main".into() },
                    fragment: Some(ShaderRef { module: 1, entry: "fs_main".into() }),
                    vertex_buffers: vec![pos_layout, color_layout],
                    color_targets: vec![ColorTargetState { format: TextureFormat::Rgba8Unorm, blend: None, write_mask: 0xF }],
                    depth: None,
                    topology: Topology::TriangleList,
                    cull: 0,
                    front_face: 0,
                    label: String::new(),
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.0, 0.0, 0.0, 1.0], store: true }],
                        depth: None,
                    },
                    Enc::SetPipeline(1),
                    Enc::SetVertexBuffer { slot: 0, buffer: 1, offset: 0 },
                    Enc::SetVertexBuffer { slot: 1, buffer: 2, offset: 0 },
                    Enc::Draw { vertex_count: 3, instance_count: 2, first_vertex: 0, first_instance: 0 },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ],
    );
    all_texels_eq(&g.read_texture(&s.resources, 1).unwrap(), [0, 255, 0, 255]);
}

#[test]
fn scissor_restricts_draw_to_subrect() {
    // Clear the 4x4 target red, scissor to the inner (1,1)-(3,3) 2x2 box, draw a fullscreen green triangle.
    // Only the scissored texels turn green.
    let spirv = wgsl_to_spirv(SEED_VINDEX_GREEN);
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::SpirV, spirv },
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef { module: 1, entry: "vs_main".into() },
                    fragment: Some(ShaderRef { module: 1, entry: "fs_main".into() }),
                    vertex_buffers: vec![],
                    color_targets: vec![ColorTargetState { format: TextureFormat::Rgba8Unorm, blend: None, write_mask: 0xF }],
                    depth: None,
                    topology: Topology::TriangleList,
                    cull: 0,
                    front_face: 0,
                    label: String::new(),
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [1.0, 0.0, 0.0, 1.0], store: true }],
                        depth: None,
                    },
                    Enc::SetScissor { x: 1, y: 1, w: 2, h: 2 },
                    Enc::SetPipeline(1),
                    Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ],
    );
    let px = g.read_texture(&s.resources, 1).unwrap();
    let green = [0u8, 255, 0, 255];
    let red = [255u8, 0, 0, 255];
    for y in 0..4u32 {
        for x in 0..4u32 {
            let t = &px[((y * 4 + x) * 4) as usize..][..4];
            let inside = (1..3).contains(&x) && (1..3).contains(&y);
            assert_eq!(t, if inside { green } else { red }, "pixel ({x},{y})");
        }
    }
}

// =================================================================================================
// (5) DEPTH — the previously-dropped depth attachment now drives a real depth test
// =================================================================================================

/// Draw two fullscreen triangles (via one pipeline, two vertex buffers) at different depths with `LESS` +
/// depth-write. `first` is drawn first, `second` second; returns the color plane. With no depth test, the
/// second draw always wins; with a real depth test the nearer fragment wins regardless of order.
fn depth_two_draws(near_first: bool) -> Vec<u8> {
    let vbuf = |z: f32, rgba: [f32; 4]| {
        let mut b = Vec::new();
        for (px, py) in [(-1.0f32, -1.0f32), (3.0, -1.0), (-1.0, 3.0)] {
            b.extend_from_slice(&px.to_le_bytes());
            b.extend_from_slice(&py.to_le_bytes());
            b.extend_from_slice(&z.to_le_bytes());
            for c in rgba {
                b.extend_from_slice(&c.to_le_bytes());
            }
        }
        b
    };
    // green near (z=0.2), red far (z=0.8) — draw order depends on `near_first`.
    let (za, ca, zb, cb) = if near_first {
        (0.2f32, [0.0, 1.0, 0.0, 1.0], 0.8f32, [1.0, 0.0, 0.0, 1.0])
    } else {
        (0.8f32, [1.0, 0.0, 0.0, 1.0], 0.2f32, [0.0, 1.0, 0.0, 1.0])
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
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::CreateTexture(2, tex(4, 4, TextureFormat::Depth32Float, texture_usage::RENDER_TARGET)),
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
                    depth: Some(DepthState { format: TextureFormat::Depth32Float, depth_write: true, depth_compare: compare::LESS }),
                    topology: Topology::TriangleList,
                    cull: 0,
                    front_face: 0,
                    label: String::new(),
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.0, 0.0, 0.0, 1.0], store: true }],
                        depth: Some(DepthAttachment { texture: 2, load: LoadOp::Clear, clear_depth: 1.0 }),
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
        ],
    );
    g.read_texture(&s.resources, 1).unwrap()
}

#[test]
fn depth_test_occludes_farther_fragment() {
    // Near green drawn first, far red second: the far fragment fails LESS and is discarded → green stays.
    // Without the depth test the later red draw would overwrite → this asserts depth is really applied.
    all_texels_eq(&depth_two_draws(true), [0, 255, 0, 255]);
}

#[test]
fn depth_test_lets_nearer_fragment_through() {
    // Reverse order: far red drawn first (writes depth 0.8), near green drawn second. The nearer fragment
    // passes LESS (0.2 < 0.8) and overwrites → green. Together with the occlusion test this shows the depth
    // result is order-independent: the nearer fragment wins whether it is drawn first or last.
    all_texels_eq(&depth_two_draws(false), [0, 255, 0, 255]);
}

// =================================================================================================
// (6) GLSL — an advertised payload really executing (naga glsl-in → wgsl-out → device)
// =================================================================================================

#[test]
fn glsl_vertex_fragment_triangle_renders() {
    let vs = "#version 450\n\
        void main() {\n\
            float x = -1.0; float y = -1.0;\n\
            if (gl_VertexIndex == 1) { x = 3.0; }\n\
            if (gl_VertexIndex == 2) { y = 3.0; }\n\
            gl_Position = vec4(x, y, 0.0, 1.0);\n\
        }\n";
    let fs = "#version 450\n\
        layout(location = 0) out vec4 o;\n\
        void main() { o = vec4(0.0, 0.0, 1.0, 1.0); }\n";
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::Glsl, spirv: glsl_words(glsl_stage::VERTEX, "vmain", vs) },
            Cmd::CreateShader { id: 2, kind: ShaderPayloadKind::Glsl, spirv: glsl_words(glsl_stage::FRAGMENT, "fmain", fs) },
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef { module: 1, entry: "vmain".into() },
                    fragment: Some(ShaderRef { module: 2, entry: "fmain".into() }),
                    vertex_buffers: vec![],
                    color_targets: vec![ColorTargetState { format: TextureFormat::Rgba8Unorm, blend: None, write_mask: 0xF }],
                    depth: None,
                    topology: Topology::TriangleList,
                    cull: 0,
                    front_face: 0,
                    label: String::new(),
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [1.0, 0.0, 0.0, 1.0], store: true }],
                        depth: None,
                    },
                    Enc::SetPipeline(1),
                    Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ],
    );
    all_texels_eq(&g.read_texture(&s.resources, 1).unwrap(), [0, 0, 255, 255]);
}

// =================================================================================================
// (7) ADVERSARIAL — malformed / unsupported inputs must return a clean Err, never panic
// =================================================================================================

fn create_shader_only(kind: ShaderPayloadKind, words: Vec<u32>) -> Vec<Cmd> {
    vec![Cmd::CreateShader { id: 1, kind, spirv: words }]
}

#[test]
fn malformed_spirv_bad_magic_errs() {
    let mut g = exec();
    assert!(try_batch(&mut g, &create_shader_only(ShaderPayloadKind::SpirV, vec![0xDEAD_BEEF, 0, 0])).is_err());
}

#[test]
fn malformed_spirv_valid_magic_garbage_errs() {
    // A valid SPIR-V header (magic, version 1.0, gen 0, bound 2, schema 0) followed by an instruction word
    // claiming a 10-word length with no following words — a truncated stream naga's spv-in must reject
    // (rather than panic). ([magic,0,0,0,0] would be a *valid empty* module, so it is not used here.)
    let words = vec![SPIRV_MAGIC, 0x0001_0000, 0, 2, 0, 0x000A_0001];
    let mut g = exec();
    let r = try_batch(&mut g, &create_shader_only(ShaderPayloadKind::SpirV, words));
    assert!(r.is_err(), "truncated SPIR-V instruction stream must be a clean Err");
}

#[test]
fn empty_spirv_words_errs() {
    let mut g = exec();
    assert!(try_batch(&mut g, &create_shader_only(ShaderPayloadKind::SpirV, vec![])).is_err());
}

#[test]
fn malformed_glsl_errs() {
    let mut g = exec();
    let words = glsl_words(glsl_stage::VERTEX, "vmain", "this is not glsl @@@ ;;;");
    assert!(try_batch(&mut g, &create_shader_only(ShaderPayloadKind::Glsl, words)).is_err());
}

#[test]
fn legacy_msl_payload_rejected() {
    // MSL is not advertised → the runtime rejects it at validate; either way it never silently succeeds.
    let mut g = exec();
    assert!(try_batch(&mut g, &create_shader_only(ShaderPayloadKind::LegacyMsl, vec![0x1234_5678, 1, 2])).is_err());
}

#[test]
fn demo_builtin_payload_rejected() {
    // DemoBuiltin passes the (bit==0) validate gate and must be rejected honestly by the executor.
    let mut g = exec();
    assert!(try_batch(&mut g, &create_shader_only(ShaderPayloadKind::DemoBuiltin, vec![1, 2, 3])).is_err());
}

#[test]
fn compute_pipeline_from_graphics_shader_errs() {
    // A SPIR-V *compute* module is now accepted (see tests/spirv_compute.rs), but a graphics-ONLY module
    // (here only the vertex/fragment entries of SEED_VINDEX_GREEN) used for compute must still fail —
    // `vs_main` is a vertex entry, not a compute entry. The executor's error scope must turn wgpu's
    // validation error into a clean typed Err, not a panic.
    let spirv = wgsl_to_spirv(SEED_VINDEX_GREEN);
    let mut g = exec();
    let r = try_batch(
        &mut g,
        &[
            Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::SpirV, spirv },
            Cmd::CreateComputePipeline(
                1,
                ComputePipelineDesc { compute: ShaderRef { module: 1, entry: "vs_main".into() }, label: String::new() },
            ),
        ],
    );
    assert!(r.is_err(), "a graphics-only module (no compute entry point) must not build a compute pipeline");
}

#[test]
fn render_pipeline_from_kernel_shader_errs() {
    let mut g = exec();
    g.define_kernel(1, atomic_counter_program());
    let r = try_batch(
        &mut g,
        &[
            Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::PtxKernel, spirv: kernel_words() },
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef { module: 1, entry: "counter".into() },
                    fragment: None,
                    vertex_buffers: vec![],
                    color_targets: vec![ColorTargetState { format: TextureFormat::Rgba8Unorm, blend: None, write_mask: 0xF }],
                    depth: None,
                    topology: Topology::TriangleList,
                    cull: 0,
                    front_face: 0,
                    label: String::new(),
                },
            ),
        ],
    );
    assert!(r.is_err(), "a render pipeline vertex stage needs a graphics shader, not a kernel");
}

#[test]
fn unsupported_vertex_format_errs() {
    // Unorm8x1 has no WebGPU vertex format → the pipeline lowering must reject it, not silently widen.
    let spirv = wgsl_to_spirv(SEED_POS2_GREEN);
    let layout = VertexLayout {
        stride: 4,
        step_mode: 0,
        attrs: vec![VertexAttr { location: 0, format: vfmt(1, 1, false), offset: 0 }], // u8x1
    };
    let mut g = exec();
    let r = try_batch(
        &mut g,
        &[
            Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::SpirV, spirv },
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef { module: 1, entry: "vs_main".into() },
                    fragment: Some(ShaderRef { module: 1, entry: "fs_main".into() }),
                    vertex_buffers: vec![layout],
                    color_targets: vec![ColorTargetState { format: TextureFormat::Rgba8Unorm, blend: None, write_mask: 0xF }],
                    depth: None,
                    topology: Topology::TriangleList,
                    cull: 0,
                    front_face: 0,
                    label: String::new(),
                },
            ),
        ],
    );
    assert!(r.is_err(), "unsupported vertex attribute format must be a clean Err");
}

#[test]
fn out_of_bounds_buffer_read_errs() {
    let mut g = exec();
    let s = run_batch(&mut g, &[Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_DST | buffer_usage::COPY_SRC))]);
    assert!(g.read_buffer(&s.resources, BufferId(1), 4, 8).is_err(), "read past end must be OutOfBounds");
}

// =================================================================================================
// (8) CAPABILITY HONESTY — the advertisement must match what the executor actually accepts
// =================================================================================================

#[test]
fn capability_advertisement_is_honest() {
    let g = exec();
    let caps = g.capabilities();

    // Present kinds: only Shm is claimed (no IOSurface/dma-buf handoff from this backend).
    assert_eq!(caps.present_kinds, vec![PresentKind::Shm]);

    // Shader payloads: exactly the ones with a real accept path. SPIR-V / GLSL / KERNEL are exercised
    // end-to-end by the tests above; WGSL and MSL have NO wire path this executor accepts, so advertising
    // them would be a lie.
    assert_ne!(caps.shader_payloads & shader_payload::SPIRV, 0, "SPIR-V advertised + accepted");
    assert_ne!(caps.shader_payloads & shader_payload::GLSL, 0, "GLSL advertised + accepted");
    assert_ne!(caps.shader_payloads & shader_payload::KERNEL, 0, "kernel advertised + accepted");
    assert_eq!(caps.shader_payloads & shader_payload::WGSL, 0, "WGSL must NOT be advertised (no accept path)");
    assert_eq!(caps.shader_payloads & shader_payload::MSL, 0, "MSL must NOT be advertised (rejected)");

    assert!(caps.supports_compute && caps.supports_graphics);
    assert!(!caps.supports_timeline_fences, "fences are emulated via submit completion, not real timelines");

    // Command set: the ops with a real replay arm are advertised; the resampling ops that are not
    // implemented (blit/resolve) are not — so a negotiation can never promise a command the executor drops.
    for &t in &[
        etag::BEGIN_RENDER_PASS, etag::DRAW, etag::DRAW_INDEXED, etag::DISPATCH, etag::CLEAR_RECT,
        etag::COPY_B2B, etag::COPY_B2T, etag::COPY_T2B, etag::COPY_T2T, etag::FILL_BUFFER,
        etag::SET_VERTEX_BUFFER, etag::SET_INDEX_BUFFER, etag::SET_SCISSOR, etag::SET_VIEWPORT,
    ] {
        assert!(caps.supports_command(t), "etag {t} has a replay arm and must be advertised");
    }
    assert!(!caps.supports_command(etag::BLIT_TEXTURE));
    assert!(!caps.supports_command(etag::RESOLVE_TEXTURE));
}
