//! Executor-neutral GPU conformance suite for the v2 stack — the semantic oracle for the hl-gpu IR,
//! driven through the NEW path (a runtime [`Session`] + a [`CpuExecutor`], submitting each batch via the
//! runtime pipeline validate → account → dispatch → execute) and asserting the exact observable result
//! (buffer readback bytes, texture pixel readback).
//!
//! Every asserted value here is IDENTICAL to what the shipping `hl-gpu/tests/conformance.rs` asserts today
//! against its `SoftwareBackend`: this proves the ported [`CpuExecutor`] reproduces the oracle byte-for-
//! byte. A future real executor (`hl-gpu-wgpu`) pointed at the same suite must match these same values.

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, ComputePipelineDesc,
    ShaderRef, TextureDesc,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, LoadOp, TextureDim, TextureFormat,
};
use hl_gpu::protocol::model::kernel::{
    gty, Inst, KernelProgram, Op, Param, CMP_GE, KERNEL_MAGIC, SR_CTAID_X, SR_NTID_X, SR_TID_X,
};
use hl_gpu::{
    BufferId, Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session,
    ShaderPayloadKind, TextureId,
};

// -------------------------------------------------------------------------------------------------
// harness: drive a program through the real runtime pipeline against a CpuExecutor
// -------------------------------------------------------------------------------------------------

/// Submit `cmds` as one batch through validate → account → dispatch against `exec`, returning the
/// [`Session`] (so its `resources` can be read back). Panics on any pipeline error (a conformance program
/// is expected to be well-formed). The connection's copy alignment is set byte-addressable because the CPU
/// oracle is byte-addressable (matching the direct-replay path `hl-gpu/tests/conformance.rs` uses).
fn run_batch(exec: &mut hl_gpu::CpuExecutor, cmds: &[Cmd]) -> Session {
    let caps = exec.capabilities();
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    let mut s = Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    hl_gpu::runtime::submit(&mut s, exec, 0, cmds).expect("conformance program must run cleanly");
    s
}

fn buf(size: u64, usage: u32) -> BufferDesc {
    BufferDesc {
        size,
        usage,
        label: String::new(),
    }
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

/// Non-empty placeholder shader words for a `PtxKernel` `CreateShader`; the compiled program itself is
/// injected via [`hl_gpu::CpuExecutor::define_kernel`] (the PTX front-end is a driver concern, not here).
fn kernel_words() -> Vec<u32> {
    vec![KERNEL_MAGIC, 0]
}

// -------------------------------------------------------------------------------------------------
// buffer: write + readback
// -------------------------------------------------------------------------------------------------

#[test]
fn buffer_write_then_readback_exact_bytes() {
    let data = vec![0x01u8, 0x02, 0x03, 0x04, 0xAA, 0xBB, 0xCC, 0xDD];
    let mut exec = hl_gpu::CpuExecutor::new();
    let s = run_batch(
        &mut exec,
        &[
            Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_DST | buffer_usage::COPY_SRC)),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: data.clone(),
            },
        ],
    );
    let mut out = [0u8; 8];
    exec.read_buffer(&s.resources, BufferId(1), 0, &mut out)
        .unwrap();
    assert_eq!(out, data.as_slice());
}

#[test]
fn buffer_write_at_offset_leaves_prefix_zeroed() {
    let mut exec = hl_gpu::CpuExecutor::new();
    let s = run_batch(
        &mut exec,
        &[
            Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_DST)),
            Cmd::WriteBuffer {
                id: 1,
                offset: 4,
                data: vec![0x11, 0x22, 0x33, 0x44],
            },
        ],
    );
    let mut out = [0u8; 8];
    exec.read_buffer(&s.resources, BufferId(1), 0, &mut out)
        .unwrap();
    assert_eq!(out, [0, 0, 0, 0, 0x11, 0x22, 0x33, 0x44]);
}

#[test]
fn buffer_partial_readback_window() {
    let mut exec = hl_gpu::CpuExecutor::new();
    let s = run_batch(
        &mut exec,
        &[
            Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_DST)),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: vec![0, 1, 2, 3, 4, 5, 6, 7],
            },
        ],
    );
    let mut out = [0u8; 3];
    exec.read_buffer(&s.resources, BufferId(1), 2, &mut out)
        .unwrap();
    assert_eq!(out, [2, 3, 4]);
}

// -------------------------------------------------------------------------------------------------
// buffer -> buffer copy
// -------------------------------------------------------------------------------------------------

#[test]
fn buffer_to_buffer_copy_full() {
    let src = vec![0xDEu8, 0xAD, 0xBE, 0xEF];
    let mut exec = hl_gpu::CpuExecutor::new();
    let s = run_batch(
        &mut exec,
        &[
            Cmd::CreateBuffer(1, buf(4, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
            Cmd::CreateBuffer(2, buf(4, buffer_usage::COPY_DST)),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: src.clone(),
            },
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
    let mut out = [0u8; 4];
    exec.read_buffer(&s.resources, BufferId(2), 0, &mut out)
        .unwrap();
    assert_eq!(out, src.as_slice());
}

#[test]
fn buffer_to_buffer_copy_with_offsets() {
    let mut exec = hl_gpu::CpuExecutor::new();
    let s = run_batch(
        &mut exec,
        &[
            Cmd::CreateBuffer(1, buf(6, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
            Cmd::CreateBuffer(2, buf(6, buffer_usage::COPY_DST)),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: vec![10, 11, 12, 13, 14, 15],
            },
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
    let mut out = [0u8; 6];
    exec.read_buffer(&s.resources, BufferId(2), 0, &mut out)
        .unwrap();
    assert_eq!(out, [0, 0, 0, 0, 12, 13]);
}

// -------------------------------------------------------------------------------------------------
// texture clear + readback (render-pass clear and ClearRect)
// -------------------------------------------------------------------------------------------------

fn clear_pass(texture: u32, clear: [f32; 4]) -> Cmd {
    Cmd::Submit(CommandBuffer {
        encoder: vec![
            Enc::BeginRenderPass {
                color: vec![ColorAttachment {
                    texture,
                    load: LoadOp::Clear,
                    clear,
                    store: true,
                }],
                depth: None,
            },
            Enc::EndRenderPass,
        ],
        signal: None,
    })
}

#[test]
fn texture_clear_rgba8_readback_red() {
    let mut exec = hl_gpu::CpuExecutor::new();
    let s = run_batch(
        &mut exec,
        &[
            Cmd::CreateTexture(
                1,
                tex(
                    1,
                    1,
                    TextureFormat::Rgba8Unorm,
                    texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
                ),
            ),
            clear_pass(1, [1.0, 0.0, 0.0, 1.0]),
        ],
    );
    let mut px = [0u8; 4];
    exec.read_texture(&s.resources, TextureId(1), &mut px)
        .unwrap();
    assert_eq!(px, [255, 0, 0, 255]);
}

#[test]
fn texture_clear_bgra8_channel_order() {
    let mut exec = hl_gpu::CpuExecutor::new();
    let s = run_batch(
        &mut exec,
        &[
            Cmd::CreateTexture(
                1,
                tex(
                    1,
                    1,
                    TextureFormat::Bgra8Unorm,
                    texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
                ),
            ),
            clear_pass(1, [1.0, 0.0, 0.0, 1.0]),
        ],
    );
    let mut px = [0u8; 4];
    exec.read_texture(&s.resources, TextureId(1), &mut px)
        .unwrap();
    assert_eq!(px, [0, 0, 255, 255]); // B, G, R, A
}

#[test]
fn texture_clear_fills_all_texels() {
    let mut exec = hl_gpu::CpuExecutor::new();
    let s = run_batch(
        &mut exec,
        &[
            Cmd::CreateTexture(
                1,
                tex(
                    2,
                    2,
                    TextureFormat::Rgba8Unorm,
                    texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
                ),
            ),
            clear_pass(1, [0.0, 1.0, 0.0, 1.0]),
        ],
    );
    let mut px = [0u8; 16];
    exec.read_texture(&s.resources, TextureId(1), &mut px)
        .unwrap();
    let green = [0u8, 255, 0, 255];
    for texel in px.chunks_exact(4) {
        assert_eq!(texel, green);
    }
}

#[test]
fn texture_clear_midgray_rounds_to_128() {
    let mut exec = hl_gpu::CpuExecutor::new();
    let s = run_batch(
        &mut exec,
        &[
            Cmd::CreateTexture(
                1,
                tex(
                    1,
                    1,
                    TextureFormat::Rgba8Unorm,
                    texture_usage::RENDER_TARGET,
                ),
            ),
            clear_pass(1, [0.5, 0.5, 0.5, 0.5]),
        ],
    );
    let mut px = [0u8; 4];
    exec.read_texture(&s.resources, TextureId(1), &mut px)
        .unwrap();
    assert_eq!(px, [128, 128, 128, 128]);
}

#[test]
fn clear_rect_scopes_to_subrectangle() {
    let mut exec = hl_gpu::CpuExecutor::new();
    let s = run_batch(
        &mut exec,
        &[
            Cmd::CreateTexture(
                1,
                tex(
                    2,
                    2,
                    TextureFormat::Rgba8Unorm,
                    texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
                ),
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
    let mut px = [0u8; 16];
    exec.read_texture(&s.resources, TextureId(1), &mut px)
        .unwrap();
    assert_eq!(&px[0..4], &[255, 0, 0, 255]);
    assert_eq!(&px[4..16], &[0u8; 12]);
}

// -------------------------------------------------------------------------------------------------
// texture -> buffer readback copy
// -------------------------------------------------------------------------------------------------

#[test]
fn texture_clear_then_copy_to_buffer() {
    let mut exec = hl_gpu::CpuExecutor::new();
    let s = run_batch(
        &mut exec,
        &[
            Cmd::CreateTexture(
                1,
                tex(
                    1,
                    1,
                    TextureFormat::Rgba8Unorm,
                    texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
                ),
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
    let mut out = [0u8; 4];
    exec.read_buffer(&s.resources, BufferId(1), 0, &mut out)
        .unwrap();
    assert_eq!(out, [0, 0, 255, 255]); // blue
}

// -------------------------------------------------------------------------------------------------
// FillBuffer — device-side memset (etag 21, WIRE_VERSION 5)
// -------------------------------------------------------------------------------------------------

#[test]
fn fill_buffer_writes_repeating_pattern() {
    // Fill an 8-byte buffer with the little-endian pattern of 0xAABBCCDD, then read it back: the pattern
    // tiles from the fill offset (LE bytes DD CC BB AA repeated).
    let mut exec = hl_gpu::CpuExecutor::new();
    let s = run_batch(
        &mut exec,
        &[
            Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_DST | buffer_usage::COPY_SRC)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::FillBuffer {
                    buffer: 1,
                    offset: 0,
                    size: 8,
                    value: 0xAABB_CCDD,
                }],
                signal: None,
            }),
        ],
    );
    let mut out = [0u8; 8];
    exec.read_buffer(&s.resources, BufferId(1), 0, &mut out)
        .unwrap();
    assert_eq!(out, [0xDD, 0xCC, 0xBB, 0xAA, 0xDD, 0xCC, 0xBB, 0xAA]);
}

#[test]
fn fill_buffer_scopes_to_offset_and_size() {
    // A sub-range fill must leave the bytes outside [offset, offset+size) untouched, and a size that is
    // not a multiple of 4 fills a partial pattern at the tail.
    let mut exec = hl_gpu::CpuExecutor::new();
    let s = run_batch(
        &mut exec,
        &[
            Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_DST | buffer_usage::COPY_SRC)),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: vec![0xFF; 8],
            },
            Cmd::Submit(CommandBuffer {
                // fill bytes [2, 5): three bytes, pattern DD CC BB tiled from the region start.
                encoder: vec![Enc::FillBuffer {
                    buffer: 1,
                    offset: 2,
                    size: 3,
                    value: 0xAABB_CCDD,
                }],
                signal: None,
            }),
        ],
    );
    let mut out = [0u8; 8];
    exec.read_buffer(&s.resources, BufferId(1), 0, &mut out)
        .unwrap();
    assert_eq!(out, [0xFF, 0xFF, 0xDD, 0xCC, 0xBB, 0xFF, 0xFF, 0xFF]);
}

#[test]
fn fill_buffer_round_trips_through_codec() {
    // The new encoder op survives encode → decode unchanged (additive wire round-trip).
    use hl_gpu::{decode_stream, encode_stream};
    let cmds = vec![Cmd::Submit(CommandBuffer {
        encoder: vec![Enc::FillBuffer {
            buffer: 7,
            offset: 16,
            size: 64,
            value: 0x1234_5678,
        }],
        signal: None,
    })];
    assert_eq!(decode_stream(&encode_stream(&cmds)).unwrap(), cmds);
}

// -------------------------------------------------------------------------------------------------
// compute dispatch — kernel-IR programs executed on the CPU oracle
// -------------------------------------------------------------------------------------------------

/// The compiled kernel IR for `store_one`: store the constant `1.0f` into the single global pointer
/// argument. Equivalent to what a driver's PTX front-end would emit for the `STORE_ONE_PTX` in
/// `hl-gpu/tests/conformance.rs` (registers: rd1=0, rd2=1, f1=2).
fn store_one_program() -> KernelProgram {
    KernelProgram {
        entry: "store_one".into(),
        block: [1, 1, 1],
        params: vec![Param {
            width: 8,
            offset: 0,
            is_ptr: true,
            region: 0,
        }],
        param_bytes: 8,
        num_regions: 1,
        shared_bytes: 0,
        reg_count: 3,
        insts: vec![
            Inst::LdParam { d: 0, param: 0 },
            Inst::Cvta { d: 1, s: 0 },
            Inst::MovImmF {
                d: 2,
                bits: 0x3F80_0000,
            }, // 1.0f
            Inst::StGlobal {
                addr: 1,
                off: 0,
                src: Op::Reg(2),
                ty: gty::F32,
            },
            Inst::Ret,
        ],
    }
}

/// The compiled kernel IR for the canonical `vecadd(a, b, c, n)`: `c[i] = a[i] + b[i]` with the standard
/// `i = blockIdx*blockDim + tid` index and an `if (i >= n) return;` bounds guard. Equivalent to what a
/// driver's PTX front-end emits for `VECADD_PTX` (three pointer params → regions 0,1,2; scalar n at param
/// offset 24; register interning order gives reg_count 19; the guard branches to the `ret` at index 21).
fn vecadd_program() -> KernelProgram {
    KernelProgram {
        entry: "vecadd".into(),
        block: [4, 1, 1],
        params: vec![
            Param {
                width: 8,
                offset: 0,
                is_ptr: true,
                region: 0,
            }, // a
            Param {
                width: 8,
                offset: 8,
                is_ptr: true,
                region: 1,
            }, // b
            Param {
                width: 8,
                offset: 16,
                is_ptr: true,
                region: 2,
            }, // c
            Param {
                width: 4,
                offset: 24,
                is_ptr: false,
                region: 0,
            }, // n (scalar)
        ],
        param_bytes: 28,
        num_regions: 3,
        shared_bytes: 0,
        reg_count: 19,
        insts: vec![
            Inst::LdParam { d: 0, param: 0 }, // rd1 = a
            Inst::LdParam { d: 1, param: 1 }, // rd2 = b
            Inst::LdParam { d: 2, param: 2 }, // rd3 = c
            Inst::LdParam { d: 3, param: 3 }, // r2  = n
            Inst::MovSReg {
                d: 4,
                sreg: SR_NTID_X,
            }, // r3 = ntid.x
            Inst::MovSReg {
                d: 5,
                sreg: SR_CTAID_X,
            }, // r4 = ctaid.x
            Inst::MovSReg {
                d: 6,
                sreg: SR_TID_X,
            }, // r5 = tid.x
            Inst::IMad {
                d: 7,
                a: Op::Reg(5),
                b: Op::Reg(4),
                c: Op::Reg(6),
            }, // r1 = r4*r3 + r5
            Inst::Setp {
                d: 8,
                a: Op::Reg(7),
                b: Op::Reg(3),
                cmp: CMP_GE,
                unsigned: false,
            }, // p1 = i>=n
            Inst::Bra {
                target: 21,
                pred: Some((8, false)),
            }, // @p1 -> ret
            Inst::Cvta { d: 9, s: 0 },        // rd4 = global(a)
            Inst::IMul {
                d: 10,
                a: Op::Reg(7),
                b: Op::ImmI(4),
                wide: true,
                unsigned: false,
            }, // rd5 = i*4
            Inst::IAdd {
                d: 11,
                a: Op::Reg(9),
                b: Op::Reg(10),
                wide: true,
            }, // rd6 = &a[i]
            Inst::Cvta { d: 12, s: 1 },       // rd7 = global(b)
            Inst::IAdd {
                d: 13,
                a: Op::Reg(12),
                b: Op::Reg(10),
                wide: true,
            }, // rd8 = &b[i]
            Inst::LdGlobal {
                d: 14,
                addr: 13,
                off: 0,
                ty: gty::F32,
            }, // f1 = b[i]
            Inst::LdGlobal {
                d: 15,
                addr: 11,
                off: 0,
                ty: gty::F32,
            }, // f2 = a[i]
            Inst::FAdd {
                d: 16,
                a: Op::Reg(15),
                b: Op::Reg(14),
            }, // f3 = a[i]+b[i]
            Inst::Cvta { d: 17, s: 2 },       // rd9 = global(c)
            Inst::IAdd {
                d: 18,
                a: Op::Reg(17),
                b: Op::Reg(10),
                wide: true,
            }, // rd10 = &c[i]
            Inst::StGlobal {
                addr: 18,
                off: 0,
                src: Op::Reg(16),
                ty: gty::F32,
            }, // c[i] = f3
            Inst::Ret,
        ],
    }
}

#[test]
fn compute_dispatch_writes_constant_into_buffer() {
    let mut exec = hl_gpu::CpuExecutor::new();
    exec.define_kernel(1, store_one_program());
    let s = run_batch(
        &mut exec,
        &[
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::PtxKernel,
                spirv: kernel_words(),
            },
            Cmd::CreateComputePipeline(
                1,
                ComputePipelineDesc {
                    compute: ShaderRef {
                        module: 1,
                        entry: "store_one".into(),
                    },
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
                        BindEntry {
                            binding: 0,
                            resource: BindResource::Buffer {
                                id: 1,
                                offset: 0,
                                size: 8,
                            },
                        },
                        BindEntry {
                            binding: 1,
                            resource: BindResource::Buffer {
                                id: 2,
                                offset: 0,
                                size: 4,
                            },
                        },
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
    let mut out = [0u8; 4];
    exec.read_buffer(&s.resources, BufferId(2), 0, &mut out)
        .unwrap();
    assert_eq!(
        f32::from_le_bytes(out),
        1.0,
        "kernel must store 1.0f into region 0"
    );
}

#[test]
fn compute_vecadd_elementwise() {
    let n = 4u32;
    let a = [1.0f32, 2.0, 3.0, 4.0];
    let b = [10.0f32, 20.0, 30.0, 40.0];
    let to_bytes = |v: &[f32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();

    // Param blob: three u64 pointers (ignored by the interpreter) then n at offset 24.
    let mut param = vec![0u8; 28];
    param[24..28].copy_from_slice(&n.to_le_bytes());

    let mut exec = hl_gpu::CpuExecutor::new();
    exec.define_kernel(1, vecadd_program());
    let s = run_batch(
        &mut exec,
        &[
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::PtxKernel,
                spirv: kernel_words(),
            },
            Cmd::CreateComputePipeline(
                1,
                ComputePipelineDesc {
                    compute: ShaderRef {
                        module: 1,
                        entry: "vecadd".into(),
                    },
                    label: String::new(),
                },
            ),
            Cmd::CreateBuffer(1, buf(28, buffer_usage::STORAGE)), // params (binding 0)
            Cmd::CreateBuffer(2, buf(16, buffer_usage::STORAGE)), // a -> region 0 (binding 1)
            Cmd::CreateBuffer(3, buf(16, buffer_usage::STORAGE)), // b -> region 1 (binding 2)
            Cmd::CreateBuffer(4, buf(16, buffer_usage::STORAGE | buffer_usage::COPY_SRC)), // c (binding 3)
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: param,
            },
            Cmd::WriteBuffer {
                id: 2,
                offset: 0,
                data: to_bytes(&a),
            },
            Cmd::WriteBuffer {
                id: 3,
                offset: 0,
                data: to_bytes(&b),
            },
            Cmd::CreateBindGroup(
                1,
                BindGroupDesc {
                    set: 0,
                    entries: vec![
                        BindEntry {
                            binding: 0,
                            resource: BindResource::Buffer {
                                id: 1,
                                offset: 0,
                                size: 28,
                            },
                        },
                        BindEntry {
                            binding: 1,
                            resource: BindResource::Buffer {
                                id: 2,
                                offset: 0,
                                size: 16,
                            },
                        },
                        BindEntry {
                            binding: 2,
                            resource: BindResource::Buffer {
                                id: 3,
                                offset: 0,
                                size: 16,
                            },
                        },
                        BindEntry {
                            binding: 3,
                            resource: BindResource::Buffer {
                                id: 4,
                                offset: 0,
                                size: 16,
                            },
                        },
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
    let mut out = [0u8; 16];
    exec.read_buffer(&s.resources, BufferId(4), 0, &mut out)
        .unwrap();
    let got: Vec<f32> = out
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    assert_eq!(got, vec![11.0, 22.0, 33.0, 44.0]);
}
