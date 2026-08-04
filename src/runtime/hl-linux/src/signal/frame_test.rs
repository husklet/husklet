use hl_isa::GuestArchitecture;
use hl_task::{AlternateStack, SignalAction, SignalDisposition, SignalInfo, SignalMask, SignalNumber};

use crate::{
    AARCH64_SIGNAL_FRAME_SIZE, Aarch64SignalMachine, SignalFrameCodec, SignalFrameError, SignalFrameRequest,
    SignalMachine, X86_SIGNAL_FRAME_SIZE, X86SignalMachine,
};

fn action(handler: u64, flags: u64) -> SignalAction {
    SignalAction {
        disposition: SignalDisposition::Handler(handler),
        flags,
        restorer: 0,
        mask: SignalMask::from_bits(1 << 6),
    }
}

fn information() -> SignalInfo {
    SignalInfo {
        signal: SignalNumber::new(35).unwrap(),
        error: 7,
        code: -1,
        sender_process: 41,
        sender_user: 42,
        value: 0x8877,
        address: 0,
        source_tag: 0,
    }
}

#[test]
fn fault_frame_exact() {
    const FAULT: u64 = 0x40_123;
    let information = SignalInfo {
        signal: SignalNumber::new(4).unwrap(),
        error: 0,
        code: 2,
        sender_process: 0,
        sender_user: 0,
        value: 0,
        address: FAULT,
        source_tag: 0,
    };
    let aarch64 = SignalFrameRequest {
        machine: SignalMachine::Aarch64(Aarch64SignalMachine {
            registers: [0; 31],
            vectors: [0; 32],
            stack_pointer: 0x20_000,
            program_counter: FAULT,
            pstate: 0,
            fpcr: 0,
            fpsr: 0,
        }),
        information,
        action: action(0x50_000, 0),
        mask: SignalMask::from_bits(0),
        alternate_stack: AlternateStack::Disabled,
        sigreturn_pc: 0x60_000,
    };
    let image = SignalFrameCodec::build(&aarch64).unwrap();
    assert_eq!(&image.bytes[8..12], &2_i32.to_le_bytes());
    assert_eq!(&image.bytes[16..24], &FAULT.to_le_bytes());
    assert_eq!(&image.bytes[568..576], &FAULT.to_le_bytes());

    let mut registers = [0; 16];
    registers[4] = 0x20_000;
    let x86 = SignalFrameRequest {
        machine: SignalMachine::X86_64(X86SignalMachine {
            registers,
            vectors: [0; 16],
            vector_upper: [0; 16],
            stack_pointer: 0x20_000,
            instruction_pointer: FAULT,
            rflags: 0x202,
        }),
        information,
        action: action(0x50_000, 0),
        mask: SignalMask::from_bits(0),
        alternate_stack: AlternateStack::Disabled,
        sigreturn_pc: 0x60_000,
    };
    let image = SignalFrameCodec::build(&x86).unwrap();
    assert_eq!(&image.bytes[528..532], &2_i32.to_le_bytes());
    assert_eq!(&image.bytes[536..544], &FAULT.to_le_bytes());
    assert_eq!(&image.bytes[176..184], &FAULT.to_le_bytes());
}

#[test]
fn aarch64_frame_trips() {
    let mut machine = Aarch64SignalMachine {
        registers: std::array::from_fn(|index| 0x1000 + index as u64),
        vectors: std::array::from_fn(|index| 0x2000 + index as u128),
        stack_pointer: 0x20_000,
        program_counter: 0x40_000,
        pstate: 0xa000_0000,
        fpcr: 3,
        fpsr: 4,
    };
    machine.registers[30] = 0x7777;
    let request = SignalFrameRequest {
        machine: SignalMachine::Aarch64(machine.clone()),
        information: information(),
        action: action(0x50_000, 0),
        mask: SignalMask::from_bits(1 << 9),
        alternate_stack: AlternateStack::Disabled,
        sigreturn_pc: 0x60_000,
    };
    let image = SignalFrameCodec::build(&request).unwrap();
    assert_eq!(image.bytes.len(), AARCH64_SIGNAL_FRAME_SIZE);
    assert_eq!(image.write_address & 15, 0);
    assert_eq!(&image.bytes[0..4], &35_i32.to_le_bytes());
    assert_eq!(&image.bytes[128 + 40..128 + 48], &(1_u64 << 9).to_le_bytes());
    assert_eq!(&image.bytes[304 + 8..304 + 16], &0x1000_u64.to_le_bytes());
    assert_eq!(&image.bytes[592..596], &0x4650_8001_u32.to_le_bytes());
    let restored = SignalFrameCodec::restore(GuestArchitecture::Aarch64, image.write_address, &image.bytes).unwrap();
    assert_eq!(restored.machine, request.machine);
    assert_eq!(restored.mask, request.mask);
}

#[test]
fn x86_frame_trips() {
    let registers = std::array::from_fn(|index| 0x3000 + index as u64);
    let machine = X86SignalMachine {
        registers,
        vectors: std::array::from_fn(|index| 0x4000 + index as u128),
        vector_upper: std::array::from_fn(|index| 0x5000 + index as u128),
        stack_pointer: registers[4],
        instruction_pointer: 0x40_000,
        rflags: 0x246,
    };
    let request = SignalFrameRequest {
        machine: SignalMachine::X86_64(machine.clone()),
        information: information(),
        action: action(0x50_000, 0),
        mask: SignalMask::from_bits(1 << 9),
        alternate_stack: AlternateStack::Disabled,
        sigreturn_pc: 0x60_000,
    };
    let image = SignalFrameCodec::build(&request).unwrap();
    assert_eq!(image.bytes.len(), X86_SIGNAL_FRAME_SIZE);
    assert_eq!(image.write_address & 15, 8);
    assert_eq!(&image.bytes[0..8], &0x60_000_u64.to_le_bytes());
    assert_eq!(&image.bytes[8 + 512..8 + 516], &35_i32.to_le_bytes());
    assert_eq!(&image.bytes[8 + 296..8 + 304], &(1_u64 << 9).to_le_bytes());
    let restored =
        SignalFrameCodec::restore(GuestArchitecture::X86_64, image.write_address + 8, &image.bytes[8..]).unwrap();
    assert_eq!(restored.machine, request.machine);
    assert_eq!(restored.mask, request.mask);
}

#[test]
fn nested_alt_stack() {
    const POINTER: u64 = 0x30_000;
    const SIZE: u64 = 0x10_000;
    const CURRENT: u64 = 0x38_000;
    let stack = AlternateStack::Active {
        pointer: POINTER,
        size: SIZE,
    };
    let machines = [
        SignalMachine::Aarch64(Aarch64SignalMachine {
            registers: [0; 31],
            vectors: [0; 32],
            stack_pointer: CURRENT,
            program_counter: 0x40_000,
            pstate: 0,
            fpcr: 0,
            fpsr: 0,
        }),
        SignalMachine::X86_64({
            let mut registers = [0; 16];
            registers[4] = CURRENT;
            X86SignalMachine {
                registers,
                vectors: [0; 16],
                vector_upper: [0; 16],
                stack_pointer: CURRENT,
                instruction_pointer: 0x40_000,
                rflags: 0x202,
            }
        }),
    ];
    for machine in machines {
        let request = SignalFrameRequest {
            machine,
            information: information(),
            action: action(0x50_000, 0x0800_0000),
            mask: SignalMask::from_bits(0),
            alternate_stack: stack,
            sigreturn_pc: 0x60_000,
        };
        let image = SignalFrameCodec::build(&request).unwrap();
        assert!(image.write_address < CURRENT);
        assert!(image.write_address >= POINTER);
        assert_eq!(image.handler_alternate_stack, stack);
        let offset = if matches!(&request.machine, SignalMachine::Aarch64(_)) {
            128
        } else {
            8
        };
        assert_eq!(&image.bytes[offset + 16..offset + 24], &POINTER.to_le_bytes());
        assert_eq!(&image.bytes[offset + 24..offset + 28], &1_u32.to_le_bytes());
        assert_eq!(&image.bytes[offset + 32..offset + 40], &SIZE.to_le_bytes());
        let (architecture, address, bytes) = match &request.machine {
            SignalMachine::Aarch64(_) => (GuestArchitecture::Aarch64, image.write_address, image.bytes.as_slice()),
            SignalMachine::X86_64(_) => (GuestArchitecture::X86_64, image.write_address + 8, &image.bytes[8..]),
        };
        let restored = SignalFrameCodec::restore(architecture, address, bytes).unwrap();
        assert_eq!(restored.alternate_stack, stack, "{architecture:?}");
    }
}

#[test]
fn autodisarm_stack() {
    const POINTER: u64 = 0x30_000;
    const SIZE: u64 = 0x10_000;
    let stack = AlternateStack::Autodisarm {
        pointer: POINTER,
        size: SIZE,
    };
    let machines = [
        SignalMachine::Aarch64(Aarch64SignalMachine {
            registers: [0; 31],
            vectors: [0; 32],
            stack_pointer: 0x20_000,
            program_counter: 0x40_000,
            pstate: 0,
            fpcr: 0,
            fpsr: 0,
        }),
        SignalMachine::X86_64({
            let mut registers = [0; 16];
            registers[4] = 0x20_000;
            X86SignalMachine {
                registers,
                vectors: [0; 16],
                vector_upper: [0; 16],
                stack_pointer: 0x20_000,
                instruction_pointer: 0x40_000,
                rflags: 0x202,
            }
        }),
    ];
    for machine in machines {
        let request = SignalFrameRequest {
            machine,
            information: information(),
            action: action(0x50_000, 0x0800_0000),
            mask: SignalMask::from_bits(0),
            alternate_stack: stack,
            sigreturn_pc: 0x60_000,
        };
        let image = SignalFrameCodec::build(&request).unwrap();
        assert_eq!(image.handler_alternate_stack, AlternateStack::Disabled);
        let offset = if matches!(&request.machine, SignalMachine::Aarch64(_)) {
            128
        } else {
            8
        };
        assert_eq!(&image.bytes[offset + 24..offset + 28], &0x8000_0000_u32.to_le_bytes());
        let (architecture, address, bytes) = match &request.machine {
            SignalMachine::Aarch64(_) => (GuestArchitecture::Aarch64, image.write_address, image.bytes.as_slice()),
            SignalMachine::X86_64(_) => (GuestArchitecture::X86_64, image.write_address + 8, &image.bytes[8..]),
        };
        assert_eq!(
            SignalFrameCodec::restore(architecture, address, bytes)
                .unwrap()
                .alternate_stack,
            stack,
        );
    }
}

#[test]
fn malformed_extension_closed() {
    let request = SignalFrameRequest {
        machine: SignalMachine::Aarch64(Aarch64SignalMachine {
            registers: [0; 31],
            vectors: [0; 32],
            stack_pointer: 0x20_000,
            program_counter: 0x40_000,
            pstate: 0,
            fpcr: 0,
            fpsr: 0,
        }),
        information: information(),
        action: action(0x50_000, 0),
        mask: SignalMask::from_bits(0),
        alternate_stack: AlternateStack::Disabled,
        sigreturn_pc: 0x60_000,
    };
    let image = SignalFrameCodec::build(&request).unwrap();
    let mut malformed = image.bytes.clone();
    malformed[592 + 4..592 + 8].copy_from_slice(&16_u32.to_le_bytes());
    assert_eq!(
        SignalFrameCodec::restore(GuestArchitecture::Aarch64, image.write_address, &malformed,),
        Err(SignalFrameError::Malformed),
    );
    let mut privileged = image.bytes;
    privileged[304 + 272..304 + 280].copy_from_slice(&1_u64.to_le_bytes());
    assert_eq!(
        SignalFrameCodec::restore(GuestArchitecture::Aarch64, image.write_address, &privileged,),
        Err(SignalFrameError::UnsupportedState),
    );
}

#[test]
fn x86_noncanonical_closed() {
    let mut registers = [0; 16];
    registers[4] = 0x20_000;
    let request = SignalFrameRequest {
        machine: SignalMachine::X86_64(X86SignalMachine {
            registers,
            vectors: [0; 16],
            vector_upper: [0; 16],
            stack_pointer: 0x20_000,
            instruction_pointer: 0x40_000,
            rflags: 0x202,
        }),
        information: information(),
        action: action(0x50_000, 0),
        mask: SignalMask::from_bits(0),
        alternate_stack: AlternateStack::Disabled,
        sigreturn_pc: 0x60_000,
    };
    let image = SignalFrameCodec::build(&request).unwrap();
    for (offset, value) in [
        (40 + 16 * 8, 0x0001_0000_0000_0000_u64),
        (40 + 17 * 8, 1_u64 << 63),
        (40 + 18 * 8, 0x33_u64),
    ] {
        let mut frame = image.bytes[8..].to_vec();
        frame[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        assert_eq!(
            SignalFrameCodec::restore(GuestArchitecture::X86_64, image.write_address + 8, &frame,),
            Err(SignalFrameError::UnsupportedState),
        );
    }
}
