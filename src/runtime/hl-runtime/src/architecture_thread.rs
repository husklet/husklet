use hl_execution::ExecutionCpuSnapshot;
use hl_linux::{Errno, GuestAccess, GuestMemory, LinuxResult};

const ARCH_SET_GS: u64 = 0x1001;
const ARCH_SET_FS: u64 = 0x1002;
const ARCH_GET_FS: u64 = 0x1003;
const ARCH_GET_GS: u64 = 0x1004;
const X86_USER_LIMIT: u64 = 1 << 47;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ThreadControlOperation {
    SetGs,
    SetFs,
    GetFs,
    GetGs,
}

impl ThreadControlOperation {
    const fn decode(code: u64) -> Option<Self> {
        match code {
            ARCH_SET_GS => Some(Self::SetGs),
            ARCH_SET_FS => Some(Self::SetFs),
            ARCH_GET_FS => Some(Self::GetFs),
            ARCH_GET_GS => Some(Self::GetGs),
            _ => None,
        }
    }
}

pub(crate) fn arch_prctl(
    cpu: &mut ExecutionCpuSnapshot,
    memory: &(dyn GuestMemory + Send),
    code: u64,
    address: u64,
) -> LinuxResult {
    let ExecutionCpuSnapshot::X86_64(cpu) = cpu else {
        return LinuxResult::Error(Errno::ENOSYS);
    };
    let Some(operation) = ThreadControlOperation::decode(code) else {
        return LinuxResult::Error(Errno::EINVAL);
    };
    match operation {
        ThreadControlOperation::SetFs | ThreadControlOperation::SetGs => {
            if address >= X86_USER_LIMIT {
                return LinuxResult::Error(Errno::EPERM);
            }
            if operation == ThreadControlOperation::SetFs {
                cpu.fs_base = address;
            } else {
                cpu.gs_base = address;
            }
            LinuxResult::Value(0)
        }
        ThreadControlOperation::GetFs | ThreadControlOperation::GetGs => {
            let value = if operation == ThreadControlOperation::GetFs {
                cpu.fs_base
            } else {
                cpu.gs_base
            };
            if memory.probe(address, 8, GuestAccess::Write) != Ok(8)
                || memory.write(address, &value.to_le_bytes()) != Ok(8)
            {
                return LinuxResult::Error(Errno::EFAULT);
            }
            LinuxResult::Value(0)
        }
    }
}
