use std::sync::{Arc, Mutex};

use hl_linux::{
    FutexOperation, FutexPlan, FutexWaitVector, GuestAccess, GuestArchitecture, GuestFault, GuestMemory, LinuxResult,
    SyscallFamily, SyscallOperation, TaskSignalTimeSyscalls,
};
use hl_sync::FutexDeadline;
use hl_task::{ProcessCredentials, ProcessLimits, RegistryConfig, TaskRegistry};

use crate::{RuntimeFutexPort, RuntimeProcessSyscalls};

#[derive(Clone, Copy)]
struct Memory;

impl GuestMemory for Memory {
    fn probe(&self, _: u64, length: usize, _: GuestAccess) -> Result<usize, GuestFault> {
        Ok(length)
    }

    fn read(&self, _: u64, output: &mut [u8]) -> Result<usize, GuestFault> {
        output.fill(0);
        Ok(output.len())
    }

    fn write(&self, _: u64, input: &[u8]) -> Result<usize, GuestFault> {
        Ok(input.len())
    }
}

#[derive(Clone, Copy)]
struct WaitvMemory;

impl GuestMemory for WaitvMemory {
    fn probe(&self, _: u64, length: usize, _: GuestAccess) -> Result<usize, GuestFault> {
        Ok(length)
    }

    fn read(&self, address: u64, output: &mut [u8]) -> Result<usize, GuestFault> {
        if output.len() != 48 {
            return Err(GuestFault {
                address,
                access: GuestAccess::Read,
            });
        }
        for (index, record) in output.chunks_exact_mut(24).enumerate() {
            record[..8].copy_from_slice(&(3_u64 + index as u64 * 2).to_le_bytes());
            record[8..16].copy_from_slice(&(0x1000_u64 + index as u64 * 0x1000).to_le_bytes());
            record[16..20].copy_from_slice(&130_u32.to_le_bytes());
        }
        Ok(output.len())
    }

    fn write(&self, _: u64, input: &[u8]) -> Result<usize, GuestFault> {
        Ok(input.len())
    }
}

#[derive(Default)]
struct RecordingFutex {
    calls: Mutex<Vec<(hl_task::ProcessId, hl_task::ThreadId, FutexPlan)>>,
    waitv: Mutex<Vec<(hl_task::ThreadId, Vec<FutexWaitVector>, Option<FutexDeadline>)>>,
}

impl RuntimeFutexPort for RecordingFutex {
    fn execute(&self, process: hl_task::ProcessId, thread: hl_task::ThreadId, plan: FutexPlan) -> LinuxResult {
        self.calls.lock().unwrap().push((process, thread, plan));
        LinuxResult::Value(7)
    }

    fn wait_multiple(
        &self,
        thread: hl_task::ThreadId,
        vectors: &[FutexWaitVector],
        deadline: Option<FutexDeadline>,
    ) -> LinuxResult {
        self.waitv.lock().unwrap().push((thread, vectors.to_vec(), deadline));
        LinuxResult::Value(11)
    }
}

#[test]
fn port_enosys() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
        let (process, thread) = tasks
            .create_init(
                ProcessCredentials::new(0, 0, &[], 65_536).unwrap(),
                ProcessLimits::default(),
            )
            .unwrap();
        let operation = SyscallOperation {
            canonical_number: 449,
            name: "futex_waitv",
            family: SyscallFamily::TaskSignalTime,
        };
        let mut absent = RuntimeProcessSyscalls::new(tasks.clone(), process, thread, WaitvMemory, architecture);
        assert_eq!(
            absent.handle(operation, [0x8000, 2, 0, 0, 1, 0]),
            LinuxResult::Error(hl_linux::Errno::ENOSYS),
        );
        let port = Arc::new(RecordingFutex::default());
        let mut runtime = RuntimeProcessSyscalls::new(tasks, process, thread, WaitvMemory, architecture)
            .with_futex_port(port.clone());
        assert_eq!(
            runtime.handle(operation, [0x8000, 2, 0, 0, 1, 0]),
            LinuxResult::Value(11),
        );
        let calls = port.waitv.lock().unwrap();
        assert_eq!(calls[0].0, thread);
        assert_eq!(calls[0].1[0].address, 0x1000);
        assert_eq!(calls[0].1[1].value, 5);
        assert!(calls[0].1.iter().all(|vector| vector.private));
        assert_eq!(calls[0].2, None);
    }
}

fn operation() -> SyscallOperation {
    SyscallOperation {
        canonical_number: 0,
        name: "futex",
        family: SyscallFamily::TaskSignalTime,
    }
}

#[test]
fn identity_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
        let (process, thread) = tasks
            .create_init(
                ProcessCredentials::new(0, 0, &[], 65_536).unwrap(),
                ProcessLimits::default(),
            )
            .unwrap();
        let mut absent = RuntimeProcessSyscalls::new(tasks.clone(), process, thread, Memory, architecture);
        assert_eq!(
            absent.handle(operation(), [3, 1, 1, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EINVAL),
        );
        assert_eq!(
            absent.handle(operation(), [4, 1 | 128, 1, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::ENOSYS),
        );

        let port = Arc::new(RecordingFutex::default());
        let mut runtime =
            RuntimeProcessSyscalls::new(tasks, process, thread, Memory, architecture).with_futex_port(port.clone());
        assert_eq!(
            runtime.handle(operation(), [4, 1 | 128, 3, 0, 0, 0]),
            LinuxResult::Value(7),
        );
        for encoded in [6_u64, 8, 7, 13] {
            assert_eq!(
                runtime.handle(operation(), [4, encoded | 128, 0, 0, 0, 0]),
                LinuxResult::Value(7),
            );
        }
        let calls = port.calls.lock().unwrap();
        assert_eq!(calls.len(), 5);
        assert_eq!(calls[0].0, process);
        assert_eq!(calls[0].1, thread);
        assert_eq!(calls[0].2.operation, FutexOperation::Wake);
        assert!(calls[0].2.private);
        assert_eq!(calls[0].2.value, 3);
        assert_eq!(
            calls.iter().skip(1).map(|call| call.2.operation).collect::<Vec<_>>(),
            vec![
                FutexOperation::LockPriorityInheritance,
                FutexOperation::TryLockPriorityInheritance,
                FutexOperation::UnlockPriorityInheritance,
                FutexOperation::LockPriorityInheritance2,
            ],
        );
    }
}
