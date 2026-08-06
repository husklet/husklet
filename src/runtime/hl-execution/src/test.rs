use hl_isa::{GuestArchitecture, HostArchitecture};

use crate::*;

#[test]
fn digest_and_identity() {
    assert_eq!(ArtifactDigest::bytes(DIGEST_SEED, b"abcdefgh"), 0xb67b_b717_c540_5306);
    assert_eq!(CacheIdentity::name(None), 0x1357);
    assert_eq!(
        CacheIdentity::name(Some("/tmp/program")),
        CacheIdentity::name(Some("program"))
    );
    assert_eq!(
        CacheIdentity::configuration(7, GuestArchitecture::Aarch64, HostArchitecture::X86_64, 3),
        CacheIdentity::configuration(7, GuestArchitecture::Aarch64, HostArchitecture::X86_64, 3)
    );
}

#[test]
fn envelope_round_trips() {
    for (architecture, abi) in [
        (GuestArchitecture::Aarch64, AARCH64_CACHE_ABI),
        (GuestArchitecture::X86_64, X86_64_CACHE_ABI),
    ] {
        let envelope = CacheEnvelope {
            architecture,
            translator_abi: abi,
            identity: 42,
            payload: vec![1, 2, 3],
        };
        let bytes = envelope.encode().unwrap();
        assert_eq!(CacheEnvelope::decode(&bytes, bytes.len()).unwrap(), envelope);
        for end in 0..bytes.len() {
            assert!(CacheEnvelope::decode(&bytes[..end], bytes.len()).is_err());
        }
        let mut corrupt = bytes;
        corrupt[10] ^= 1;
        assert_eq!(
            CacheEnvelope::decode(&corrupt, corrupt.len()),
            Err(PersistenceError::Checksum)
        );
    }
}

#[test]
fn cache_abi_compatibility() {
    let format = 10;
    let current = CacheCompatibility {
        format,
        translator_abi: AARCH64_CACHE_ABI,
    };
    assert!(current.is_compatible(current));
    for stored in [
        CacheCompatibility {
            format,
            translator_abi: 0x4136_3450_4341_3031,
        },
        CacheCompatibility {
            format: format - 1,
            translator_abi: AARCH64_CACHE_ABI,
        },
        CacheCompatibility {
            format,
            translator_abi: AARCH64_CACHE_ABI - 1,
        },
        CacheCompatibility {
            format,
            translator_abi: X86_64_CACHE_ABI,
        },
    ] {
        assert!(!stored.is_compatible(current));
    }
}

#[test]
fn x86_prefix_families() {
    let legacy = X86Decoder::decode(&[0xf0, 0xf3, 0x64, 0x67, 0x66, 0x48, 0x90]).unwrap();
    assert_eq!(legacy.length, 7);
    assert!(legacy.prefixes.lock && legacy.prefixes.rep && legacy.prefixes.address_32);
    assert_eq!(legacy.prefixes.segment, Some(Segment::Fs));
    assert!(matches!(
        legacy.encoding,
        Encoding::Legacy {
            rex: Some(Rex { w: true, .. }),
            map: 0
        }
    ));

    let vex = X86Decoder::decode(&[0xc5, 0xf8, 0x77]).unwrap();
    assert!(matches!(vex.encoding, Encoding::Vex { map: 1, .. }));
    let evex = X86Decoder::decode(&[0x62, 0x01, 0xed, 0xd3, 0x70, 0xcc, 0x80]).unwrap();
    assert!(matches!(evex.encoding, Encoding::Evex { length: 2, mask: 3, .. }));
}

#[test]
fn x86_prefix_decode() {
    for bytes in [&[][..], &[0x48][..], &[0xc5][..], &[0xc4, 0][..], &[0x62, 0, 0][..]] {
        assert_eq!(X86Decoder::decode(bytes), Err(DecodeError::Truncated));
    }
    assert_eq!(X86Decoder::decode(&[0x66; 16]), Err(DecodeError::TooLong));
    assert_eq!(X86Decoder::decode(&[0x66; 15]), Err(DecodeError::TooLong));
}

#[test]
fn x86_modrm_sib() {
    let decoded = X86Decoder::decode(&[0x4f, 0x8b, 0x84, 0xcc, 0x78, 0x56, 0x34, 0x12]).unwrap();
    assert_eq!(decoded.length, 8);
    assert_eq!(decoded.register, Some(8));
    assert_eq!(
        decoded.address,
        Some(EffectiveAddress {
            base: Some(12),
            index: Some(9),
            scale: 3,
            displacement: 0x1234_5678,
            ..EffectiveAddress::default()
        })
    );
    let rip = X86Decoder::decode(&[0x48, 0x8b, 0x05, 0xfc, 0xff, 0xff, 0xff]).unwrap();
    assert_eq!(rip.address.unwrap().displacement, -4);
    assert!(rip.address.unwrap().rip_relative);
    let no_base = X86Decoder::decode(&[0x8b, 0x04, 0x25, 0x78, 0x56, 0x34, 0x12]).unwrap();
    assert_eq!(no_base.address.unwrap().base, None);
    assert_eq!(no_base.address.unwrap().index, None);
    let movabs = X86Decoder::decode(&[0x48, 0xb8, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11]).unwrap();
    assert_eq!(movabs.immediate, Some((0x1122_3344_5566_7788, 8)));
    let test_group = X86Decoder::decode(&[0x47, 0xf6, 0xc0, 0x80]).unwrap();
    assert_eq!(test_group.immediate, Some((-128, 1)));
    let plan = EffectiveAddress {
        base: Some(1),
        index: Some(2),
        scale: 2,
        displacement: -4,
        address_32: true,
        segment: Some(Segment::Fs),
        ..EffectiveAddress::default()
    };
    let mut registers = [0_u64; 16];
    registers[1] = 0xffff_ffff_0000_0010;
    registers[2] = 3;
    assert_eq!(plan.resolve(&registers, 0, 0x1000, 0), 0x1018);
}

struct BoundaryFetch {
    bytes: Vec<u8>,
    allow_full: bool,
}

impl InstructionFetch for BoundaryFetch {
    fn fetch(&self, _: u64, destination: &mut [u8]) -> Result<(), FetchError> {
        if destination.len() == 15 && !self.allow_full {
            return Err(FetchError);
        }
        destination.copy_from_slice(&self.bytes[..destination.len()]);
        Ok(())
    }
}

#[test]
fn x86_fetch_touches() {
    let mut bytes = vec![0x90; 15];
    let short = BoundaryFetch {
        bytes: bytes.clone(),
        allow_full: false,
    };
    assert_eq!(X86Decoder::decode_at(&short, 4095).unwrap().length, 1);
    bytes[0] = 0x48;
    bytes[1] = 0xb8;
    let denied = BoundaryFetch {
        bytes: bytes.clone(),
        allow_full: false,
    };
    assert_eq!(X86Decoder::decode_at(&denied, 4095), Err(DecodeError::Fetch));
    let allowed = BoundaryFetch {
        bytes,
        allow_full: true,
    };
    assert_eq!(X86Decoder::decode_at(&allowed, 4095).unwrap().length, 10);
}

#[test]
fn x86_arithmetic_flags() {
    for left in 0_u64..=255 {
        for right in 0_u64..=255 {
            let add = Arithmetic::add(IntegerWidth::Byte, left, right, false);
            assert_eq!(add.result, (left + right) & 255);
            assert_eq!(add.flags.values() & 1, u16::from(left + right > 255));
            let sub = Arithmetic::sub(IntegerWidth::Byte, left, right, false);
            assert_eq!(sub.result, left.wrapping_sub(right) & 255);
            assert_eq!(sub.flags.values() & 1 != 0, left < right);
        }
    }
}

#[test]
fn x86_retained_nzcv() {
    assert_eq!(
        Arithmetic::retained_sub_nzcv(IntegerWidth::Byte, 5, 5),
        (1 << 30) | (1 << 29)
    );
    assert_eq!(Arithmetic::retained_sub_nzcv(IntegerWidth::Byte, 0, 1), 1 << 31);
    assert_eq!(
        Arithmetic::retained_sub_nzcv(IntegerWidth::Byte, 0x80, 1),
        (1 << 29) | (1 << 28)
    );

    let unchanged = Arithmetic::shift_left(IntegerWidth::Qword, 7, 64);
    assert_eq!(unchanged.result, 7);
    assert!(unchanged.flags.preserved(Flag::Carry));
    let shl = Arithmetic::shift_left(IntegerWidth::Byte, 0x81, 1);
    assert_eq!(shl.result, 2);
    assert!(shl.flags.values() & 1 != 0);
    assert!(shl.flags.values() & (1 << 11) != 0);
    let shr = Arithmetic::shift_right(IntegerWidth::Byte, 0x81, 1);
    assert_eq!(shr.result, 0x40);
    assert!(shr.flags.values() & (1 << 11) != 0);
    let sar = Arithmetic::shift_arithmetic_right(IntegerWidth::Byte, 0x80, 7);
    assert_eq!(sar.result, 0xff);

    let rol = Arithmetic::rotate_left(IntegerWidth::Byte, 0x81, 1);
    assert_eq!(rol.result, 3);
    let rcr = Arithmetic::rotate_carry_right(IntegerWidth::Byte, 0, 1, true);
    assert_eq!(rcr.result, 0x80);
    assert_eq!(rcr.flags.values() & 1, 0);
    let undefined = Arithmetic::rotate_left(IntegerWidth::Byte, 1, 2);
    assert!(undefined.flags.undefined() & (1 << 11) != 0);
    assert!(undefined.flags.preserved(Flag::Zero));
    let retained_rcl = Arithmetic::rotate_carry_left(IntegerWidth::Byte, 0x80, 1, false);
    assert_eq!(retained_rcl.result, 0);
    assert_eq!(retained_rcl.flags.values() & ((1 << 11) | 1), (1 << 11) | 1);
    let effective_zero = Arithmetic::rotate_carry_left(IntegerWidth::Byte, 0xa5, 9, true);
    assert_eq!(effective_zero.result, 0xa5);
    assert!(effective_zero.flags.preserved(Flag::Carry));
}

#[test]
fn x86_wide_arithmetic() {
    let cases = [
        (IntegerWidth::Word, 0, 0),
        (IntegerWidth::Word, 0x8000, 1),
        (IntegerWidth::Word, u64::MAX, 16),
        (IntegerWidth::Word, 0x7fff, 255),
        (IntegerWidth::Dword, 1, 2),
        (IntegerWidth::Dword, u32::MAX as u64, 31),
        (IntegerWidth::Dword, 0x8000_0000, 32),
        (IntegerWidth::Dword, u64::MAX, 64),
        (IntegerWidth::Qword, 1, 63),
        (IntegerWidth::Qword, u64::MAX, 64),
        (IntegerWidth::Qword, 0x8000_0000_0000_0000, 1),
        (IntegerWidth::Qword, 0x7fff, 255),
    ];
    for (width, value, count) in cases {
        let result = Arithmetic::shift_left(width, value, count);
        assert_eq!(result.result & !width.mask(), 0);
        let rotated = Arithmetic::rotate_right(width, value, count);
        assert_eq!(rotated.result & !width.mask(), 0);
    }
}

#[test]
fn x86_remaining_integer() {
    assert_eq!(
        Division::unsigned(IntegerWidth::Byte, 0, 0x0102, 2).unwrap(),
        Division {
            quotient: 0x81,
            remainder: 0
        }
    );
    assert_eq!(
        Division::unsigned(IntegerWidth::Byte, 0, 0x01ff, 1),
        Err(DivisionError::QuotientOverflow)
    );
    assert_eq!(
        Division::unsigned(IntegerWidth::Qword, 1, 0, 1),
        Err(DivisionError::QuotientOverflow)
    );
    assert_eq!(
        Division::signed(IntegerWidth::Qword, 0x8000_0000_0000_0000, 0, u64::MAX),
        Err(DivisionError::QuotientOverflow)
    );
    assert_eq!(Division::signed(IntegerWidth::Word, 0, 1, 0), Err(DivisionError::Zero));

    let shld = Arithmetic::shift_left_double(IntegerWidth::Word, 0x8001, 0xf0f0, 4);
    assert_eq!(shld.result, 0x001f);
    assert!(
        Arithmetic::shift_right_double(IntegerWidth::Dword, 7, 9, 32)
            .flags
            .preserved(Flag::Carry)
    );
    assert!(
        Arithmetic::shift_left_double(IntegerWidth::Qword, 7, 9, 64)
            .flags
            .preserved(Flag::Carry)
    );

    let negative = BitPlan::memory(-1, 0x80, BitAction::Reset);
    assert_eq!(
        (negative.byte_delta, negative.bit, negative.prior, negative.proposed),
        (-1, 7, true, 0)
    );
    assert_eq!(BitScan::forward(IntegerWidth::Dword, 0).result, None);
    assert_eq!(BitScan::trailing_zero_count(IntegerWidth::Word, 0).result, Some(16));
    assert_eq!(BitScan::leading_zero_count(IntegerWidth::Word, 1).result, Some(15));

    let product = Multiplication::widening(IntegerWidth::Byte, 0xff, 2, false);
    assert_eq!((product.low, product.high), (0xfe, 1));
    assert!(product.flags.values() & 1 != 0);
}

#[test]
fn x86_cpuid_retains() {
    let host = HostCapabilities {
        integer: true,
        floating_point: true,
        timestamp: true,
        compare_exchange: true,
        conditional_move: true,
        mmx: true,
        fxsave: true,
        sse: true,
        sse2: true,
        population_count: true,
        level_two: true,
        crypto: true,
        rep: true,
        bmi1: true,
        bmi2: true,
    };
    let policy = GuestFeaturePolicy::new(host).unwrap();
    assert_eq!(
        policy.cpuid(0, 0),
        CpuidRegisters {
            eax: 7,
            ebx: 0x756e6547,
            ecx: 0x6c65746e,
            edx: 0x49656e69
        }
    );
    assert_eq!(
        policy.cpuid(1, 0),
        CpuidRegisters {
            eax: 0x000206c2,
            ebx: 0,
            ecx: 0x02982203,
            edx: 0x0788a911
        }
    );
    assert_eq!(policy.cpuid(7, 1), CpuidRegisters::default());
    assert_eq!(policy.cpuid(0xb, 0), CpuidRegisters::default());
    assert_eq!(policy.cpuid(0x12345678, 0), CpuidRegisters::default());
    let mut brand = Vec::new();
    for leaf in 0x80000002..=0x80000004 {
        let registers = policy.cpuid(leaf, 0);
        for value in [registers.eax, registers.ebx, registers.ecx, registers.edx] {
            brand.extend_from_slice(&value.to_le_bytes());
        }
    }
    assert_eq!(&brand[..23], b"hl JIT x86-64 processor");
    assert!(brand[23..].iter().all(|byte| *byte == 0));
    assert_eq!(policy.xgetbv(0), Err(XgetbvError::UndefinedInstruction));
    assert_eq!(policy.xgetbv(1), Err(XgetbvError::UndefinedInstruction));
}

#[test]
fn x86_cpuid_capabilities() {
    assert_eq!(GuestFeaturePolicy::new(HostCapabilities::default()), None);
    let host = HostCapabilities {
        integer: true,
        floating_point: true,
        timestamp: true,
        compare_exchange: true,
        conditional_move: true,
        mmx: true,
        fxsave: true,
        sse: true,
        sse2: true,
        population_count: true,
        level_two: true,
        crypto: true,
        rep: true,
        bmi1: true,
        bmi2: true,
    };
    let first = GuestFeaturePolicy::new(host).unwrap();
    let second = GuestFeaturePolicy::new(host).unwrap();
    let left = std::thread::spawn(move || first.cpuid(7, 0));
    let right = std::thread::spawn(move || second.cpuid(7, 0));
    assert_eq!(left.join().unwrap(), right.join().unwrap());

    let scalar = GuestFeaturePolicy::interpreter();
    let leaf_one = scalar.cpuid(1, 0);
    assert_eq!(leaf_one.edx, 0x0788_a911);
    let baseline = (1 << 0) | (1 << 8) | (1 << 15) | (1 << 23) | (1 << 24) | (1 << 25) | (1 << 26);
    assert_eq!(leaf_one.edx & baseline, baseline);
    assert_eq!(leaf_one.ecx, 0x0298_2203);
    for incomplete in [3, 12, 22, 26, 27, 28] {
        assert_eq!(leaf_one.ecx & (1 << incomplete), 0);
    }
    let leaf_seven = scalar.cpuid(7, 0);
    assert_eq!(leaf_seven.ebx, (1 << 3) | (1 << 8) | (1 << 9) | (1 << 29));
    assert_eq!(leaf_seven.edx, 1 << 4);
    assert_eq!(scalar.cpuid(0xb, 0), CpuidRegisters::default());
    assert_eq!(scalar.xgetbv(0), Err(XgetbvError::UndefinedInstruction));

    let legacy = GuestFeaturePolicy::new(HostCapabilities {
        integer: true,
        floating_point: false,
        timestamp: false,
        compare_exchange: false,
        conditional_move: false,
        mmx: false,
        fxsave: false,
        sse: false,
        sse2: false,
        population_count: false,
        level_two: false,
        crypto: true,
        rep: false,
        bmi2: false,
        bmi1: false,
    })
    .unwrap();
    assert_eq!(legacy.cpuid(7, 0).ebx & (1 << 3), 0);
    assert_ne!(legacy.cpuid(7, 0).ebx & (1 << 8), 0);
}

#[test]
fn x86_decoder_preserves() {
    for raw in 0_u8..8 {
        let legacy = X86Decoder::decode(&[0x88, 0xc0 | (raw << 3) | raw]).unwrap();
        assert_eq!(
            (legacy.raw_mod, legacy.raw_reg, legacy.raw_rm),
            (Some(3), Some(raw), Some(raw))
        );
        let expected = if raw < 4 {
            ByteRegister::Low(raw)
        } else {
            ByteRegister::High(raw - 4)
        };
        assert_eq!(legacy.byte_register(raw, false), Some(expected));
        let rex = X86Decoder::decode(&[0x40, 0x88, 0xc0 | (raw << 3) | raw]).unwrap();
        assert_eq!(rex.byte_register(raw, false), Some(ByteRegister::Low(raw)));
        assert_eq!(rex.byte_register(raw, true), Some(ByteRegister::Low(raw + 8)));
    }
    let memory = X86Decoder::decode(&[0x88, 0x20]).unwrap();
    assert_eq!(
        (memory.raw_mod, memory.raw_reg, memory.raw_rm),
        (Some(0), Some(4), Some(0))
    );
    assert!(memory.address.is_some() && memory.register_operand.is_none());
    let vex = X86Decoder::decode(&[0xc5, 0xf9, 0x70, 0xc1, 1]).unwrap();
    assert_eq!(vex.rex(), None);
}

#[test]
fn x86_scalar_ir() {
    let high = X86ScalarDecoder::decode(&[0x88, 0xe0], 0).unwrap();
    assert_eq!(high.width, ScalarWidth::Byte);
    assert_eq!(
        high.instruction,
        ScalarInstruction::Move {
            destination: ScalarOperand::Register(ScalarRegister::Byte(ByteRegister::Low(0))),
            source: ScalarOperand::Register(ScalarRegister::Byte(ByteRegister::High(0))),
        }
    );
    let rex = X86ScalarDecoder::decode(&[0x40, 0x88, 0xe0], 0).unwrap();
    assert!(matches!(
        rex.instruction,
        ScalarInstruction::Move {
            source: ScalarOperand::Register(ScalarRegister::Byte(ByteRegister::Low(4))),
            ..
        }
    ));
    let memory = X86ScalarDecoder::decode(&[0x48, 0x8b, 0x43, 0xf8], 0).unwrap();
    assert_eq!(memory.width, ScalarWidth::Qword);
    assert!(matches!(
        memory.instruction,
        ScalarInstruction::Move {
            source: ScalarOperand::Memory(EffectiveAddress {
                base: Some(3),
                displacement: -8,
                ..
            }),
            ..
        }
    ));
    let lea = X86ScalarDecoder::decode(&[0x48, 0x8d, 0x4c, 0x90, 0x10], 0).unwrap();
    assert!(matches!(
        lea.instruction,
        ScalarInstruction::Lea {
            destination: ScalarRegister::General(1),
            ..
        }
    ));
}

#[test]
fn x86_scalar_bytes() {
    let add = X86ScalarDecoder::decode(&[0x48, 0x83, 0xc0, 0xff], 0).unwrap();
    assert_eq!(
        add.instruction,
        ScalarInstruction::Alu {
            operation: AluOperation::Add,
            destination: ScalarOperand::Register(ScalarRegister::General(0)),
            source: ScalarOperand::Immediate(-1),
            locked: false,
        }
    );
    assert!(matches!(
        X86ScalarDecoder::decode(&[0x50], 0).unwrap().instruction,
        ScalarInstruction::Push { .. }
    ));
    assert!(matches!(
        X86ScalarDecoder::decode(&[0x5f], 0).unwrap().instruction,
        ScalarInstruction::Pop { .. }
    ));
    assert_eq!(
        X86ScalarDecoder::decode(&[0xeb, 0xfe], 0x100).unwrap().instruction,
        ScalarInstruction::Jump { target: 0x100 }
    );
    assert_eq!(
        X86ScalarDecoder::decode(&[0x75, 5], u64::MAX - 1).unwrap().instruction,
        ScalarInstruction::JumpConditional {
            condition: BranchCondition(5),
            target: 5
        }
    );
    assert!(matches!(
        X86ScalarDecoder::decode(&[0xe8, 1, 0, 0, 0], 4).unwrap().instruction,
        ScalarInstruction::Call { target: 10 }
    ));
    assert_eq!(
        X86ScalarDecoder::decode(&[0xc2, 8, 0], 0).unwrap().instruction,
        ScalarInstruction::Return { pop_bytes: 8 }
    );
    assert!(matches!(
        X86ScalarDecoder::decode(&[0x0f, 0x05], 0).unwrap().instruction,
        ScalarInstruction::Syscall
    ));
    assert!(matches!(
        X86ScalarDecoder::decode(&[0x0f, 0x0b], 0).unwrap().instruction,
        ScalarInstruction::Undefined
    ));
}

#[test]
fn x86_scalar_unsupported() {
    assert_eq!(X86ScalarDecoder::decode(&[0xf0, 0x90], 0), Err(ScalarIrError::Invalid));
    assert_eq!(
        X86ScalarDecoder::decode(&[0xf3, 0x01, 0xc0], 0),
        Err(ScalarIrError::Invalid)
    );
    assert_eq!(
        X86ScalarDecoder::decode(&[0xf0, 0x84, 0x00], 0),
        Err(ScalarIrError::Invalid)
    );
    assert!(X86ScalarDecoder::decode(&[0xf0, 0x01, 0x00], 0).is_ok());
    assert!(X86ScalarDecoder::decode(&[0xf3, 0x90], 0).is_ok());
    assert!(matches!(
        X86ScalarDecoder::decode(&[0xc5, 0xf8, 0x77], 0).unwrap().instruction,
        ScalarInstruction::VexZeroUpper
    ));
    assert_eq!(
        X86ScalarDecoder::decode(&[0xd9, 0xef], 0),
        Err(ScalarIrError::Unsupported)
    );
    assert!(matches!(
        X86ScalarDecoder::decode(&[0x48], 0),
        Err(ScalarIrError::Structural(DecodeError::Truncated))
    ));
    let mut maximum = vec![0x66; 14];
    maximum.push(0x90);
    assert_eq!(X86ScalarDecoder::decode(&maximum, 0).unwrap().length, 15);
    assert!(matches!(
        X86ScalarDecoder::decode(&[0x66; 15], 0),
        Err(ScalarIrError::Structural(DecodeError::TooLong))
    ));
}

#[test]
fn x86_scalar_operands() {
    let moffs = X86ScalarDecoder::decode(&[0x67, 0xa1, 0x78, 0x56, 0x34, 0x12], 0).unwrap();
    assert!(matches!(
        moffs.instruction,
        ScalarInstruction::Move {
            source: ScalarOperand::Memory(EffectiveAddress {
                displacement: 0x12345678,
                address_32: true,
                ..
            }),
            ..
        }
    ));
    assert!(matches!(
        X86ScalarDecoder::decode(&[0xa8, 0x80], 0).unwrap().instruction,
        ScalarInstruction::Alu {
            operation: AluOperation::Test,
            source: ScalarOperand::Immediate(-128),
            ..
        }
    ));
    assert!(matches!(
        X86ScalarDecoder::decode(&[0xff, 0xd0], 0).unwrap().instruction,
        ScalarInstruction::CallIndirect {
            target: ScalarOperand::Register(ScalarRegister::General(0))
        }
    ));
    assert!(matches!(
        X86ScalarDecoder::decode(&[0xff, 0x20], 0).unwrap().instruction,
        ScalarInstruction::JumpIndirect {
            target: ScalarOperand::Memory(_)
        }
    ));
}
