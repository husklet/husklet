use super::*;

// (3) COMPUTE — multi-workgroup index math, boundary size, atomics, zero dispatch
// =================================================================================================

/// `out[gid] = gid` for `gid < n`, with `gid = ctaid.x*ntid.x + tid.x`. Exercises SR_CTAID/NTID/TID,
/// a predicated early-out, pointer arithmetic, and a U32 global store across multiple workgroups.
fn iota_program() -> KernelProgram {
    KernelProgram {
        entry: "iota".into(),
        block: [8, 1, 1],
        params: vec![
            Param {
                width: 8,
                offset: 0,
                is_ptr: true,
                region: 0,
            },
            Param {
                width: 4,
                offset: 8,
                is_ptr: false,
                region: 0,
            },
        ],
        param_bytes: 12,
        num_regions: 1,
        shared_bytes: 0,
        reg_count: 10,
        insts: vec![
            Inst::LdParam { d: 0, param: 0 }, // 0: out ptr
            Inst::LdParam { d: 1, param: 1 }, // 1: n
            Inst::MovSReg {
                d: 2,
                sreg: SR_NTID_X,
            }, // 2
            Inst::MovSReg {
                d: 3,
                sreg: SR_CTAID_X,
            }, // 3
            Inst::MovSReg {
                d: 4,
                sreg: SR_TID_X,
            }, // 4
            Inst::IMad {
                d: 5,
                a: Op::Reg(3),
                b: Op::Reg(2),
                c: Op::Reg(4),
            }, // 5: gid
            Inst::Setp {
                d: 6,
                a: Op::Reg(5),
                b: Op::Reg(1),
                cmp: CMP_GE,
                unsigned: true,
            }, // 6: gid>=n
            Inst::Bra {
                target: 12,
                pred: Some((6, false)),
            }, // 7: if true → Ret
            Inst::Cvta { d: 7, s: 0 },        // 8: base
            Inst::IMul {
                d: 8,
                a: Op::Reg(5),
                b: Op::ImmI(4),
                wide: true,
                unsigned: false,
            }, // 9: gid*4
            Inst::IAdd {
                d: 9,
                a: Op::Reg(7),
                b: Op::Reg(8),
                wide: true,
            }, // 10: addr
            Inst::StGlobal {
                addr: 9,
                off: 0,
                src: Op::Reg(5),
                ty: gty::U32,
            }, // 11
            Inst::Ret,                        // 12
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
                        entry: "iota".into(),
                    },
                    label: String::new(),
                },
            ),
            Cmd::CreateBuffer(1, buf(12, buffer_usage::STORAGE)),
            Cmd::CreateBuffer(
                2,
                buf(
                    (n * 4) as u64,
                    buffer_usage::STORAGE | buffer_usage::COPY_SRC,
                ),
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: param,
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
                                size: 12,
                            },
                        },
                        BindEntry {
                            binding: 1,
                            resource: BindResource::Buffer {
                                id: 2,
                                offset: 0,
                                size: (n * 4) as u64,
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
                    Enc::Dispatch { x: 3, y: 1, z: 1 }, // ceil(20/8) = 3 workgroups
                    Enc::EndComputePass,
                ],
                signal: None,
            }),
        ],
    );
    let out = g
        .read_buffer(&s.resources, BufferId(2), 0, (n * 4) as usize)
        .unwrap();
    let got: Vec<u32> = out
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    assert_eq!(
        got,
        (0..n).collect::<Vec<_>>(),
        "each thread writes its global index; out-of-range guarded"
    );
}

/// Every thread atomically increments counter[0]; the total must equal the launched thread count.
pub(super) fn atomic_counter_program() -> KernelProgram {
    KernelProgram {
        entry: "counter".into(),
        block: [8, 1, 1],
        params: vec![Param {
            width: 8,
            offset: 0,
            is_ptr: true,
            region: 0,
        }],
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
                        entry: "counter".into(),
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
                    Enc::Dispatch { x: 4, y: 1, z: 1 }, // 4 workgroups * 8 threads = 32
                    Enc::EndComputePass,
                ],
                signal: None,
            }),
        ],
    );
    let out = g.read_buffer(&s.resources, BufferId(2), 0, 4).unwrap();
    assert_eq!(
        u32::from_le_bytes(out.try_into().unwrap()),
        32,
        "32 atomic increments"
    );
}

#[test]
fn compute_zero_dispatch_is_noop_not_panic() {
    let mut g = exec();
    g.define_kernel(1, atomic_counter_program());
    let s = run_batch(
        &mut g,
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
                        entry: "counter".into(),
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
                    Enc::Dispatch { x: 0, y: 1, z: 1 }, // zero workgroups
                    Enc::EndComputePass,
                ],
                signal: None,
            }),
        ],
    );
    let out = g.read_buffer(&s.resources, BufferId(2), 0, 4).unwrap();
    assert_eq!(
        u32::from_le_bytes(out.try_into().unwrap()),
        0,
        "no workgroups → counter untouched"
    );
}

// =================================================================================================
