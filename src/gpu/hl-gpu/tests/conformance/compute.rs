use super::*;

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
