use super::{BoundaryCapture, Executor, Exit, Projection, ProjectionView, Source, SourceSpan};
use hl_execution::{
    Aarch64CpuState, AtomicOperation, AtomicValue, CpuState as X86CpuState, EXECUTION_SNAPSHOT_VERSION, ExclusiveLoad,
    ExclusiveMemory, ExclusiveReservation, ExecutionCpuSnapshot, ExecutionInstructionMemory, ExecutionMachine,
    ExecutionSnapshot, GuestOperandMemory, MappingGeneration, MemoryOrder, StepOutcome,
};
use hl_memory::Protection;
use std::path::PathBuf;

const MEMORY_LIMIT: usize = 8 << 20;

#[cfg(target_arch = "aarch64")]
#[test]
fn x86_memory_movq_unpack_matches_interpreter() {
    let bytes = [
        0xf3, 0x0f, 0x7e, 0x05, 0xf8, 0x0f, 0x00, 0x00, 0x66, 0x48, 0x0f, 0x6e, 0xc8, 0x66, 0x0f, 0x6c, 0xc1, 0x0f,
        0x05,
    ];
    let source = [super::BorrowedSource {
        guest_first: 0x4000,
        bytes: &bytes,
    }];
    let mut initial = X86CpuState {
        rip: 0x4000,
        ..Default::default()
    };
    initial.registers[0] = 0x8877_6655_4433_2211;
    initial.vectors[0] = u128::MAX;
    let mut native = initial.clone();
    let mut constant = 0x1122_3344_5566_7788_u64.to_le_bytes();
    let views = [ProjectionView {
        guest_first: 0x5000,
        guest_last: 0x5008,
        host_first: constant.as_mut_ptr() as usize as u64,
        mapping_incarnation: 1,
        permissions: 1,
        write_policy: super::WRITE_EXACT,
        write_index: 0,
    }];
    let projection = Projection {
        views: views.as_ptr(),
        count: views.len(),
        mapping_incarnation: 1,
        active: 0,
    };
    let executor = Executor::create().unwrap();
    let mut resolve = |_: u64, _: &mut [u8]| None;
    let outcome = executor
        .run_x86(&mut native, &source, 1, 1, 32, false, Some(&projection), &mut resolve)
        .unwrap()
        .0;
    assert_eq!(outcome.exit, Exit::Syscall);
    let expected = 0x8877_6655_4433_2211_1122_3344_5566_7788_u128;
    assert_eq!(native.vectors[0], expected);
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TraceCheckpoint {
    instruction_count: u64,
    cpu: Aarch64CpuState,
    memory: Vec<Vec<u8>>,
}

#[cfg(target_arch = "aarch64")]
#[test]
fn x86_strsearch_arithmetic_loop_matches_interpreter_at_each_boundary() {
    let bytes = [
        0x48, 0x89, 0xca, 0x48, 0x83, 0xc6, 0x01, 0x48, 0xc1, 0xe2, 0x0d, 0x48, 0x31, 0xca, 0x48, 0x89, 0xd0, 0x48,
        0xc1, 0xe8, 0x07, 0x48, 0x31, 0xd0, 0x48, 0x89, 0xc1, 0x48, 0xc1, 0xe1, 0x11, 0x48, 0x31, 0xc1, 0x48, 0x89,
        0xc8, 0x48, 0xf7, 0xe7, 0x48, 0x89, 0xc8, 0x48, 0xc1, 0xea, 0x03, 0x48, 0x6b, 0xd2, 0x1a, 0x48, 0x29, 0xd0,
        0x83, 0xc0, 0x61, 0x88, 0x46, 0xff, 0x0f, 0x05,
    ];
    let instruction_ends = [3, 7, 11, 14, 17, 21, 24, 27, 31, 34, 37, 40, 43, 47, 51, 54, 57, 60];
    let initial = X86CpuState {
        rip: 0x402720,
        registers: [
            0x1111,
            0x0123_4567_89ab_cdef,
            0x2222,
            0,
            0,
            0,
            0x7001,
            0x4ec4_ec4e_c4ec_4ec5,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ],
        ..X86CpuState::default()
    };
    for (index, &end) in instruction_ends.iter().enumerate() {
        let mut prefix = bytes[..end].to_vec();
        prefix.extend_from_slice(&[0x0f, 0x05]);
        let source = [super::BorrowedSource {
            guest_first: 0x402720,
            bytes: &prefix,
        }];
        let mut native = initial.clone();
        let mut storage = [0xa5_u8; 8];
        let view = ProjectionView {
            guest_first: 0x7000,
            guest_last: 0x7008,
            host_first: storage.as_mut_ptr() as usize as u64,
            mapping_incarnation: 1,
            permissions: 3,
            write_policy: super::WRITE_EXACT,
            write_index: 0,
        };
        let projection = Projection {
            views: &raw const view,
            count: 1,
            mapping_incarnation: 1,
            active: 0,
        };
        let executor = Executor::create().expect("native executor");
        let mut resolve = |_: u64, _: &mut [u8]| None;
        let outcome = executor
            .run_x86(
                &mut native,
                &source,
                1,
                index as u64 + 1,
                64,
                false,
                Some(&projection),
                &mut resolve,
            )
            .unwrap()
            .0;
        assert_eq!(
            outcome.exit,
            Exit::Syscall,
            "boundary {} outcome {outcome:?}",
            index + 1
        );

        let mut memory = ReplayMemory {
            sources: vec![ReplaySource {
                first: 0x402720,
                bytes: prefix.clone(),
            }],
            data: vec![ReplayView {
                first: 0x7000,
                data: vec![0xa5; 8],
            }],
            generation: 1,
        };
        let machine = ExecutionMachine::new(ExecutionSnapshot {
            version: EXECUTION_SNAPSHOT_VERSION,
            cpu: ExecutionCpuSnapshot::X86_64(initial.clone()),
            cache_epoch: 1,
            fault: None,
        })
        .unwrap();
        let step = machine.run_slice(1, index as u64 + 3, &mut memory);
        assert!(
            matches!(step, StepOutcome::Syscall { .. }),
            "boundary {} step {step:?}",
            index + 1
        );
        machine.freeze().unwrap();
        let ExecutionCpuSnapshot::X86_64(interpreted) = machine.snapshot().unwrap().cpu else {
            unreachable!()
        };
        assert_eq!(
            native,
            interpreted,
            "first state divergence after instruction {} ending at {:#x}",
            index + 1,
            0x402720 + end
        );
        assert_eq!(
            storage.as_slice(),
            memory.data[0].data.as_slice(),
            "first byte divergence after instruction {} ending at {:#x}",
            index + 1,
            0x402720 + end
        );
    }
}

/// Legacy SSE2 integer min/max (`pminub`/`pmaxub`/`pminsw`/`pmaxsw`), in both the
/// register and the memory-operand form that `__strlen_sse2` uses.
#[cfg(target_arch = "aarch64")]
#[test]
fn x86_sse_integer_min_max_matches_interpreter_at_each_boundary() {
    let bytes = [
        0x66, 0x0f, 0xda, 0xc1, // pminub %xmm1,%xmm0
        0x66, 0x0f, 0xde, 0xc1, // pmaxub %xmm1,%xmm0
        0x66, 0x0f, 0xea, 0xc1, // pminsw %xmm1,%xmm0
        0x66, 0x0f, 0xee, 0xc1, // pmaxsw %xmm1,%xmm0
        0x66, 0x0f, 0xda, 0x06, // pminub (%rsi),%xmm0
        0x66, 0x0f, 0xde, 0x06, // pmaxub (%rsi),%xmm0
        0x66, 0x0f, 0xea, 0x06, // pminsw (%rsi),%xmm0
        0x66, 0x0f, 0xee, 0x06, // pmaxsw (%rsi),%xmm0
    ];
    let instruction_ends = [4, 8, 12, 16, 20, 24, 28, 32];
    let mut initial = X86CpuState {
        rip: 0x402720,
        ..X86CpuState::default()
    };
    initial.registers[6] = 0x7000;
    initial.vectors[0] = 0x00ff_7f80_0102_fefd_8000_7fff_1234_abcd;
    initial.vectors[1] = 0x8000_0180_fe03_0405_7fff_8000_abcd_1234;
    let operand: [u8; 16] = [
        0x00, 0xff, 0x7f, 0x80, 0x01, 0x02, 0xfe, 0xfd, 0x80, 0x00, 0x7f, 0xff, 0x12, 0x34, 0xab, 0xcd,
    ];
    for (index, &end) in instruction_ends.iter().enumerate() {
        let mut prefix = bytes[..end].to_vec();
        prefix.extend_from_slice(&[0x0f, 0x05]);
        let source = [super::BorrowedSource {
            guest_first: 0x402720,
            bytes: &prefix,
        }];
        let mut native = initial.clone();
        let mut storage = operand;
        let view = ProjectionView {
            guest_first: 0x7000,
            guest_last: 0x7010,
            host_first: storage.as_mut_ptr() as usize as u64,
            mapping_incarnation: 1,
            permissions: 3,
            write_policy: super::WRITE_EXACT,
            write_index: 0,
        };
        let projection = Projection {
            views: &raw const view,
            count: 1,
            mapping_incarnation: 1,
            active: 0,
        };
        let executor = Executor::create().expect("native executor");
        let mut resolve = |_: u64, _: &mut [u8]| None;
        let outcome = executor
            .run_x86(
                &mut native,
                &source,
                1,
                index as u64 + 1,
                64,
                false,
                Some(&projection),
                &mut resolve,
            )
            .unwrap()
            .0;
        assert_eq!(outcome.exit, Exit::Syscall, "boundary {} outcome {outcome:?}", index + 1);

        let mut memory = ReplayMemory {
            sources: vec![ReplaySource {
                first: 0x402720,
                bytes: prefix.clone(),
            }],
            data: vec![ReplayView {
                first: 0x7000,
                data: operand.to_vec(),
            }],
            generation: 1,
        };
        let machine = ExecutionMachine::new(ExecutionSnapshot {
            version: EXECUTION_SNAPSHOT_VERSION,
            cpu: ExecutionCpuSnapshot::X86_64(initial.clone()),
            cache_epoch: 1,
            fault: None,
        })
        .unwrap();
        let step = machine.run_slice(1, index as u64 + 3, &mut memory);
        assert!(matches!(step, StepOutcome::Syscall { .. }), "boundary {} step {step:?}", index + 1);
        machine.freeze().unwrap();
        let ExecutionCpuSnapshot::X86_64(interpreted) = machine.snapshot().unwrap().cpu else {
            unreachable!()
        };
        assert_eq!(
            native,
            interpreted,
            "first state divergence after instruction {} ending at {:#x}",
            index + 1,
            0x402720 + end
        );
    }
}

/// Legacy packed single/dword conversions (`cvtdq2ps`/`cvtps2dq`/`cvttps2dq`), in
/// both the register and the memory-operand form, across saturation and NaN inputs.
#[cfg(target_arch = "aarch64")]
#[test]
fn x86_sse_packed_float_dword_conversions_match_interpreter_at_each_boundary() {
    let bytes = [
        0x0f, 0x5b, 0xc1, // cvtdq2ps %xmm1,%xmm0
        0x66, 0x0f, 0x5b, 0xc1, // cvtps2dq %xmm1,%xmm0
        0xf3, 0x0f, 0x5b, 0xc1, // cvttps2dq %xmm1,%xmm0
        0x0f, 0x5b, 0x06, // cvtdq2ps (%rsi),%xmm0
        0x66, 0x0f, 0x5b, 0x06, // cvtps2dq (%rsi),%xmm0
        0xf3, 0x0f, 0x5b, 0x06, // cvttps2dq (%rsi),%xmm0
    ];
    let instruction_ends = [3, 7, 11, 14, 18, 22];
    let mut initial = X86CpuState {
        rip: 0x402720,
        ..X86CpuState::default()
    };
    initial.registers[6] = 0x7000;
    initial.vectors[0] = 0x1111_2222_3333_4444_5555_6666_7777_8888;
    // 2^31 (saturates), -2^31 (exact), NaN, and 1.5 -- also a sane dword pattern.
    initial.vectors[1] = 0x4f00_0000_cf00_0000_7fc0_0000_3fc0_0000;
    let operand: [u8; 16] = [
        0x00, 0x00, 0xc0, 0x3f, 0x00, 0x00, 0xc0, 0x7f, 0x00, 0x00, 0x00, 0xcf, 0x00, 0x00, 0x00, 0x4f,
    ];
    for (index, &end) in instruction_ends.iter().enumerate() {
        let mut prefix = bytes[..end].to_vec();
        prefix.extend_from_slice(&[0x0f, 0x05]);
        let source = [super::BorrowedSource {
            guest_first: 0x402720,
            bytes: &prefix,
        }];
        let mut native = initial.clone();
        let mut storage = operand;
        let view = ProjectionView {
            guest_first: 0x7000,
            guest_last: 0x7010,
            host_first: storage.as_mut_ptr() as usize as u64,
            mapping_incarnation: 1,
            permissions: 3,
            write_policy: super::WRITE_EXACT,
            write_index: 0,
        };
        let projection = Projection {
            views: &raw const view,
            count: 1,
            mapping_incarnation: 1,
            active: 0,
        };
        let executor = Executor::create().expect("native executor");
        let mut resolve = |_: u64, _: &mut [u8]| None;
        let outcome = executor
            .run_x86(
                &mut native,
                &source,
                1,
                index as u64 + 1,
                64,
                false,
                Some(&projection),
                &mut resolve,
            )
            .unwrap()
            .0;
        assert_eq!(outcome.exit, Exit::Syscall, "boundary {} outcome {outcome:?}", index + 1);

        let mut memory = ReplayMemory {
            sources: vec![ReplaySource {
                first: 0x402720,
                bytes: prefix.clone(),
            }],
            data: vec![ReplayView {
                first: 0x7000,
                data: operand.to_vec(),
            }],
            generation: 1,
        };
        let machine = ExecutionMachine::new(ExecutionSnapshot {
            version: EXECUTION_SNAPSHOT_VERSION,
            cpu: ExecutionCpuSnapshot::X86_64(initial.clone()),
            cache_epoch: 1,
            fault: None,
        })
        .unwrap();
        let step = machine.run_slice(1, index as u64 + 3, &mut memory);
        assert!(matches!(step, StepOutcome::Syscall { .. }), "boundary {} step {step:?}", index + 1);
        machine.freeze().unwrap();
        let ExecutionCpuSnapshot::X86_64(interpreted) = machine.snapshot().unwrap().cpu else {
            unreachable!()
        };
        assert_eq!(
            native,
            interpreted,
            "first state divergence after instruction {} ending at {:#x}",
            index + 1,
            0x402720 + end
        );
    }
}

/// Scalar ordered/unordered compares. The single forms (`comiss`/`ucomiss`) are new;
/// the double forms guard the shared lowering. Covers below, equal, above and the
/// unordered NaN case, which is the only one that sets PF.
#[cfg(target_arch = "aarch64")]
#[test]
fn x86_sse_scalar_compare_flags_match_interpreter_at_each_boundary() {
    let bytes = [
        0x0f, 0x2f, 0xc1, // comiss %xmm1,%xmm0   1.5 vs 2.5 -> below
        0x0f, 0x2f, 0xc2, // comiss %xmm2,%xmm0   equal
        0x0f, 0x2e, 0xc3, // ucomiss %xmm3,%xmm0  unordered
        0x0f, 0x2f, 0xc8, // comiss %xmm0,%xmm1   above
        0x0f, 0x2e, 0xcb, // ucomiss %xmm3,%xmm1  unordered
        0x0f, 0x2f, 0x06, // comiss (%rsi),%xmm0
        0x0f, 0x2e, 0x06, // ucomiss (%rsi),%xmm0
        0x66, 0x0f, 0x2f, 0xe5, // comisd %xmm5,%xmm4
        0x66, 0x0f, 0x2e, 0xe6, // ucomisd %xmm6,%xmm4
        0x66, 0x0f, 0x2f, 0x26, // comisd (%rsi),%xmm4
    ];
    let instruction_ends = [3, 6, 9, 12, 15, 18, 21, 25, 29, 33];
    let mut initial = X86CpuState {
        rip: 0x402720,
        ..X86CpuState::default()
    };
    initial.registers[6] = 0x7000;
    initial.vectors[0] = 0x3fc0_0000; // 1.5f
    initial.vectors[1] = 0x4020_0000; // 2.5f
    initial.vectors[2] = 0x3fc0_0000; // 1.5f
    initial.vectors[3] = 0x7fc0_0000; // NaN f
    initial.vectors[4] = 0x3ff8_0000_0000_0000; // 1.5
    initial.vectors[5] = 0x4004_0000_0000_0000; // 2.5
    initial.vectors[6] = 0x7ff8_0000_0000_0000; // NaN
    // Low 8 bytes are 2.5 as a double, whose low 4 bytes are +0.0 as a float.
    let operand: [u8; 16] = [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    for (index, &end) in instruction_ends.iter().enumerate() {
        let mut prefix = bytes[..end].to_vec();
        prefix.extend_from_slice(&[0x0f, 0x05]);
        let source = [super::BorrowedSource {
            guest_first: 0x402720,
            bytes: &prefix,
        }];
        let mut native = initial.clone();
        let mut storage = operand;
        let view = ProjectionView {
            guest_first: 0x7000,
            guest_last: 0x7010,
            host_first: storage.as_mut_ptr() as usize as u64,
            mapping_incarnation: 1,
            permissions: 3,
            write_policy: super::WRITE_EXACT,
            write_index: 0,
        };
        let projection = Projection {
            views: &raw const view,
            count: 1,
            mapping_incarnation: 1,
            active: 0,
        };
        let executor = Executor::create().expect("native executor");
        let mut resolve = |_: u64, _: &mut [u8]| None;
        let outcome = executor
            .run_x86(
                &mut native,
                &source,
                1,
                index as u64 + 1,
                64,
                false,
                Some(&projection),
                &mut resolve,
            )
            .unwrap()
            .0;
        assert_eq!(outcome.exit, Exit::Syscall, "boundary {} outcome {outcome:?}", index + 1);

        let mut memory = ReplayMemory {
            sources: vec![ReplaySource {
                first: 0x402720,
                bytes: prefix.clone(),
            }],
            data: vec![ReplayView {
                first: 0x7000,
                data: operand.to_vec(),
            }],
            generation: 1,
        };
        let machine = ExecutionMachine::new(ExecutionSnapshot {
            version: EXECUTION_SNAPSHOT_VERSION,
            cpu: ExecutionCpuSnapshot::X86_64(initial.clone()),
            cache_epoch: 1,
            fault: None,
        })
        .unwrap();
        let step = machine.run_slice(1, index as u64 + 3, &mut memory);
        assert!(matches!(step, StepOutcome::Syscall { .. }), "boundary {} step {step:?}", index + 1);
        machine.freeze().unwrap();
        let ExecutionCpuSnapshot::X86_64(interpreted) = machine.snapshot().unwrap().cpu else {
            unreachable!()
        };
        assert_eq!(
            native,
            interpreted,
            "first state divergence after instruction {} ending at {:#x}",
            index + 1,
            0x402720 + end
        );
    }
}

/// `cvttss2si`/`cvttsd2si` to both a 32- and a 64-bit integer, exercising the
/// saturation edges where x86 yields the integer indefinite value: at and just
/// past 2^31 and 2^63, negative overflow, and NaN.
#[cfg(target_arch = "aarch64")]
#[test]
fn x86_sse_truncating_float_to_integer_matches_interpreter_at_each_boundary() {
    // xmm0 walks the interesting single-precision inputs, xmm1 the double ones.
    let cases: [(u128, u128); 7] = [
        (0x3fc0_0000, 0x3ff8_0000_0000_0000),                     // 1.5
        (0xbfc0_0000, 0xbff8_0000_0000_0000),                     // -1.5
        (0x4f00_0000, 0x41e0_0000_0000_0000),                     // 2^31
        (0xcf00_0000, 0xc1e0_0000_0000_0000),                     // -2^31
        (0x5f00_0000, 0x43e0_0000_0000_0000),                     // 2^63
        (0xdf00_0000, 0xc3e0_0000_0000_0000),                     // -2^63
        (0x7fc0_0000, 0x7ff8_0000_0000_0000),                     // NaN
    ];
    let bytes = [
        0xf3, 0x0f, 0x2c, 0xd8, // cvttss2si %xmm0,%ebx
        0xf3, 0x48, 0x0f, 0x2c, 0xd8, // cvttss2si %xmm0,%rbx
        0xf2, 0x0f, 0x2c, 0xd9, // cvttsd2si %xmm1,%ebx
        0xf2, 0x48, 0x0f, 0x2c, 0xd9, // cvttsd2si %xmm1,%rbx
        0xf3, 0x0f, 0x2c, 0x5e, 0x08, // cvttss2si 0x8(%rsi),%ebx
        0xf3, 0x48, 0x0f, 0x2c, 0x5e, 0x08, // cvttss2si 0x8(%rsi),%rbx
        0xf2, 0x0f, 0x2c, 0x1e, // cvttsd2si (%rsi),%ebx
        0xf2, 0x48, 0x0f, 0x2c, 0x1e, // cvttsd2si (%rsi),%rbx
    ];
    let instruction_ends = [4, 9, 13, 18, 23, 29, 33, 38];
    for (case, &(single, double)) in cases.iter().enumerate() {
        let mut initial = X86CpuState {
            rip: 0x402720,
            ..X86CpuState::default()
        };
        initial.registers[6] = 0x7000;
        initial.vectors[0] = single;
        initial.vectors[1] = double;
        // The double case sits at offset 0 and the single case at offset 8.
        let mut memory_operand = [0u8; 16];
        memory_operand[..8].copy_from_slice(&(double as u64).to_le_bytes());
        memory_operand[8..12].copy_from_slice(&(single as u32).to_le_bytes());
        for (index, &end) in instruction_ends.iter().enumerate() {
            let mut prefix = bytes[..end].to_vec();
            prefix.extend_from_slice(&[0x0f, 0x05]);
            let source = [super::BorrowedSource {
                guest_first: 0x402720,
                bytes: &prefix,
            }];
            let mut native = initial.clone();
            let mut storage = memory_operand;
            let view = ProjectionView {
                guest_first: 0x7000,
                guest_last: 0x7010,
                host_first: storage.as_mut_ptr() as usize as u64,
                mapping_incarnation: 1,
                permissions: 3,
                write_policy: super::WRITE_EXACT,
                write_index: 0,
            };
            let projection = Projection {
                views: &raw const view,
                count: 1,
                mapping_incarnation: 1,
                active: 0,
            };
            let executor = Executor::create().expect("native executor");
            let mut resolve = |_: u64, _: &mut [u8]| None;
            let outcome = executor
                .run_x86(
                    &mut native,
                    &source,
                    1,
                    index as u64 + 1,
                    64,
                    false,
                    Some(&projection),
                    &mut resolve,
                )
                .unwrap()
                .0;
            assert_eq!(outcome.exit, Exit::Syscall, "case {case} boundary {} outcome {outcome:?}", index + 1);

            let mut memory = ReplayMemory {
                sources: vec![ReplaySource {
                    first: 0x402720,
                    bytes: prefix.clone(),
                }],
                data: vec![ReplayView {
                    first: 0x7000,
                    data: memory_operand.to_vec(),
                }],
                generation: 1,
            };
            let machine = ExecutionMachine::new(ExecutionSnapshot {
                version: EXECUTION_SNAPSHOT_VERSION,
                cpu: ExecutionCpuSnapshot::X86_64(initial.clone()),
                cache_epoch: 1,
                fault: None,
            })
            .unwrap();
            let step = machine.run_slice(1, index as u64 + 3, &mut memory);
            assert!(matches!(step, StepOutcome::Syscall { .. }), "case {case} boundary {} step {step:?}", index + 1);
            machine.freeze().unwrap();
            let ExecutionCpuSnapshot::X86_64(interpreted) = machine.snapshot().unwrap().cpu else {
                unreachable!()
            };
            assert_eq!(
                native,
                interpreted,
                "case {case} first state divergence after instruction {} ending at {:#x}",
                index + 1,
                0x402720 + end
            );
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TraceResult {
    checkpoint: TraceCheckpoint,
    cpu: Aarch64CpuState,
    memory: Vec<Vec<u8>>,
    written: Vec<(u64, u64)>,
}

impl TraceResult {
    fn compare(&self, interpreter: &Self) -> Result<(), &'static str> {
        if self.checkpoint != interpreter.checkpoint {
            return Err("checkpoint identity differs");
        }
        if self.cpu != interpreter.cpu {
            return Err("architectural CPU state differs");
        }
        if self.memory != interpreter.memory || self.written != interpreter.written {
            return Err("guest memory writes differ");
        }
        Ok(())
    }
}

struct TraceCase {
    sources: Vec<TraceSource>,
    views: Vec<TraceView>,
    cpu: Aarch64CpuState,
}

struct TraceView {
    first: u64,
    memory: Vec<u8>,
    permissions: u32,
}
struct TraceSource {
    first: u64,
    bytes: Vec<u8>,
}

impl TraceCase {
    fn from_capture(capture: BoundaryCapture) -> Result<Self, &'static str> {
        if capture.sources.is_empty() {
            return Err("capture has no source");
        }
        let sources = capture
            .sources
            .into_iter()
            .map(|source| {
                if source.bytes.is_empty() || source.bytes.len() % 4 != 0 {
                    return Err("capture source is not words");
                }
                Ok(TraceSource {
                    first: source.guest_first,
                    bytes: source.bytes,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut views = capture
            .views
            .into_iter()
            .map(|view| {
                if view.guest_last.checked_sub(view.guest_first) != Some(view.bytes.len() as u64) {
                    return Err("capture view length differs");
                }
                Ok(TraceView {
                    first: view.guest_first,
                    memory: view.bytes,
                    permissions: view.permissions,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        views.sort_unstable_by_key(|view| view.first);
        Ok(Self {
            sources,
            views,
            cpu: capture.cpu,
        })
    }

    fn checkpoint(&self, instruction_count: u64) -> Result<TraceCheckpoint, &'static str> {
        let memory_size = self
            .views
            .iter()
            .try_fold(0usize, |size, view| size.checked_add(view.memory.len()));
        if instruction_count == 0 || memory_size.is_none_or(|size| size > MEMORY_LIMIT) {
            return Err("invalid differential bound");
        }
        Ok(TraceCheckpoint {
            instruction_count,
            cpu: self.cpu.clone(),
            memory: self.views.iter().map(|view| view.memory.clone()).collect(),
        })
    }

    fn native(&self, instruction_count: u64) -> Result<TraceResult, &'static str> {
        let checkpoint = self.checkpoint(instruction_count)?;
        let spans: Vec<_> = self
            .sources
            .iter()
            .map(|source| SourceSpan {
                guest_first: source.first,
                bytes: source.bytes.as_ptr(),
                size: source.bytes.len(),
                mapping_incarnation: 1,
                instruction_epoch: 1,
            })
            .collect();
        let source = Source {
            spans: spans.as_ptr(),
            span_count: spans.len(),
            mapping_incarnation: 1,
            instruction_epoch: 1,
        };
        let mut memory: Vec<_> = self.views.iter().map(|view| view.memory.clone()).collect();
        let views: Vec<_> = self
            .views
            .iter()
            .zip(memory.iter_mut())
            .map(|(input, bytes)| {
                let guest_last = input
                    .first
                    .checked_add(bytes.len() as u64)
                    .ok_or("memory span overflow")?;
                Ok(ProjectionView {
                    guest_first: input.first,
                    guest_last,
                    host_first: bytes.as_mut_ptr() as usize as u64,
                    mapping_incarnation: 1,
                    permissions: input.permissions,
                    write_policy: super::WRITE_EXACT,
                    write_index: 0,
                })
            })
            .collect::<Result<_, &'static str>>()?;
        let projection = Projection {
            views: views.as_ptr(),
            count: views.len(),
            mapping_incarnation: 1,
            active: 0,
        };
        let executor = Executor::create().map_err(|()| "native create failed")?;
        executor.reset(1).map_err(|()| "native reset failed")?;
        let mut cpu = self.cpu.clone();
        let outcome = executor
            .run_aarch64(&mut cpu, &source, Some(&projection), 1, instruction_count, None, None)
            .map_err(|()| "native run failed")?;
        if outcome.0 != Exit::Yield || outcome.4 != 0 || outcome.5 != instruction_count {
            return Err("native checkpoint was not an exact fully-spilled budget exit");
        }
        let written = changed_view_ranges(&self.views, &checkpoint.memory, &memory);
        Ok(TraceResult {
            checkpoint,
            cpu,
            memory,
            written,
        })
    }

    fn interpreter(&self, checkpoint: &TraceCheckpoint) -> Result<TraceResult, &'static str> {
        if checkpoint != &self.checkpoint(checkpoint.instruction_count)? {
            return Err("checkpoint does not belong to trace input");
        }
        let mut memory = ReplayMemory {
            sources: self
                .sources
                .iter()
                .map(|source| ReplaySource {
                    first: source.first,
                    bytes: source.bytes.clone(),
                })
                .collect(),
            data: self
                .views
                .iter()
                .zip(checkpoint.memory.iter())
                .map(|(view, bytes)| ReplayView {
                    first: view.first,
                    data: bytes.clone(),
                })
                .collect(),
            generation: 1,
        };
        let machine = ExecutionMachine::new(ExecutionSnapshot {
            version: EXECUTION_SNAPSHOT_VERSION,
            cpu: ExecutionCpuSnapshot::Aarch64(checkpoint.cpu.clone()),
            cache_epoch: 1,
            fault: None,
        })
        .map_err(|_| "interpreter create failed")?;
        if machine.run_slice(1, checkpoint.instruction_count, &mut memory) != StepOutcome::Yield {
            return Err("interpreter did not stop at requested boundary");
        }
        machine.freeze().map_err(|_| "interpreter freeze failed")?;
        let ExecutionCpuSnapshot::Aarch64(cpu) = machine.snapshot().map_err(|_| "interpreter snapshot failed")?.cpu
        else {
            return Err("interpreter returned the wrong architecture");
        };
        let after: Vec<_> = memory.data.iter().map(|view| view.data.clone()).collect();
        let written = changed_view_ranges(&self.views, &checkpoint.memory, &after);
        Ok(TraceResult {
            checkpoint: checkpoint.clone(),
            cpu,
            memory: after,
            written,
        })
    }
}

impl BoundaryCapture {
    fn encode(&self) -> Result<Vec<u8>, &'static str> {
        let snapshot = ExecutionSnapshot {
            version: EXECUTION_SNAPSHOT_VERSION,
            cpu: ExecutionCpuSnapshot::Aarch64(self.cpu.clone()),
            cache_epoch: 1,
            fault: None,
        };
        let cpu = snapshot.encode().map_err(|_| "capture cpu encode")?;
        let mut output = CaptureOutput::new();
        output.bytes(&cpu)?;
        output.count(self.sources.len())?;
        for source in &self.sources {
            output.u64(source.guest_first);
            output.u64(source.incarnation);
            output.u64(source.version);
            output.bytes(&source.bytes)?;
        }
        output.count(self.views.len())?;
        for view in &self.views {
            output.u64(view.guest_first);
            output.u64(view.guest_last);
            output.u32(view.permissions);
            output.bytes(&view.bytes)?;
        }
        Ok(output.finish())
    }

    fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        let mut input = CaptureInput { bytes, offset: 0 };
        if input.take(8)? != b"HLCAP01\0" {
            return Err("capture magic");
        }
        let mut aggregate = 0usize;
        let snapshot =
            ExecutionSnapshot::decode(input.bounded_bytes(&mut aggregate)?).map_err(|_| "capture cpu decode")?;
        let ExecutionCpuSnapshot::Aarch64(cpu) = snapshot.cpu else {
            return Err("capture cpu architecture");
        };
        let source_count = input.u32()?;
        if source_count > 8 {
            return Err("capture source count");
        }
        let mut sources = Vec::with_capacity(source_count);
        for _ in 0..source_count {
            sources.push(super::BoundarySource {
                guest_first: input.u64()?,
                incarnation: input.u64()?,
                version: input.u64()?,
                bytes: input.bounded_bytes(&mut aggregate)?.to_vec(),
            });
        }
        let view_count = input.u32()?;
        if view_count > 64 {
            return Err("capture view count");
        }
        let mut views = Vec::with_capacity(view_count);
        for _ in 0..view_count {
            views.push(super::BoundaryView {
                guest_first: input.u64()?,
                guest_last: input.u64()?,
                permissions: input.u32()? as u32,
                bytes: input.bounded_bytes(&mut aggregate)?.to_vec(),
            });
        }
        if input.offset != bytes.len() {
            return Err("capture trailing bytes");
        }
        Ok(Self { cpu, sources, views })
    }
}

struct CaptureOutput {
    bytes: Vec<u8>,
}

impl CaptureOutput {
    fn new() -> Self {
        Self {
            bytes: b"HLCAP01\0".to_vec(),
        }
    }

    fn count(&mut self, value: usize) -> Result<(), &'static str> {
        self.u32(u32::try_from(value).map_err(|_| "capture count overflow")?);
        Ok(())
    }

    fn bytes(&mut self, value: &[u8]) -> Result<(), &'static str> {
        self.u64(u64::try_from(value.len()).map_err(|_| "capture length overflow")?);
        self.bytes.extend_from_slice(value);
        Ok(())
    }

    fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

struct CaptureInput<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl<'a> CaptureInput<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8], &'static str> {
        let end = self.offset.checked_add(length).ok_or("capture offset overflow")?;
        let value = self.bytes.get(self.offset..end).ok_or("capture truncated")?;
        self.offset = end;
        Ok(value)
    }
    fn u32(&mut self) -> Result<usize, &'static str> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()) as usize)
    }
    fn u64(&mut self) -> Result<u64, &'static str> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn bounded_bytes(&mut self, aggregate: &mut usize) -> Result<&'a [u8], &'static str> {
        let length = usize::try_from(self.u64()?).map_err(|_| "capture length overflow")?;
        *aggregate = aggregate.checked_add(length).ok_or("capture aggregate overflow")?;
        if *aggregate > MEMORY_LIMIT {
            return Err("capture aggregate limit");
        }
        self.take(length)
    }
}

struct PrefixSearch {
    maximum: u64,
}

impl PrefixSearch {
    fn through(maximum: u64) -> Self {
        Self { maximum }
    }

    fn first(self, mut differs: impl FnMut(u64) -> Result<bool, &'static str>) -> Result<Option<u64>, &'static str> {
        if self.maximum == 0 || !differs(self.maximum)? {
            return Ok(None);
        }
        let (mut low, mut high) = (1, self.maximum);
        while low < high {
            let middle = low + (high - low) / 2;
            if differs(middle)? {
                high = middle;
            } else {
                low = middle + 1;
            }
        }
        Ok(Some(low))
    }
}

fn changed_ranges(first: u64, before: &[u8], after: &[u8]) -> Vec<(u64, u64)> {
    let mut ranges = Vec::new();
    let mut cursor = 0;
    while cursor < before.len() {
        if before[cursor] == after[cursor] {
            cursor += 1;
            continue;
        }
        let start = cursor;
        while cursor < before.len() && before[cursor] != after[cursor] {
            cursor += 1;
        }
        ranges.push((first + start as u64, first + cursor as u64));
    }
    ranges
}

fn changed_view_ranges(views: &[TraceView], before: &[Vec<u8>], after: &[Vec<u8>]) -> Vec<(u64, u64)> {
    views
        .iter()
        .zip(before)
        .zip(after)
        .flat_map(|((view, before), after)| changed_ranges(view.first, before, after))
        .collect()
}

struct ReplayMemory {
    sources: Vec<ReplaySource>,
    data: Vec<ReplayView>,
    generation: u64,
}
struct ReplaySource {
    first: u64,
    bytes: Vec<u8>,
}

struct ReplayView {
    first: u64,
    data: Vec<u8>,
}

impl ReplayMemory {
    fn data_range(&self, address: u64, bytes: u8) -> Result<(usize, std::ops::Range<usize>), ()> {
        self.data
            .iter()
            .enumerate()
            .find_map(|(index, view)| {
                let start = usize::try_from(address.checked_sub(view.first)?).ok()?;
                let end = start.checked_add(usize::from(bytes))?;
                view.data.get(start..end)?;
                Some((index, start..end))
            })
            .ok_or(())
    }
}

impl GuestOperandMemory for ReplayMemory {
    type Reservation = (u64, u8);
    type BatchReservation = Vec<(u64, u8)>;

    fn read(&self, address: u64, bytes: u8) -> Result<u64, ()> {
        for source in &self.sources {
            if let Some(offset) = address.checked_sub(source.first) {
                let start = usize::try_from(offset).map_err(|_| ())?;
                let end = start.checked_add(usize::from(bytes)).ok_or(())?;
                if let Some(value) = source.bytes.get(start..end) {
                    return Ok(value
                        .iter()
                        .enumerate()
                        .fold(0, |word, (index, byte)| word | (u64::from(*byte) << (index * 8))));
                }
            }
        }
        let (index, range) = self.data_range(address, bytes)?;
        Ok(self.data[index].data[range]
            .iter()
            .enumerate()
            .fold(0, |word, (index, byte)| word | (u64::from(*byte) << (index * 8))))
    }

    fn reserve_write(&self, address: u64, bytes: u8) -> Result<Self::Reservation, ()> {
        self.data_range(address, bytes)?;
        Ok((address, bytes))
    }

    fn commit_write(&mut self, reservation: Self::Reservation, value: u64) -> Result<(), ()> {
        let (index, range) = self.data_range(reservation.0, reservation.1)?;
        self.data[index].data[range].copy_from_slice(&value.to_le_bytes()[..usize::from(reservation.1)]);
        self.generation = self.generation.wrapping_add(1);
        Ok(())
    }

    fn reserve_write_batch(&self, writes: &[(u64, u8)]) -> Result<Self::BatchReservation, u64> {
        for &(address, bytes) in writes {
            self.reserve_write(address, bytes).map_err(|()| address)?;
        }
        Ok(writes.to_vec())
    }

    fn commit_write_batch(&mut self, reservation: Self::BatchReservation, values: &[u64]) -> Result<(), ()> {
        for (write, value) in reservation.into_iter().zip(values) {
            self.commit_write(write, *value)?;
        }
        Ok(())
    }
}

impl ExclusiveMemory for ReplayMemory {
    fn load_ordered(&mut self, address: u64, bytes: u8, _order: MemoryOrder) -> Result<u64, ()> {
        self.read(address, bytes)
    }

    fn store_ordered(&mut self, address: u64, bytes: u8, value: u64, _order: MemoryOrder) -> Result<(), ()> {
        let reservation = self.reserve_write(address, bytes)?;
        self.commit_write(reservation, value)
    }

    fn load_exclusive(
        &mut self,
        address: u64,
        bytes: u8,
        pair: bool,
        _order: MemoryOrder,
    ) -> Result<ExclusiveLoad, ()> {
        Ok(ExclusiveLoad {
            value: AtomicValue {
                low: self.read(address, bytes)?,
                high: if pair {
                    self.read(address + u64::from(bytes), bytes)?
                } else {
                    0
                },
            },
            reservation: ExclusiveReservation::new(address, bytes, pair, MappingGeneration::new(self.generation)),
        })
    }

    fn store_exclusive(
        &mut self,
        reservation: ExclusiveReservation,
        value: AtomicValue,
        _order: MemoryOrder,
    ) -> Result<bool, ()> {
        if reservation.generation().value() != self.generation {
            return Ok(false);
        }
        self.store_ordered(
            reservation.address(),
            reservation.element_bytes(),
            value.low,
            MemoryOrder::Relaxed,
        )?;
        if reservation.pair() {
            self.store_ordered(
                reservation.address() + u64::from(reservation.element_bytes()),
                reservation.element_bytes(),
                value.high,
                MemoryOrder::Relaxed,
            )?;
        }
        Ok(true)
    }

    fn compare_exchange(
        &mut self,
        address: u64,
        bytes: u8,
        pair: bool,
        expected: AtomicValue,
        replacement: AtomicValue,
        _order: MemoryOrder,
    ) -> Result<AtomicValue, ()> {
        let load = self.load_exclusive(address, bytes, pair, MemoryOrder::Relaxed)?;
        if load.value == expected {
            self.store_exclusive(load.reservation, replacement, MemoryOrder::Relaxed)?;
        }
        Ok(load.value)
    }

    fn fetch_update(
        &mut self,
        address: u64,
        bytes: u8,
        operation: AtomicOperation,
        operand: u64,
        _order: MemoryOrder,
    ) -> Result<u64, ()> {
        let old = self.read(address, bytes)?;
        let value = match operation {
            AtomicOperation::Swap => operand,
            AtomicOperation::Add => old.wrapping_add(operand),
            AtomicOperation::Clear => old & !operand,
            AtomicOperation::ExclusiveOr => old ^ operand,
            AtomicOperation::Set => old | operand,
            AtomicOperation::SignedMaximum => (old as i64).max(operand as i64) as u64,
            AtomicOperation::SignedMinimum => (old as i64).min(operand as i64) as u64,
            AtomicOperation::UnsignedMaximum => old.max(operand),
            AtomicOperation::UnsignedMinimum => old.min(operand),
        };
        self.store_ordered(address, bytes, value, MemoryOrder::Relaxed)?;
        Ok(old)
    }
}

impl ExecutionInstructionMemory for ReplayMemory {
    fn fetch(&self, address: u64, bytes: &mut [u8]) -> Result<usize, ()> {
        let source = self
            .sources
            .iter()
            .find_map(|source| {
                let offset = usize::try_from(address.checked_sub(source.first)?).ok()?;
                source.bytes.get(offset..)
            })
            .ok_or(())?;
        let length = source.len().min(bytes.len());
        bytes[..length].copy_from_slice(&source[..length]);
        Ok(length)
    }
}

fn representative() -> TraceCase {
    let mut cpu = Aarch64CpuState {
        pc: 0x4000,
        ..Aarch64CpuState::default()
    };
    cpu.registers[1] = 0x8010;
    cpu.vectors[3] = 0x8877_6655_4433_2211_00ff_eedd_ccbb_aa99;
    TraceCase {
        sources: vec![TraceSource {
            first: 0x4000,
            bytes: [
                0xd282_4680_u32, // movz x0,#0x1234
                0xf900_0020,     // str x0,[x1]
                0x9100_0402,     // add x2,x0,#1
                0xd400_0001,     // svc #0; remains beyond the checkpoint
            ]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect(),
        }],
        views: vec![TraceView {
            first: 0x8000,
            memory: vec![0xa5; 64],
            permissions: u32::from(Protection::READ.union(Protection::WRITE).bits()),
        }],
        cpu,
    }
}

#[test]
fn store_replay() {
    let case = representative();
    let native = case.native(3).expect("native checkpoint");
    assert_eq!(native.checkpoint.instruction_count, 3);
    assert_eq!(native.written, [(0x8010, 0x8018)]);
    let interpreter = case.interpreter(&native.checkpoint).expect("interpreter replay");
    native.compare(&interpreter).expect("native/interpreter equivalence");
}

#[test]
fn divergence_detected() {
    let case = representative();
    let native = case.native(3).expect("native checkpoint");
    let mut divergent = case.interpreter(&native.checkpoint).expect("interpreter replay");
    divergent.cpu.registers[2] ^= 1;
    assert_eq!(native.compare(&divergent), Err("architectural CPU state differs"));
    divergent.cpu = native.cpu.clone();
    divergent.memory[0][16] ^= 1;
    assert_eq!(native.compare(&divergent), Err("guest memory writes differ"));
}

#[test]
fn captured_multi_view_replay_bisects() {
    let mut cpu = Aarch64CpuState {
        pc: 0x5000,
        ..Aarch64CpuState::default()
    };
    cpu.registers[0] = 0x1122_3344_5566_7788;
    cpu.registers[1] = 0x8010;
    cpu.registers[3] = 0x9018;
    let words = [0xf900_0020_u32, 0xf940_0062, 0x9100_0442, 0xd400_0001];
    let capture = BoundaryCapture {
        cpu,
        sources: vec![super::BoundarySource {
            guest_first: 0x5000,
            incarnation: 1,
            version: 1,
            bytes: words.into_iter().flat_map(u32::to_le_bytes).collect(),
        }],
        views: vec![
            super::BoundaryView {
                guest_first: 0x8000,
                guest_last: 0x8040,
                permissions: u32::from(Protection::READ.union(Protection::WRITE).bits()),
                bytes: vec![0xa5; 64],
            },
            super::BoundaryView {
                guest_first: 0x9000,
                guest_last: 0x9040,
                permissions: u32::from(Protection::READ.union(Protection::WRITE).bits()),
                bytes: vec![0x5a; 64],
            },
        ],
    };
    let encoded = capture.encode().expect("capture encoding");
    let capture = BoundaryCapture::decode(&encoded).expect("capture decoding");
    let case = TraceCase::from_capture(capture).expect("captured trace");
    for count in 1..4 {
        let native = case.native(count).expect("native prefix");
        let interpreter = case.interpreter(&native.checkpoint).expect("interpreter prefix");
        native.compare(&interpreter).expect("prefix equivalence");
    }
    let first = PrefixSearch::through(3)
        .first(|count| {
            let native = case.native(count)?;
            let mut interpreter = case.interpreter(&native.checkpoint)?;
            if count >= 2 {
                interpreter.cpu.registers[2] ^= 1;
            }
            Ok(native.compare(&interpreter).is_err())
        })
        .expect("bounded bisection");
    assert_eq!(first, Some(2));
}

#[test]
#[ignore = "private live native capture; requires HL_TEST_CAPTURE_* inputs"]
fn live_binary_capture() {
    let isa = std::env::var("HL_TEST_CAPTURE_ISA").expect("HL_TEST_CAPTURE_ISA");
    assert_eq!(isa, "aarch64", "live differential capture currently owns AArch64 state");
    let binary = PathBuf::from(std::env::var_os("HL_TEST_CAPTURE_BINARY").expect("HL_TEST_CAPTURE_BINARY"));
    let ordinal = std::env::var("HL_TEST_CAPTURE_RUN")
        .expect("HL_TEST_CAPTURE_RUN")
        .parse::<usize>()
        .expect("capture run ordinal");
    let maximum = std::env::var("HL_TEST_CAPTURE_CAP")
        .expect("HL_TEST_CAPTURE_CAP")
        .parse::<usize>()
        .expect("capture byte cap");
    assert!(maximum <= MEMORY_LIMIT, "offline replay memory limit");
    let output = PathBuf::from(std::env::var_os("HL_TEST_CAPTURE_OUTPUT").expect("HL_TEST_CAPTURE_OUTPUT"));
    assert!(
        output.starts_with(std::env::temp_dir()),
        "capture output must stay in the temporary directory"
    );
    super::arm_live_capture(ordinal, maximum).expect("arm live capture");
    let _signals = crate::native::TerminationSignals::install().expect("termination signals");
    let engine = crate::runtime::Builder::new(crate::activation::GuestIsa::Aarch64, binary)
        .with_option("HL_NATIVE_EXECUTION", "1")
        .build()
        .expect("build in-process engine");
    engine.start().expect("start in-process engine");
    let _ = engine.wait().expect("wait in-process engine");
    engine.destroy().expect("destroy in-process engine");
    let capture = super::take_live_capture()
        .expect("selected native run was not reached")
        .expect("selected native run exceeded capture bounds");
    let bytes = capture.encode().expect("serialize capture");
    std::fs::write(&output, bytes).expect("write capture");
}

#[test]
#[ignore = "private offline replay; requires HL_TEST_CAPTURE_INPUT"]
fn replay_binary_capture() {
    let input = PathBuf::from(std::env::var_os("HL_TEST_CAPTURE_INPUT").expect("HL_TEST_CAPTURE_INPUT"));
    assert!(
        input.starts_with(std::env::temp_dir()),
        "capture input must stay in the temporary directory"
    );
    let capture = BoundaryCapture::decode(&std::fs::read(input).expect("read capture")).expect("decode capture");
    let case = TraceCase::from_capture(capture).expect("captured trace case");
    let maximum = case.sources.iter().map(|source| source.bytes.len() / 4).sum::<usize>() as u64;
    let first = PrefixSearch::through(maximum)
        .first(|count| {
            let native = case.native(count)?;
            let interpreter = case.interpreter(&native.checkpoint)?;
            Ok(native.compare(&interpreter).is_err())
        })
        .expect("differential bisection");
    assert_eq!(first, None, "first divergent instruction count: {first:?}");
}

#[test]
fn boundary_capture_is_ordinal_and_bounded() {
    let executor = Executor::create().unwrap();
    executor.arm_boundary_capture(2, 32).unwrap();
    let cpu = Aarch64CpuState::default();
    let source_bytes = [0u8; 4];
    let source = [super::BorrowedSource {
        guest_first: 0x4000,
        bytes: &source_bytes,
    }];
    let mut data = [0x5a_u8; 8];
    let view = [ProjectionView {
        guest_first: 0x8000,
        guest_last: 0x8008,
        host_first: data.as_mut_ptr() as usize as u64,
        mapping_incarnation: 1,
        permissions: u32::from(Protection::READ.bits()),
        write_policy: super::WRITE_EXACT,
        write_index: 0,
    }];
    let token = hl_memory::ExecutableToken {
        incarnation: 1,
        version: 1,
    };
    executor.capture_boundary(&cpu, &source, &view, token);
    assert!(executor.take_boundary_capture().is_none());
    executor.capture_boundary(&cpu, &source, &view, token);
    let mut slow_data = [0x33_u8; 8];
    let mut slow = ProjectionView {
        guest_first: 0x9000,
        guest_last: 0x9008,
        host_first: slow_data.as_mut_ptr() as usize as u64,
        mapping_incarnation: 1,
        permissions: u32::from(Protection::READ.bits()),
        write_policy: super::WRITE_EXACT,
        write_index: 0,
    };
    {
        let mut capture = executor.boundary_capture.lock().unwrap();
        capture.append_view(&slow);
        slow_data.fill(0x44);
        slow.permissions = u32::from(Protection::WRITE.bits());
        capture.append_view(&slow);
    }
    let captured = executor.take_boundary_capture().unwrap().unwrap();
    assert_eq!(captured.sources[0].bytes, source_bytes);
    assert_eq!(captured.views[0].bytes, data);
    assert_eq!(captured.views.len(), 2);
    assert_eq!(captured.views[1].bytes, [0x33; 8]);
    assert_eq!(
        captured.views[1].permissions,
        u32::from(Protection::READ.union(Protection::WRITE).bits())
    );

    executor.arm_boundary_capture(1, 11).unwrap();
    executor.capture_boundary(&cpu, &source, &view, token);
    assert_eq!(
        executor.take_boundary_capture(),
        Some(Err("boundary capture size limit"))
    );
}

#[test]
fn capture_decoder_rejects_resource_counts_before_allocation() {
    let empty = BoundaryCapture {
        cpu: Aarch64CpuState::default(),
        sources: Vec::new(),
        views: Vec::new(),
    }
    .encode()
    .unwrap();
    let cpu_length = usize::try_from(u64::from_le_bytes(empty[8..16].try_into().unwrap())).unwrap();
    let count = 16 + cpu_length;
    let mut sources = empty.clone();
    sources[count..count + 4].copy_from_slice(&9_u32.to_le_bytes());
    assert_eq!(BoundaryCapture::decode(&sources), Err("capture source count"));

    let mut views = empty.clone();
    views[count + 4..count + 8].copy_from_slice(&65_u32.to_le_bytes());
    assert_eq!(BoundaryCapture::decode(&views), Err("capture view count"));

    let mut aggregate = empty[..count].to_vec();
    aggregate.extend_from_slice(&1_u32.to_le_bytes());
    aggregate.extend_from_slice(&[0; 24]);
    aggregate.extend_from_slice(&((MEMORY_LIMIT as u64) + 1).to_le_bytes());
    assert_eq!(BoundaryCapture::decode(&aggregate), Err("capture aggregate limit"));
}
