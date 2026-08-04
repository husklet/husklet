use std::sync::Mutex;

use hl_execution::{Aarch64CpuState, CpuState, ExecutionCpuSnapshot};
use hl_linux::{Errno, GuestAccess, GuestFault, GuestMemory, LinuxResult};

use crate::architecture_thread::arch_prctl;

const BASE: u64 = 0x1000;

struct Memory {
    bytes: Mutex<[u8; 16]>,
    writable: bool,
}

impl GuestMemory for Memory {
    fn probe(&self, address: u64, length: usize, access: GuestAccess) -> Result<usize, GuestFault> {
        if self.writable && access == GuestAccess::Write && address == BASE && length == 8 {
            Ok(length)
        } else {
            Err(GuestFault { address, access })
        }
    }

    fn read(&self, _: u64, _: &mut [u8]) -> Result<usize, GuestFault> {
        unreachable!()
    }

    fn write(&self, address: u64, source: &[u8]) -> Result<usize, GuestFault> {
        if !self.writable || address != BASE || source.len() != 8 {
            return Err(GuestFault {
                address,
                access: GuestAccess::Write,
            });
        }
        self.bytes.lock().unwrap()[..8].copy_from_slice(source);
        Ok(8)
    }
}

fn memory(writable: bool) -> Memory {
    Memory {
        bytes: Mutex::new([0xaa; 16]),
        writable,
    }
}

#[test]
fn set_get_bases() {
    let mut cpu = ExecutionCpuSnapshot::X86_64(CpuState::default());
    let memory = memory(true);
    assert_eq!(arch_prctl(&mut cpu, &memory, 0x1002, 0x2340), LinuxResult::Value(0));
    assert_eq!(arch_prctl(&mut cpu, &memory, 0x1001, 0x5670), LinuxResult::Value(0));
    let ExecutionCpuSnapshot::X86_64(state) = &cpu else {
        unreachable!()
    };
    assert_eq!((state.fs_base, state.gs_base), (0x2340, 0x5670));
    assert_eq!(arch_prctl(&mut cpu, &memory, 0x1003, BASE), LinuxResult::Value(0));
    assert_eq!(
        u64::from_le_bytes(memory.bytes.lock().unwrap()[..8].try_into().unwrap()),
        0x2340
    );
    assert_eq!(arch_prctl(&mut cpu, &memory, 0x1004, BASE), LinuxResult::Value(0));
    assert_eq!(
        u64::from_le_bytes(memory.bytes.lock().unwrap()[..8].try_into().unwrap()),
        0x5670
    );
}

#[test]
fn errors_preserve_state() {
    let mut state = CpuState::default();
    state.fs_base = 7;
    state.gs_base = 9;
    let mut cpu = ExecutionCpuSnapshot::X86_64(state);
    let denied = memory(false);
    assert_eq!(
        arch_prctl(&mut cpu, &denied, 0x1003, BASE),
        LinuxResult::Error(Errno::EFAULT)
    );
    assert_eq!(denied.bytes.lock().unwrap().as_slice(), &[0xaa; 16]);
    assert_eq!(
        arch_prctl(&mut cpu, &denied, 0x9999, 0),
        LinuxResult::Error(Errno::EINVAL)
    );
    assert_eq!(
        arch_prctl(&mut cpu, &denied, 0x1002, 1 << 47),
        LinuxResult::Error(Errno::EPERM)
    );
    let ExecutionCpuSnapshot::X86_64(state) = cpu else {
        unreachable!()
    };
    assert_eq!((state.fs_base, state.gs_base), (7, 9));
}

#[test]
fn aarch64_not_exposed() {
    let mut cpu = ExecutionCpuSnapshot::Aarch64(Aarch64CpuState::default());
    assert_eq!(
        arch_prctl(&mut cpu, &memory(true), 0x1002, BASE),
        LinuxResult::Error(Errno::ENOSYS)
    );
}
