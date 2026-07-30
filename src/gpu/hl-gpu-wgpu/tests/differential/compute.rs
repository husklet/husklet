use super::*;

/// (18) COMPUTE `iota`: `out[gid] = gid` for `gid < n`, driven by the SAME neutral kernel-IR
/// (`KernelProgram`) on both backends — the CPU interpreter and the wgpu WGSL-lowered compute. EXACT.
/// COMPUTE float compare on NEGATIVE operands: `out[gid] = (-f32(gid) < -1.0) ? 1 : 0`, driven by the
/// same neutral `KernelProgram` on both backends. This is the case a signed-integer compare of the float
/// BIT PATTERNS gets wrong — IEEE-754 magnitude ordering runs backwards once the sign bit is set — so it
/// pins `Inst::FSetp` against the CPU interpreter where `Inst::Setp` would silently invert. EXACT.
pub(super) fn gen_compute_fcmp(seed: u64) -> Prog {
    let n = 8 + (seed % 25) as u32; // 8..=32
    let mut param = vec![0u8; 12];
    param[8..12].copy_from_slice(&n.to_le_bytes());
    let groups = n.div_ceil(8);
    let cmds = vec![
        Cmd::CreateShader {
            id: 1,
            kind: ShaderPayloadKind::PtxKernel,
            spirv: vec![KERNEL_MAGIC, 0],
        },
        Cmd::CreateComputePipeline(
            1,
            ComputePipelineDesc {
                compute: ShaderRef {
                    module: 1,
                    entry: "fcmp".into(),
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
                Enc::Dispatch {
                    x: groups,
                    y: 1,
                    z: 1,
                },
                Enc::EndComputePass,
            ],
            signal: None,
        }),
    ];
    Prog {
        seed,
        category: "compute_fcmp",
        ops: vec!["BeginComputePass", "Dispatch", "EndComputePass"],
        cmds,
        read: Read::Buf {
            id: 2,
            offset: 0,
            len: (n * 4) as usize,
        },
        tol: 0,
        kernel: Some((1, fcmp_program())),
    }
}

pub(super) fn gen_compute_iota(seed: u64) -> Prog {
    let n = 8 + (seed % 25) as u32; // 8..=32
    let mut param = vec![0u8; 12];
    param[8..12].copy_from_slice(&n.to_le_bytes());
    let groups = n.div_ceil(8);
    let cmds = vec![
        Cmd::CreateShader {
            id: 1,
            kind: ShaderPayloadKind::PtxKernel,
            spirv: vec![KERNEL_MAGIC, 0],
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
                Enc::Dispatch {
                    x: groups,
                    y: 1,
                    z: 1,
                },
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
        read: Read::Buf {
            id: 2,
            offset: 0,
            len: (n * 4) as usize,
        },
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
            Inst::LdParam { d: 0, param: 0 },
            Inst::LdParam { d: 1, param: 1 },
            Inst::MovSReg {
                d: 2,
                sreg: SR_NTID_X,
            },
            Inst::MovSReg {
                d: 3,
                sreg: SR_CTAID_X,
            },
            Inst::MovSReg {
                d: 4,
                sreg: SR_TID_X,
            },
            Inst::IMad {
                d: 5,
                a: Op::Reg(3),
                b: Op::Reg(2),
                c: Op::Reg(4),
            },
            Inst::Setp {
                d: 6,
                a: Op::Reg(5),
                b: Op::Reg(1),
                cmp: CMP_GE,
                unsigned: true,
            },
            Inst::Bra {
                target: 12,
                pred: Some((6, false)),
            },
            Inst::Cvta { d: 7, s: 0 },
            Inst::IMul {
                d: 8,
                a: Op::Reg(5),
                b: Op::ImmI(4),
                wide: true,
                unsigned: false,
            },
            Inst::IAdd {
                d: 9,
                a: Op::Reg(7),
                b: Op::Reg(8),
                wide: true,
            },
            Inst::StGlobal {
                addr: 9,
                off: 0,
                src: Op::Reg(5),
                ty: gty::U32,
            },
            Inst::Ret,
        ],
    }
}

/// `out[gid] = (-f32(gid) < -1.0) ? 1 : 0` for `gid < n`. Expected: `0` for `gid` 0 and 1, `1` beyond —
/// the ordered `setp.lt.f32` family. Comparing the operands' bit patterns as signed integers instead
/// inverts the result for these negative values, which is the defect this pins.
fn fcmp_program() -> KernelProgram {
    const NEG_ONE_BITS: u32 = 0xBF80_0000; // -1.0f32
    const ZERO_BITS: u32 = 0x0000_0000; // 0.0f32
    KernelProgram {
        entry: "fcmp".into(),
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
        reg_count: 16,
        insts: vec![
            Inst::LdParam { d: 0, param: 0 },
            Inst::LdParam { d: 1, param: 1 },
            Inst::MovSReg {
                d: 2,
                sreg: SR_NTID_X,
            },
            Inst::MovSReg {
                d: 3,
                sreg: SR_CTAID_X,
            },
            Inst::MovSReg {
                d: 4,
                sreg: SR_TID_X,
            },
            Inst::IMad {
                d: 5,
                a: Op::Reg(3),
                b: Op::Reg(2),
                c: Op::Reg(4),
            },
            Inst::Setp {
                d: 6,
                a: Op::Reg(5),
                b: Op::Reg(1),
                cmp: CMP_GE,
                unsigned: true,
            },
            Inst::Bra {
                target: 15,
                pred: Some((6, false)),
            },
            // f = f32(gid); neg = 0.0 - f  (a genuinely negative operand for gid > 0)
            Inst::Cvt {
                d: 10,
                s: Op::Reg(5),
                kind: CVT_F32_FROM_U32,
            },
            Inst::FSub {
                d: 11,
                a: Op::ImmF(ZERO_BITS),
                b: Op::Reg(10),
            },
            Inst::FSetp {
                d: 12,
                a: Op::Reg(11),
                b: Op::ImmF(NEG_ONE_BITS),
                cmp: CMP_LT,
                ordered: true,
            },
            Inst::Cvta { d: 7, s: 0 },
            Inst::IMul {
                d: 8,
                a: Op::Reg(5),
                b: Op::ImmI(4),
                wide: true,
                unsigned: false,
            },
            Inst::IAdd {
                d: 9,
                a: Op::Reg(7),
                b: Op::Reg(8),
                wide: true,
            },
            Inst::StGlobal {
                addr: 9,
                off: 0,
                src: Op::Reg(12),
                ty: gty::U32,
            },
            Inst::Ret,
        ],
    }
}
