use hl_isa::{CoreRegister, GuestArchitecture};

use crate::{Errno, LinuxResult, RegisterView, RestartKind, SyscallFrameDecoder};

struct Registers {
    values: [u64; 16],
}

impl RegisterView for Registers {
    fn read(&self, register: CoreRegister) -> Option<u64> {
        match register {
            CoreRegister::GeneralPurpose(index) => self.values.get(usize::from(index)).copied(),
            _ => None,
        }
    }

    fn write(&mut self, register: CoreRegister, value: u64) -> bool {
        let CoreRegister::GeneralPurpose(index) = register else {
            return false;
        };
        let Some(slot) = self.values.get_mut(usize::from(index)) else {
            return false;
        };
        *slot = value;
        true
    }
}

#[test]
fn aarch64_decodes_x5() {
    let mut registers = Registers { values: [0; 16] };
    registers.values[8] = 56;
    registers.values[..6].copy_from_slice(&[10, 11, 12, 13, 14, 15]);
    let frame = SyscallFrameDecoder::decode(GuestArchitecture::Aarch64, &registers).unwrap();
    assert_eq!(frame.raw_number, 56);
    assert_eq!(frame.arguments, [10, 11, 12, 13, 14, 15]);
}

#[test]
fn x86_64_order() {
    let mut registers = Registers { values: [0; 16] };
    registers.values[0] = 257;
    for (index, value) in [(7, 1), (6, 2), (2, 3), (10, 4), (8, 5), (9, 6)] {
        registers.values[index] = value;
    }
    let frame = SyscallFrameDecoder::decode(GuestArchitecture::X86_64, &registers).unwrap();
    assert_eq!(frame.raw_number, 257);
    assert_eq!(frame.arguments, [1, 2, 3, 4, 5, 6]);
}

#[test]
fn results_restart_register() {
    let mut registers = Registers { values: [0; 16] };
    let frame = SyscallFrameDecoder::decode(GuestArchitecture::Aarch64, &registers).unwrap();
    for result in [
        LinuxResult::Value(u64::MAX),
        LinuxResult::Error(Errno::EFAULT),
        LinuxResult::Restart(RestartKind::RestartBlock),
    ] {
        SyscallFrameDecoder::write_result(&frame, &mut registers, result).unwrap();
        assert_eq!(registers.values[0], result.encode());
    }
    assert_eq!(LinuxResult::Error(Errno::EFAULT).encode(), (-14_i64) as u64);
    assert_eq!(
        LinuxResult::Restart(RestartKind::RestartBlock).encode(),
        (-516_i64) as u64
    );
}
