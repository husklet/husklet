//! wgpu conformance — the SAME frozen cases as `hl_wip-gpu/tests/conformance.rs`, driven through the real
//! runtime pipeline (validate → account → dispatch → execute) but against a `WgpuExecutor` running on the
//! headless software Vulkan device (lavapipe / `llvmpipe`) instead of the CPU oracle. Every asserted value
//! is IDENTICAL to the oracle's, proving the wgpu backend reproduces it — now with real SPIR-V/WGSL
//! shaders EXECUTING on the device (the compute vecadd kernel and a SPIR-V vertex+fragment triangle, which
//! the pure-CPU oracle cannot run).
//!
//! A single lavapipe device is shared across the cases behind a mutex (device bring-up is the expensive
//! part; the cases themselves are independent, each with its own fresh runtime `Session`).

use std::sync::{Mutex, MutexGuard, OnceLock};

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, ColorTargetState,
    ComputePipelineDesc, RenderPipelineDesc, ShaderRef, TextureDesc,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, LoadOp, TextureDim, TextureFormat, Topology,
};
use hl_gpu::protocol::model::kernel::{
    gty, Inst, KernelProgram, Op, Param, CMP_GE, KERNEL_MAGIC, SR_CTAID_X, SR_NTID_X, SR_TID_X,
};
use hl_gpu::{
    BufferId, Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session,
    ShaderPayloadKind,
};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

// -------------------------------------------------------------------------------------------------
// shared device + runtime-pipeline harness
// -------------------------------------------------------------------------------------------------

static EXEC: OnceLock<Mutex<WgpuExecutor>> = OnceLock::new();

/// The process-wide wgpu executor bound to the headless software Vulkan adapter.
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

/// Submit `cmds` as one batch through validate → account → dispatch against `exec`, returning the
/// `Session` so its `resources` can be read back. Copy alignment is byte-addressable (1), matching the
/// oracle harness, so the suite's unaligned copies validate.
fn run_batch(exec: &mut WgpuExecutor, cmds: &[Cmd]) -> Session {
    let caps = exec.capabilities();
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    let mut s = Session::new(limits, GlobalLedger::unbounded(), Box::new(FakeClock::new(0)));
    hl_gpu::runtime::submit(&mut s, exec, 0, cmds).expect("conformance program must run cleanly");
    s
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

fn kernel_words() -> Vec<u32> {
    vec![KERNEL_MAGIC, 0]
}

// -------------------------------------------------------------------------------------------------
// adapter identity: prove we bound the software Vulkan device
// -------------------------------------------------------------------------------------------------

#[test]
fn bound_adapter_is_software_vulkan() {
    let g = exec();
    let info = g.adapter_info();
    eprintln!("wgpu adapter: name={:?} backend={:?} type={:?}", info.name, info.backend, info.device_type);
    assert_eq!(info.backend, wgpu::Backend::Vulkan, "expected the Vulkan backend (lavapipe)");
    let name = info.name.to_lowercase();
    assert!(
        name.contains("llvmpipe") || name.contains("lavapipe") || info.device_type == wgpu::DeviceType::Cpu,
        "expected a software adapter, got {:?}",
        info.name
    );
}

// -------------------------------------------------------------------------------------------------
// buffer: write + readback
// -------------------------------------------------------------------------------------------------

#[test]
fn buffer_write_then_readback_exact_bytes() {
    let data = vec![0x01u8, 0x02, 0x03, 0x04, 0xAA, 0xBB, 0xCC, 0xDD];
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_DST | buffer_usage::COPY_SRC)),
            Cmd::WriteBuffer { id: 1, offset: 0, data: data.clone() },
        ],
    );
    let out = g.read_buffer(&s.resources, BufferId(1), 0, 8).unwrap();
    assert_eq!(out, data);
}

#[test]
fn buffer_write_at_offset_leaves_prefix_zeroed() {
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_DST)),
            Cmd::WriteBuffer { id: 1, offset: 4, data: vec![0x11, 0x22, 0x33, 0x44] },
        ],
    );
    let out = g.read_buffer(&s.resources, BufferId(1), 0, 8).unwrap();
    assert_eq!(out, [0, 0, 0, 0, 0x11, 0x22, 0x33, 0x44]);
}

#[test]
fn buffer_partial_readback_window() {
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_DST)),
            Cmd::WriteBuffer { id: 1, offset: 0, data: vec![0, 1, 2, 3, 4, 5, 6, 7] },
        ],
    );
    let out = g.read_buffer(&s.resources, BufferId(1), 2, 3).unwrap();
    assert_eq!(out, [2, 3, 4]);
}

// -------------------------------------------------------------------------------------------------
// buffer -> buffer copy
// -------------------------------------------------------------------------------------------------

#[test]
fn buffer_to_buffer_copy_full() {
    let src = vec![0xDEu8, 0xAD, 0xBE, 0xEF];
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateBuffer(1, buf(4, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
            Cmd::CreateBuffer(2, buf(4, buffer_usage::COPY_DST)),
            Cmd::WriteBuffer { id: 1, offset: 0, data: src.clone() },
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::CopyBufferToBuffer {
                    src: 1,
                    src_offset: 0,
                    dst: 2,
                    dst_offset: 0,
                    size: 4,
                }],
                signal: None,
            }),
        ],
    );
    let out = g.read_buffer(&s.resources, BufferId(2), 0, 4).unwrap();
    assert_eq!(out, src);
}

#[test]
fn buffer_to_buffer_copy_with_offsets() {
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateBuffer(1, buf(6, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
            Cmd::CreateBuffer(2, buf(6, buffer_usage::COPY_DST)),
            Cmd::WriteBuffer { id: 1, offset: 0, data: vec![10, 11, 12, 13, 14, 15] },
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::CopyBufferToBuffer {
                    src: 1,
                    src_offset: 2,
                    dst: 2,
                    dst_offset: 4,
                    size: 2,
                }],
                signal: None,
            }),
        ],
    );
    let out = g.read_buffer(&s.resources, BufferId(2), 0, 6).unwrap();
    assert_eq!(out, [0, 0, 0, 0, 12, 13]);
}

// -------------------------------------------------------------------------------------------------
// texture clear + readback
// -------------------------------------------------------------------------------------------------

fn clear_pass(texture: u32, clear: [f32; 4]) -> Cmd {
    Cmd::Submit(CommandBuffer {
        encoder: vec![
            Enc::BeginRenderPass {
                color: vec![ColorAttachment { texture, load: LoadOp::Clear, clear, store: true }],
                depth: None,
            },
            Enc::EndRenderPass,
        ],
        signal: None,
    })
}

#[test]
fn texture_clear_rgba8_readback_red() {
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateTexture(
                1,
                tex(1, 1, TextureFormat::Rgba8Unorm, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC),
            ),
            clear_pass(1, [1.0, 0.0, 0.0, 1.0]),
        ],
    );
    let px = g.read_texture(&s.resources, 1).unwrap();
    assert_eq!(px, [255, 0, 0, 255]);
}

#[test]
fn texture_clear_bgra8_channel_order() {
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateTexture(
                1,
                tex(1, 1, TextureFormat::Bgra8Unorm, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC),
            ),
            clear_pass(1, [1.0, 0.0, 0.0, 1.0]),
        ],
    );
    let px = g.read_texture(&s.resources, 1).unwrap();
    assert_eq!(px, [0, 0, 255, 255]); // B, G, R, A
}

#[test]
fn texture_clear_fills_all_texels() {
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateTexture(
                1,
                tex(2, 2, TextureFormat::Rgba8Unorm, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC),
            ),
            clear_pass(1, [0.0, 1.0, 0.0, 1.0]),
        ],
    );
    let px = g.read_texture(&s.resources, 1).unwrap();
    let green = [0u8, 255, 0, 255];
    for texel in px.chunks_exact(4) {
        assert_eq!(texel, green);
    }
}

#[test]
fn texture_clear_midgray_rounds_to_128() {
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateTexture(
                1,
                tex(1, 1, TextureFormat::Rgba8Unorm, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC),
            ),
            clear_pass(1, [0.5, 0.5, 0.5, 0.5]),
        ],
    );
    let px = g.read_texture(&s.resources, 1).unwrap();
    assert_eq!(px, [128, 128, 128, 128]);
}

#[test]
fn clear_rect_scopes_to_subrectangle() {
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateTexture(
                1,
                tex(2, 2, TextureFormat::Rgba8Unorm, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC),
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::ClearRect {
                    texture: 1,
                    x: 0,
                    y: 0,
                    w: 1,
                    h: 1,
                    color: [1.0, 0.0, 0.0, 1.0],
                }],
                signal: None,
            }),
        ],
    );
    let px = g.read_texture(&s.resources, 1).unwrap();
    assert_eq!(&px[0..4], &[255, 0, 0, 255]);
    assert_eq!(&px[4..16], &[0u8; 12]);
}

// -------------------------------------------------------------------------------------------------
// texture -> buffer readback copy
// -------------------------------------------------------------------------------------------------

#[test]
fn texture_clear_then_copy_to_buffer() {
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateTexture(
                1,
                tex(1, 1, TextureFormat::Rgba8Unorm, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC),
            ),
            Cmd::CreateBuffer(1, buf(4, buffer_usage::COPY_DST)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment {
                            texture: 1,
                            load: LoadOp::Clear,
                            clear: [0.0, 0.0, 1.0, 1.0],
                            store: true,
                        }],
                        depth: None,
                    },
                    Enc::EndRenderPass,
                    Enc::CopyTextureToBuffer {
                        src: 1,
                        mip: 0,
                        width: 1,
                        height: 1,
                        dst: 1,
                        dst_offset: 0,
                        bytes_per_row: 4,
                    },
                ],
                signal: None,
            }),
        ],
    );
    let out = g.read_buffer(&s.resources, BufferId(1), 0, 4).unwrap();
    assert_eq!(out, [0, 0, 255, 255]); // blue
}

// -------------------------------------------------------------------------------------------------
// FillBuffer — device-side memset
// -------------------------------------------------------------------------------------------------

#[test]
fn fill_buffer_writes_repeating_pattern() {
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_DST | buffer_usage::COPY_SRC)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::FillBuffer { buffer: 1, offset: 0, size: 8, value: 0xAABB_CCDD }],
                signal: None,
            }),
        ],
    );
    let out = g.read_buffer(&s.resources, BufferId(1), 0, 8).unwrap();
    assert_eq!(out, [0xDD, 0xCC, 0xBB, 0xAA, 0xDD, 0xCC, 0xBB, 0xAA]);
}

#[test]
fn fill_buffer_scopes_to_offset_and_size() {
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_DST | buffer_usage::COPY_SRC)),
            Cmd::WriteBuffer { id: 1, offset: 0, data: vec![0xFF; 8] },
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::FillBuffer { buffer: 1, offset: 2, size: 3, value: 0xAABB_CCDD }],
                signal: None,
            }),
        ],
    );
    let out = g.read_buffer(&s.resources, BufferId(1), 0, 8).unwrap();
    assert_eq!(out, [0xFF, 0xFF, 0xDD, 0xCC, 0xBB, 0xFF, 0xFF, 0xFF]);
}

// -------------------------------------------------------------------------------------------------
// compute dispatch — kernel-IR lowered to WGSL and EXECUTED on the device
// -------------------------------------------------------------------------------------------------

fn store_one_program() -> KernelProgram {
    KernelProgram {
        entry: "store_one".into(),
        block: [1, 1, 1],
        params: vec![Param { width: 8, offset: 0, is_ptr: true, region: 0 }],
        param_bytes: 8,
        num_regions: 1,
        shared_bytes: 0,
        reg_count: 3,
        insts: vec![
            Inst::LdParam { d: 0, param: 0 },
            Inst::Cvta { d: 1, s: 0 },
            Inst::MovImmF { d: 2, bits: 0x3F80_0000 }, // 1.0f
            Inst::StGlobal { addr: 1, off: 0, src: Op::Reg(2), ty: gty::F32 },
            Inst::Ret,
        ],
    }
}

fn vecadd_program() -> KernelProgram {
    KernelProgram {
        entry: "vecadd".into(),
        block: [4, 1, 1],
        params: vec![
            Param { width: 8, offset: 0, is_ptr: true, region: 0 },
            Param { width: 8, offset: 8, is_ptr: true, region: 1 },
            Param { width: 8, offset: 16, is_ptr: true, region: 2 },
            Param { width: 4, offset: 24, is_ptr: false, region: 0 },
        ],
        param_bytes: 28,
        num_regions: 3,
        shared_bytes: 0,
        reg_count: 19,
        insts: vec![
            Inst::LdParam { d: 0, param: 0 },
            Inst::LdParam { d: 1, param: 1 },
            Inst::LdParam { d: 2, param: 2 },
            Inst::LdParam { d: 3, param: 3 },
            Inst::MovSReg { d: 4, sreg: SR_NTID_X },
            Inst::MovSReg { d: 5, sreg: SR_CTAID_X },
            Inst::MovSReg { d: 6, sreg: SR_TID_X },
            Inst::IMad { d: 7, a: Op::Reg(5), b: Op::Reg(4), c: Op::Reg(6) },
            Inst::Setp { d: 8, a: Op::Reg(7), b: Op::Reg(3), cmp: CMP_GE, unsigned: false },
            Inst::Bra { target: 21, pred: Some((8, false)) },
            Inst::Cvta { d: 9, s: 0 },
            Inst::IMul { d: 10, a: Op::Reg(7), b: Op::ImmI(4), wide: true, unsigned: false },
            Inst::IAdd { d: 11, a: Op::Reg(9), b: Op::Reg(10), wide: true },
            Inst::Cvta { d: 12, s: 1 },
            Inst::IAdd { d: 13, a: Op::Reg(12), b: Op::Reg(10), wide: true },
            Inst::LdGlobal { d: 14, addr: 13, off: 0, ty: gty::F32 },
            Inst::LdGlobal { d: 15, addr: 11, off: 0, ty: gty::F32 },
            Inst::FAdd { d: 16, a: Op::Reg(15), b: Op::Reg(14) },
            Inst::Cvta { d: 17, s: 2 },
            Inst::IAdd { d: 18, a: Op::Reg(17), b: Op::Reg(10), wide: true },
            Inst::StGlobal { addr: 18, off: 0, src: Op::Reg(16), ty: gty::F32 },
            Inst::Ret,
        ],
    }
}

#[test]
fn compute_dispatch_writes_constant_into_buffer() {
    let mut g = exec();
    g.define_kernel(1, store_one_program());
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::PtxKernel, spirv: kernel_words() },
            Cmd::CreateComputePipeline(
                1,
                ComputePipelineDesc {
                    compute: ShaderRef { module: 1, entry: "store_one".into() },
                    label: String::new(),
                },
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
                    Enc::Dispatch { x: 1, y: 1, z: 1 },
                    Enc::EndComputePass,
                ],
                signal: None,
            }),
        ],
    );
    let out = g.read_buffer(&s.resources, BufferId(2), 0, 4).unwrap();
    assert_eq!(f32::from_le_bytes(out.try_into().unwrap()), 1.0, "kernel must store 1.0f into region 0");
}

#[test]
fn compute_vecadd_elementwise() {
    let n = 4u32;
    let a = [1.0f32, 2.0, 3.0, 4.0];
    let b = [10.0f32, 20.0, 30.0, 40.0];
    let to_bytes = |v: &[f32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();

    let mut param = vec![0u8; 28];
    param[24..28].copy_from_slice(&n.to_le_bytes());

    let mut g = exec();
    g.define_kernel(1, vecadd_program());
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::PtxKernel, spirv: kernel_words() },
            Cmd::CreateComputePipeline(
                1,
                ComputePipelineDesc {
                    compute: ShaderRef { module: 1, entry: "vecadd".into() },
                    label: String::new(),
                },
            ),
            Cmd::CreateBuffer(1, buf(28, buffer_usage::STORAGE)),
            Cmd::CreateBuffer(2, buf(16, buffer_usage::STORAGE)),
            Cmd::CreateBuffer(3, buf(16, buffer_usage::STORAGE)),
            Cmd::CreateBuffer(4, buf(16, buffer_usage::STORAGE | buffer_usage::COPY_SRC)),
            Cmd::WriteBuffer { id: 1, offset: 0, data: param },
            Cmd::WriteBuffer { id: 2, offset: 0, data: to_bytes(&a) },
            Cmd::WriteBuffer { id: 3, offset: 0, data: to_bytes(&b) },
            Cmd::CreateBindGroup(
                1,
                BindGroupDesc {
                    set: 0,
                    entries: vec![
                        BindEntry { binding: 0, resource: BindResource::Buffer { id: 1, offset: 0, size: 28 } },
                        BindEntry { binding: 1, resource: BindResource::Buffer { id: 2, offset: 0, size: 16 } },
                        BindEntry { binding: 2, resource: BindResource::Buffer { id: 3, offset: 0, size: 16 } },
                        BindEntry { binding: 3, resource: BindResource::Buffer { id: 4, offset: 0, size: 16 } },
                    ],
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginComputePass,
                    Enc::SetPipeline(1),
                    Enc::SetBindGroup { index: 0, group: 1 },
                    Enc::Dispatch { x: 1, y: 1, z: 1 },
                    Enc::EndComputePass,
                ],
                signal: None,
            }),
        ],
    );
    let out = g.read_buffer(&s.resources, BufferId(4), 0, 16).unwrap();
    let got: Vec<f32> =
        out.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
    assert_eq!(got, vec![11.0, 22.0, 33.0, 44.0], "real WGSL compute kernel on lavapipe");
}

// -------------------------------------------------------------------------------------------------
// graphics — a REAL SPIR-V vertex+fragment triangle rasterized on lavapipe (the CPU oracle cannot)
// -------------------------------------------------------------------------------------------------

/// Mint real SPIR-V (with both entry points) from a WGSL seed via naga (wgsl-in → spv-out) — the round
/// trip the guest's SPIR-V ABI relies on. The executor then translates it back (spv-in → wgsl-out) and
/// builds a real render pipeline, so the SPIR-V genuinely drives the rasterizer.
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

#[test]
fn graphics_spirv_triangle_shades_pixels() {
    // A full-clip-space triangle (covers every pixel of the target) whose fragment shader outputs solid
    // green. Both entry points live in one SPIR-V module.
    let seed = r#"
        @vertex
        fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
            var p = array<vec2<f32>, 3>(
                vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
            return vec4<f32>(p[vi], 0.0, 1.0);
        }
        @fragment
        fn fs_main() -> @location(0) vec4<f32> {
            return vec4<f32>(0.0, 1.0, 0.0, 1.0);
        }
    "#;
    let spirv = wgsl_to_spirv(seed);

    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateTexture(
                1,
                tex(4, 4, TextureFormat::Rgba8Unorm, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC),
            ),
            Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::SpirV, spirv },
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef { module: 1, entry: "vs_main".into() },
                    fragment: Some(ShaderRef { module: 1, entry: "fs_main".into() }),
                    vertex_buffers: vec![],
                    color_targets: vec![ColorTargetState {
                        format: TextureFormat::Rgba8Unorm,
                        blend: None,
                        write_mask: 0xF,
                    }],
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
                        color: vec![ColorAttachment {
                            texture: 1,
                            load: LoadOp::Clear,
                            clear: [1.0, 0.0, 0.0, 1.0], // red background — overwritten by the triangle
                            store: true,
                        }],
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
    let px = g.read_texture(&s.resources, 1).unwrap();
    let green = [0u8, 255, 0, 255];
    for (i, texel) in px.chunks_exact(4).enumerate() {
        assert_eq!(texel, green, "pixel {i} should be shaded green by the SPIR-V fragment shader");
    }
}
