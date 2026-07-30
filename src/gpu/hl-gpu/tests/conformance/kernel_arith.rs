//! Conformance for the kernel IR's FLOAT comparison and the int↔float conversions — the arithmetic a guest
//! front end could not express before and had to either mis-lower or reject.
//!
//! Every case runs a one-thread kernel that reads its operands out of device memory (so nothing is folded
//! at build time), computes a single value, and stores it where the assertion reads it back. The values are
//! chosen so a wrong lowering produces a DIFFERENT number, not merely a less precise one:
//!
//! * negative operands, because comparing float bit patterns as signed integers agrees with float ordering
//!   only while both operands are non-negative and inverts below zero;
//! * `NaN`, because the ordered and unordered comparison families differ only there;
//! * a value ≥ 2^31, because reading it through the SIGNED int→float conversion yields a negative float;
//! * `2.5` and `3.5`, because round-to-nearest-TIES-TO-EVEN is the only rounding that gives 2 and 4.

use super::*;

/// A one-thread kernel over two pointer params: region 0 is a `[f32; 2]`/`[u32; 2]` input, region 1 is the
/// single-value output. `body` receives registers 4 and 5 preloaded with the two inputs (as `ty`) and must
/// leave its result in register 7; the harness appends the store and the return.
fn probe(ty: u8, body: Vec<Inst>) -> KernelProgram {
    let mut insts = vec![
        Inst::LdParam { d: 0, param: 0 },
        Inst::Cvta { d: 1, s: 0 },
        Inst::LdParam { d: 2, param: 1 },
        Inst::Cvta { d: 3, s: 2 },
        Inst::LdGlobal {
            d: 4,
            addr: 1,
            off: 0,
            ty,
        },
        Inst::LdGlobal {
            d: 5,
            addr: 1,
            off: 4,
            ty,
        },
    ];
    insts.extend(body);
    insts.push(Inst::StGlobal {
        addr: 3,
        off: 0,
        src: Op::Reg(7),
        ty,
    });
    insts.push(Inst::Ret);
    KernelProgram {
        entry: "probe".into(),
        block: [1, 1, 1],
        params: vec![
            Param {
                width: 8,
                offset: 0,
                is_ptr: true,
                region: 0,
            },
            Param {
                width: 8,
                offset: 8,
                is_ptr: true,
                region: 1,
            },
        ],
        param_bytes: 16,
        num_regions: 2,
        shared_bytes: 0,
        reg_count: 8,
        insts,
    }
}

/// A predicate probe: `p = cmp(in[0], in[1])`, storing `1.0` when true and `0.0` when false.
fn predicate_probe(cmp: u8, ordered: bool) -> KernelProgram {
    probe(
        gty::F32,
        vec![
            Inst::FSetp {
                d: 6,
                a: Op::Reg(4),
                b: Op::Reg(5),
                cmp,
                ordered,
            },
            Inst::MovImmF { d: 7, bits: 0 },
            // Skip the "true" store when the predicate is false.
            Inst::Bra {
                target: 10,
                pred: Some((6, true)),
            },
            Inst::MovImmF {
                d: 7,
                bits: 0x3F80_0000,
            },
        ],
    )
}

/// Run `program` over the two 4-byte input words and return the 4-byte result.
fn run(program: KernelProgram, in0: [u8; 4], in1: [u8; 4]) -> [u8; 4] {
    let mut exec = hl_gpu::CpuExecutor::new();
    exec.define_kernel(1, program);
    let mut data = in0.to_vec();
    data.extend_from_slice(&in1);
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
                        entry: "probe".into(),
                    },
                    label: String::new(),
                },
            ),
            // Binding 0 is the kernel's flat parameter blob; 1 and 2 are its two pointer regions.
            Cmd::CreateBuffer(1, buf(16, buffer_usage::STORAGE)),
            Cmd::CreateBuffer(2, buf(8, buffer_usage::STORAGE | buffer_usage::COPY_DST)),
            Cmd::CreateBuffer(3, buf(4, buffer_usage::STORAGE | buffer_usage::COPY_SRC)),
            Cmd::WriteBuffer {
                id: 2,
                offset: 0,
                data,
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
                                size: 16,
                            },
                        },
                        BindEntry {
                            binding: 1,
                            resource: BindResource::Buffer {
                                id: 2,
                                offset: 0,
                                size: 8,
                            },
                        },
                        BindEntry {
                            binding: 2,
                            resource: BindResource::Buffer {
                                id: 3,
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
    exec.read_buffer(&s.resources, BufferId(3), 0, &mut out)
        .unwrap();
    out
}

fn compare(cmp: u8, ordered: bool, x: f32, y: f32) -> bool {
    let out = run(
        predicate_probe(cmp, ordered),
        x.to_le_bytes(),
        y.to_le_bytes(),
    );
    f32::from_le_bytes(out) == 1.0
}

// ---------------------------------------------------------------------------------------------------
// float comparison
// ---------------------------------------------------------------------------------------------------

/// The defect that made this variant necessary: a float comparison lowered onto the INTEGER `Setp`
/// compares bit patterns, which inverts once an operand is negative. `-1.0 < -2.0` is false, but the
/// integer compare of `0xBF800000` and `0xC0000000` as `i32` reports true — so an ordinary `if (x < y)`
/// took the opposite branch and still returned success.
#[test]
fn float_comparison_orders_negative_operands_correctly() {
    assert!(!compare(CMP_LT, true, -1.0, -2.0), "-1.0 < -2.0 is false");
    assert!(compare(CMP_LT, true, -2.0, -1.0), "-2.0 < -1.0 is true");
    assert!(compare(CMP_GT, true, -1.0, -2.0), "-1.0 > -2.0 is true");
    // Mixed signs and the all-positive case the bit trick happened to get right.
    assert!(compare(CMP_LT, true, -1.0, 1.0));
    assert!(compare(CMP_LT, true, 1.0, 2.0));
    assert!(!compare(CMP_LT, true, 2.0, 1.0));
    // Zeroes compare equal across signs, as IEEE-754 requires (and as a bit compare would not).
    assert!(compare(CMP_EQ, true, -0.0, 0.0), "-0.0 == 0.0");
    assert!(!compare(CMP_LT, true, -0.0, 0.0), "-0.0 < 0.0 is false");
}

/// The ordered and unordered families differ only at NaN, and a front end needs both because a source-level
/// `!(x < y)` lowers to the UNORDERED `ge`, not to the ordered one.
#[test]
fn ordered_and_unordered_comparisons_differ_only_at_nan() {
    let nan = f32::NAN;
    for cmp in [CMP_EQ, CMP_LT, CMP_LE, CMP_GT, CMP_GE] {
        assert!(!compare(cmp, true, nan, 1.0), "ordered compare with NaN");
        assert!(compare(cmp, false, nan, 1.0), "unordered compare with NaN");
    }
    // Not-equal is the asymmetric one: ordered `ne` is FALSE for NaN (unlike Rust's `!=`), unordered is true.
    assert!(!compare(CMP_NE, true, nan, 1.0), "ordered ne with NaN");
    assert!(compare(CMP_NE, false, nan, 1.0), "unordered ne with NaN");
    // Away from NaN the two families agree exactly.
    for cmp in [CMP_EQ, CMP_NE, CMP_LT, CMP_LE, CMP_GT, CMP_GE] {
        assert_eq!(
            compare(cmp, true, -3.0, 2.0),
            compare(cmp, false, -3.0, 2.0),
            "the families must agree when neither operand is NaN"
        );
    }
}

// ---------------------------------------------------------------------------------------------------
// int <-> float conversion
// ---------------------------------------------------------------------------------------------------

fn convert(kind: u8, in_ty: u8, out_ty: u8, word: [u8; 4]) -> [u8; 4] {
    // Read the operand as `in_ty`, convert, store as `out_ty`. `probe` stores register 7 as its `ty`, so the
    // conversion probe is built directly rather than through `predicate_probe`.
    let mut program = probe(
        in_ty,
        vec![Inst::Cvt {
            d: 7,
            s: Op::Reg(4),
            kind,
        }],
    );
    let last = program.insts.len() - 2;
    program.insts[last] = Inst::StGlobal {
        addr: 3,
        off: 0,
        src: Op::Reg(7),
        ty: out_ty,
    };
    run(program, word, [0; 4])
}

/// `(float)someUnsigned` needs its own conversion: routing it through the SIGNED int→float kind reads any
/// value at or above 2^31 as negative. Before this kind existed the pair fell through to a bit-preserving
/// move, which handed the kernel the integer's bits reinterpreted as a float — for `0x80000000`, `-0.0`.
#[test]
fn unsigned_int_to_float_does_not_go_negative() {
    let big = 0x8000_0000u32; // 2_147_483_648
    let got = f32::from_le_bytes(convert(CVT_F32_FROM_U32, gty::U32, gty::F32, big.to_le_bytes()));
    assert_eq!(got, 2_147_483_648.0);

    // The signed kind is still available and still signed — the two are genuinely different conversions.
    let signed =
        f32::from_le_bytes(convert(CVT_F32_FROM_S32, gty::U32, gty::F32, big.to_le_bytes()));
    assert_eq!(signed, -2_147_483_648.0);
}

/// Float → unsigned, which likewise had no kind of its own.
#[test]
fn float_to_unsigned_int_truncates_toward_zero() {
    let got = |v: f32| {
        u32::from_le_bytes(convert(
            CVT_U32_FROM_F32,
            gty::F32,
            gty::U32,
            v.to_le_bytes(),
        ))
    };
    assert_eq!(got(3.9), 3);
    assert_eq!(got(3.1), 3);
    // Out-of-range clamps rather than wrapping, matching PTX's unsaturated float→int conversion.
    assert_eq!(got(-1.0), 0);
}

/// `cvt.rni` (round to nearest, ties to even) and `cvt.rzi` (truncate) were collapsed onto one truncating
/// conversion, so every round-to-nearest silently truncated. `2.5` and `3.5` both round to EVEN under
/// `rni` — to 2 and 4 — which no truncation and no round-half-up can reproduce.
#[test]
fn round_to_nearest_even_is_distinct_from_truncation() {
    let rni = |v: f32| {
        i32::from_le_bytes(convert(
            CVT_S32_FROM_F32_RNI,
            gty::F32,
            gty::U32,
            v.to_le_bytes(),
        ))
    };
    let rzi = |v: f32| {
        i32::from_le_bytes(convert(
            CVT_S32_FROM_F32,
            gty::F32,
            gty::U32,
            v.to_le_bytes(),
        ))
    };

    assert_eq!((rni(2.5), rzi(2.5)), (2, 2), "2.5 ties down to even 2");
    assert_eq!((rni(3.5), rzi(3.5)), (4, 3), "3.5 ties UP to even 4");
    assert_eq!((rni(2.9), rzi(2.9)), (3, 2), "the plain rounding difference");
    assert_eq!((rni(-2.5), rzi(-2.5)), (-2, -2));
    assert_eq!((rni(-3.5), rzi(-3.5)), (-4, -3));

    // The unsigned round-to-nearest kind behaves the same way.
    let urni = |v: f32| {
        u32::from_le_bytes(convert(
            CVT_U32_FROM_F32_RNI,
            gty::F32,
            gty::U32,
            v.to_le_bytes(),
        ))
    };
    assert_eq!((urni(2.5), urni(3.5)), (2, 4));
}

/// A conversion kind the executor does not implement must be REJECTED. The old fallback silently
/// reinterpreted the bits, which is how an unmatched pair produced garbage instead of an error.
#[test]
fn an_unknown_conversion_kind_is_rejected_not_reinterpreted() {
    let mut exec = hl_gpu::CpuExecutor::new();
    exec.define_kernel(
        1,
        probe(
            gty::F32,
            vec![Inst::Cvt {
                d: 7,
                s: Op::Reg(4),
                kind: 200,
            }],
        ),
    );
    let mut resources = hl_gpu::SessionResources::new();
    let err = exec
        .execute(
            &mut resources,
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
                            entry: "probe".into(),
                        },
                        label: String::new(),
                    },
                ),
                Cmd::CreateBuffer(1, buf(16, buffer_usage::STORAGE)),
                Cmd::CreateBuffer(2, buf(8, buffer_usage::STORAGE)),
                Cmd::CreateBuffer(3, buf(4, buffer_usage::STORAGE)),
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
                                    size: 16,
                                },
                            },
                            BindEntry {
                                binding: 1,
                                resource: BindResource::Buffer {
                                    id: 2,
                                    offset: 0,
                                    size: 8,
                                },
                            },
                            BindEntry {
                                binding: 2,
                                resource: BindResource::Buffer {
                                    id: 3,
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
        )
        .unwrap_err();
    assert_eq!(
        err,
        hl_gpu::GpuError::Kernel("kernel: unsupported cvt kind 200".into())
    );
}
