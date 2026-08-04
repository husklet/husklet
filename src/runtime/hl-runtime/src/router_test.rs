use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use hl_execution::{Aarch64CpuState, CpuState, ExecutionCpuSnapshot};
use hl_isa::GuestArchitecture;
use hl_linux::{
    AioSyscalls, DescriptorIoSyscalls, EventSyscalls, FilesystemSyscalls, GuestAccess, GuestFault, GuestMemory,
    IpcSyscalls, LinuxResult, MemorySyscalls, NetworkSyscalls, RestartKind, SeccompSyscalls, SyscallDispatcher,
    SyscallDisposition, SyscallFamily, SyscallOperation, TaskSignalTimeSyscalls,
};

use crate::{
    ExecQueue, PreparedExec, RouterDependencies, RuntimeExecError, RuntimeSyscallRouter, RuntimeSyscallTrap,
    RuntimeTerminal, RuntimeTrapOutcome, SignalBoundaryPort,
};

struct ExecToken(Arc<AtomicUsize>);

impl PreparedExec for ExecToken {
    fn commit(self: Box<Self>) -> Result<(), RuntimeExecError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

fn task_thread() -> hl_task::ThreadId {
    let tasks = hl_task::TaskRegistry::new(hl_task::RegistryConfig::default()).unwrap();
    let credentials = hl_task::ProcessCredentials::new(0, 0, &[], 1).unwrap();
    tasks
        .create_init(credentials, hl_task::ProcessLimits::empty())
        .unwrap()
        .1
}

fn task_identity() -> (hl_task::ProcessId, hl_task::ThreadId) {
    let tasks = hl_task::TaskRegistry::new(hl_task::RegistryConfig::default()).unwrap();
    let credentials = hl_task::ProcessCredentials::new(0, 0, &[], 1).unwrap();
    tasks.create_init(credentials, hl_task::ProcessLimits::empty()).unwrap()
}

struct Port {
    calls: Arc<Mutex<Vec<&'static str>>>,
    result: LinuxResult,
}

impl Port {
    fn handle(&self, operation: SyscallOperation) -> LinuxResult {
        self.calls.lock().unwrap().push(operation.name);
        self.result
    }
}

macro_rules! port {
    ($trait:ident) => {
        impl $trait for Port {
            fn handle(&mut self, operation: SyscallOperation, _: [u64; 6]) -> LinuxResult {
                Port::handle(self, operation)
            }
        }
    };
}

port!(FilesystemSyscalls);
port!(AioSyscalls);
port!(DescriptorIoSyscalls);
port!(EventSyscalls);
port!(MemorySyscalls);
port!(NetworkSyscalls);
port!(TaskSignalTimeSyscalls);
port!(IpcSyscalls);
port!(SeccompSyscalls);

struct Fixture {
    calls: Arc<Mutex<Vec<&'static str>>>,
}

struct DecisionPort(hl_linux::SeccompDecision);

impl SeccompSyscalls for DecisionPort {
    fn handle(&mut self, _: SyscallOperation, _: [u64; 6]) -> LinuxResult {
        LinuxResult::Error(hl_linux::Errno::ENOSYS)
    }

    fn evaluate(&self, _: &hl_linux::SyscallFrame, _: u64) -> hl_linux::SeccompDecision {
        self.0
    }
}

struct KillPort(Arc<Mutex<Vec<(hl_linux::SeccompKillScope, u8)>>>);

impl SignalBoundaryPort for KillPort {
    fn deliver(&mut self) -> Result<crate::SignalBoundaryOutcome, ()> {
        Ok(crate::SignalBoundaryOutcome::None)
    }

    fn restore(&mut self) -> Result<(), ()> {
        Ok(())
    }

    fn kill(&mut self, scope: hl_linux::SeccompKillScope, signal: u8) -> Result<(), ()> {
        self.0.lock().unwrap().push((scope, signal));
        Ok(())
    }
}

struct Memory;

impl GuestMemory for Memory {
    fn probe(&self, _: u64, length: usize, _: GuestAccess) -> Result<usize, GuestFault> {
        Ok(length)
    }

    fn read(&self, _: u64, _: &mut [u8]) -> Result<usize, GuestFault> {
        unreachable!()
    }

    fn write(&self, _: u64, source: &[u8]) -> Result<usize, GuestFault> {
        Ok(source.len())
    }
}

impl Fixture {
    fn router(&self, result: LinuxResult) -> RuntimeSyscallRouter {
        let port = || Port {
            calls: self.calls.clone(),
            result,
        };
        RuntimeSyscallRouter::new(RouterDependencies {
            aio: Box::new(port()),
            process_fork: None,
            thread_clone: None,
            architecture_memory: Box::new(Memory),
            filesystem: Box::new(port()),
            descriptor_io: Box::new(port()),
            event: Box::new(port()),
            memory: Box::new(port()),
            network: Box::new(port()),
            task_signal_time: Box::new(port()),
            ipc: Box::new(port()),
            seccomp: Box::new(port()),
        })
    }

    fn clone_router(&self, clone: Box<dyn crate::ThreadCloneTrapPort>) -> RuntimeSyscallRouter {
        self.router(LinuxResult::Error(hl_linux::Errno::ENOSYS))
            .with_clone(clone)
    }

    fn kill_router(
        &self,
        decision: hl_linux::SeccompDecision,
        calls: Arc<Mutex<Vec<(hl_linux::SeccompKillScope, u8)>>>,
    ) -> RuntimeSyscallRouter {
        let port = || Port {
            calls: self.calls.clone(),
            result: LinuxResult::Value(0),
        };
        RuntimeSyscallRouter::new(RouterDependencies {
            aio: Box::new(port()),
            process_fork: None,
            thread_clone: None,
            architecture_memory: Box::new(Memory),
            filesystem: Box::new(port()),
            descriptor_io: Box::new(port()),
            event: Box::new(port()),
            memory: Box::new(port()),
            network: Box::new(port()),
            task_signal_time: Box::new(port()),
            ipc: Box::new(port()),
            seccomp: Box::new(DecisionPort(decision)),
        })
        .with_signal_boundary(Box::new(KillPort(calls)))
    }

    fn cpu(architecture: GuestArchitecture, number: u64, first: u64) -> ExecutionCpuSnapshot {
        match architecture {
            GuestArchitecture::Aarch64 => {
                let mut cpu = Aarch64CpuState::default();
                cpu.registers[8] = number;
                cpu.registers[0] = first;
                ExecutionCpuSnapshot::Aarch64(cpu)
            }
            GuestArchitecture::X86_64 => {
                let mut cpu = CpuState::default();
                cpu.registers[0] = number;
                cpu.registers[7] = first;
                ExecutionCpuSnapshot::X86_64(cpu)
            }
        }
    }

    fn result(cpu: &ExecutionCpuSnapshot) -> u64 {
        match cpu {
            ExecutionCpuSnapshot::Aarch64(cpu) => cpu.registers[0],
            ExecutionCpuSnapshot::X86_64(cpu) => cpu.registers[0],
        }
    }
}

struct CloneCapture(Arc<Mutex<Vec<(GuestArchitecture, hl_linux::ClonePlan)>>>);

struct ForkCapture(Arc<Mutex<Vec<(GuestArchitecture, hl_linux::ClonePlan)>>>);

impl crate::ThreadCloneTrapPort for CloneCapture {
    fn clone(&self, cpu: &ExecutionCpuSnapshot, plan: hl_linux::ClonePlan) -> LinuxResult {
        self.0.lock().unwrap().push((
            match cpu {
                ExecutionCpuSnapshot::Aarch64(_) => GuestArchitecture::Aarch64,
                ExecutionCpuSnapshot::X86_64(_) => GuestArchitecture::X86_64,
            },
            plan,
        ));
        LinuxResult::Value(55)
    }
}

impl crate::ProcessForkTrap for ForkCapture {
    fn fork(&self, cpu: &ExecutionCpuSnapshot, plan: hl_linux::ClonePlan) -> LinuxResult {
        self.0.lock().unwrap().push((
            match cpu {
                ExecutionCpuSnapshot::Aarch64(_) => GuestArchitecture::Aarch64,
                ExecutionCpuSnapshot::X86_64(_) => GuestArchitecture::X86_64,
            },
            plan,
        ));
        LinuxResult::Value(77)
    }
}

#[test]
fn clone_trap_isas() {
    let flags = 0x0000_0100 | 0x0000_0200 | 0x0000_0400 | 0x0000_0800 | 0x0001_0000 | 0x0004_0000 | 0x0008_0000;
    for (architecture, number) in [(GuestArchitecture::Aarch64, 220), (GuestArchitecture::X86_64, 56)] {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let fixture = Fixture {
            calls: Arc::new(Mutex::new(Vec::new())),
        };
        let router = fixture.clone_router(Box::new(CloneCapture(Arc::clone(&calls))));
        let mut cpu = Fixture::cpu(architecture, number, flags);
        match &mut cpu {
            ExecutionCpuSnapshot::Aarch64(cpu) => {
                cpu.registers[1] = 0x8000;
                cpu.registers[2] = 0x1000;
                cpu.registers[3] = 0x3000;
                cpu.registers[4] = 0x2000;
            }
            ExecutionCpuSnapshot::X86_64(cpu) => {
                cpu.registers[6] = 0x8000;
                cpu.registers[2] = 0x1000;
                cpu.registers[10] = 0x2000;
                cpu.registers[8] = 0x3000;
            }
        }
        assert_eq!(router.dispatch(architecture, &mut cpu), RuntimeTrapOutcome::Continue);
        let recorded = calls.lock().unwrap();
        assert_eq!(recorded[0].0, architecture);
        assert_eq!((recorded[0].1.stack, recorded[0].1.tls), (0x8000, 0x3000));
    }
}

#[test]
fn fork_trap_isas() {
    for (architecture, number) in [(GuestArchitecture::Aarch64, 220), (GuestArchitecture::X86_64, 56)] {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let fixture = Fixture {
            calls: Arc::new(Mutex::new(Vec::new())),
        };
        let router = fixture
            .router(LinuxResult::Error(hl_linux::Errno::ENOSYS))
            .with_fork(Box::new(ForkCapture(Arc::clone(&calls))));
        let mut cpu = Fixture::cpu(architecture, number, 17);
        assert_eq!(router.dispatch(architecture, &mut cpu), RuntimeTrapOutcome::Continue);
        assert_eq!(Fixture::result(&cpu), 77);
        let recorded = calls.lock().unwrap();
        assert_eq!(recorded[0].0, architecture);
        assert_eq!((recorded[0].1.flags, recorded[0].1.exit_signal), (0, 17));
    }
}

#[test]
fn isa_restart_encoding() {
    for (architecture, write) in [(GuestArchitecture::Aarch64, 64), (GuestArchitecture::X86_64, 1)] {
        let fixture = Fixture {
            calls: Arc::new(Mutex::new(Vec::new())),
        };
        let router = fixture.router(LinuxResult::Restart(RestartKind::NoHandler));
        let mut cpu = Fixture::cpu(architecture, write, 2);
        match &mut cpu {
            ExecutionCpuSnapshot::Aarch64(cpu) => cpu.pc = 0x104,
            ExecutionCpuSnapshot::X86_64(cpu) => cpu.rip = 0x102,
        }
        assert_eq!(router.dispatch(architecture, &mut cpu), RuntimeTrapOutcome::Continue,);
        match &cpu {
            ExecutionCpuSnapshot::Aarch64(cpu) => assert_eq!((cpu.pc, cpu.registers[0]), (0x100, 2)),
            ExecutionCpuSnapshot::X86_64(cpu) => assert_eq!((cpu.rip, cpu.registers[0]), (0x100, write)),
        }
        assert_eq!(*fixture.calls.lock().unwrap(), vec!["write"]);
    }
}

#[test]
fn exec_replaces_once() {
    for (architecture, number) in [(GuestArchitecture::Aarch64, 221), (GuestArchitecture::X86_64, 59)] {
        let fixture = Fixture {
            calls: Arc::new(Mutex::new(Vec::new())),
        };
        let thread = task_thread();
        let queue = Arc::new(ExecQueue::default());
        let commits = Arc::new(AtomicUsize::new(0));
        let key = queue.stage(thread, Box::new(ExecToken(Arc::clone(&commits)))).unwrap();
        let router = fixture
            .router(LinuxResult::Value(0))
            .with_exec_queue(thread, Arc::clone(&queue));
        let mut cpu = Fixture::cpu(architecture, number, 0x1234);
        let before = cpu.clone();
        assert_eq!(
            router.dispatch(architecture, &mut cpu),
            RuntimeTrapOutcome::ReplaceImage {
                generation: key.generation
            },
        );
        assert_eq!(cpu, before);
        router.take_exec(key.generation).unwrap().commit().unwrap();
        assert!(router.take_exec(key.generation).is_none());
        assert_eq!(commits.load(Ordering::Relaxed), 1);
    }
}

#[test]
fn exec_error_returns() {
    for (architecture, number) in [(GuestArchitecture::Aarch64, 221), (GuestArchitecture::X86_64, 59)] {
        let fixture = Fixture {
            calls: Arc::new(Mutex::new(Vec::new())),
        };
        let thread = task_thread();
        let queue = Arc::new(ExecQueue::default());
        let router = fixture
            .router(LinuxResult::Error(hl_linux::Errno::ENOENT))
            .with_exec_queue(thread, queue);
        let mut cpu = Fixture::cpu(architecture, number, 0x1234);
        assert_eq!(router.dispatch(architecture, &mut cpu), RuntimeTrapOutcome::Continue);
        assert_eq!(
            Fixture::result(&cpu),
            LinuxResult::Error(hl_linux::Errno::ENOENT).encode()
        );
    }
}

#[test]
fn trace_disabled_empty() {
    let fixture = Fixture {
        calls: Arc::new(Mutex::new(Vec::new())),
    };
    let router = fixture.router(LinuxResult::Value(0));
    let mut cpu = Fixture::cpu(GuestArchitecture::Aarch64, 64, 1);
    router.dispatch(GuestArchitecture::Aarch64, &mut cpu);
    assert!(router.trace().is_none());
}

#[test]
fn trace_wraps_bound() {
    let fixture = Fixture {
        calls: Arc::new(Mutex::new(Vec::new())),
    };
    let router = fixture.router(LinuxResult::Value(7)).with_trace(2);
    for first in 1..=3 {
        let mut cpu = Fixture::cpu(GuestArchitecture::Aarch64, 64, first);
        router.dispatch(GuestArchitecture::Aarch64, &mut cpu);
    }
    let trace = router.trace().unwrap();
    assert_eq!(trace.len(), 2);
    assert_eq!(trace[0].arguments[0], 2);
    assert_eq!(trace[1].arguments[0], 3);
}

#[test]
fn trace_engine_isolation() {
    let fixture = Fixture {
        calls: Arc::new(Mutex::new(Vec::new())),
    };
    let first = fixture.router(LinuxResult::Value(0)).with_trace(1);
    let second = fixture.router(LinuxResult::Value(0)).with_trace(1);
    let mut cpu = Fixture::cpu(GuestArchitecture::X86_64, 1, 9);
    first.dispatch(GuestArchitecture::X86_64, &mut cpu);
    assert_eq!(first.trace().unwrap().len(), 1);
    assert!(second.trace().unwrap().is_empty());
}

#[test]
fn x86_control_dispatch() {
    let fixture = Fixture {
        calls: Arc::new(Mutex::new(Vec::new())),
    };
    let router = fixture.router(LinuxResult::Error(hl_linux::Errno::ENOSYS));
    let mut cpu = Fixture::cpu(GuestArchitecture::X86_64, 158, 0x1002);
    let ExecutionCpuSnapshot::X86_64(state) = &mut cpu else {
        unreachable!()
    };
    state.registers[6] = 0x8003c0;
    assert_eq!(
        router.dispatch(GuestArchitecture::X86_64, &mut cpu),
        RuntimeTrapOutcome::Continue,
    );
    let ExecutionCpuSnapshot::X86_64(state) = cpu else {
        unreachable!()
    };
    assert_eq!((state.fs_base, state.registers[0]), (0x8003c0, 0));
    assert!(fixture.calls.lock().unwrap().is_empty());
}

#[test]
fn isa_guest_status() {
    for (architecture, exit) in [(GuestArchitecture::Aarch64, 93), (GuestArchitecture::X86_64, 60)] {
        let fixture = Fixture {
            calls: Arc::new(Mutex::new(Vec::new())),
        };
        let router = fixture.router(LinuxResult::Value(0));
        let mut cpu = Fixture::cpu(architecture, exit, 37);
        assert_eq!(router.dispatch(architecture, &mut cpu), RuntimeTrapOutcome::Exit(37),);
        assert_eq!(router.take_terminal(), Some(RuntimeTerminal::Thread(37)));
        assert_eq!(router.take_terminal(), None);
        assert_eq!(*fixture.calls.lock().unwrap(), vec!["exit"]);
    }
}

#[test]
fn group_exit_scope() {
    for (architecture, exit) in [(GuestArchitecture::Aarch64, 94), (GuestArchitecture::X86_64, 231)] {
        let fixture = Fixture {
            calls: Arc::new(Mutex::new(Vec::new())),
        };
        let router = fixture.router(LinuxResult::Value(0));
        let mut cpu = Fixture::cpu(architecture, exit, 23);
        assert_eq!(router.dispatch(architecture, &mut cpu), RuntimeTrapOutcome::Exit(23));
        assert_eq!(router.take_terminal(), Some(RuntimeTerminal::Group(23)));
        assert_eq!(*fixture.calls.lock().unwrap(), vec!["exit_group"]);
    }
}

#[test]
fn immutable_task_identity_bypasses_mutable_process_port() {
    let (process, thread) = task_identity();
    for (architecture, getpid, gettid) in [
        (GuestArchitecture::Aarch64, 172, 178),
        (GuestArchitecture::X86_64, 39, 186),
    ] {
        let fixture = Fixture {
            calls: Arc::new(Mutex::new(Vec::new())),
        };
        let router = fixture
            .router(LinuxResult::Error(hl_linux::Errno::ENOSYS))
            .with_task_identity(process, thread);
        for (number, expected) in [(getpid, process.number()), (gettid, thread.number())] {
            let mut cpu = Fixture::cpu(architecture, number, 0);
            assert_eq!(router.dispatch(architecture, &mut cpu), RuntimeTrapOutcome::Continue);
            assert_eq!(Fixture::result(&cpu), u64::from(expected));
        }
        assert!(fixture.calls.lock().unwrap().is_empty());
    }
}

#[test]
fn seccomp_kill_scope() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let syscall = match architecture {
            GuestArchitecture::Aarch64 => 172,
            GuestArchitecture::X86_64 => 39,
        };
        for (scope, terminal) in [
            (hl_linux::SeccompKillScope::Thread, RuntimeTerminal::Thread(159)),
            (hl_linux::SeccompKillScope::Process, RuntimeTerminal::Group(159)),
        ] {
            let fixture = Fixture {
                calls: Arc::new(Mutex::new(Vec::new())),
            };
            let kills = Arc::new(Mutex::new(Vec::new()));
            let router = fixture.kill_router(
                hl_linux::SeccompDecision::Kill { scope, signal: 31 },
                Arc::clone(&kills),
            );
            let mut cpu = Fixture::cpu(architecture, syscall, 0);
            assert_eq!(router.dispatch(architecture, &mut cpu), RuntimeTrapOutcome::Exit(159),);
            assert_eq!(router.take_terminal(), Some(terminal));
            assert_eq!(*kills.lock().unwrap(), [(scope, 31)]);
            assert!(fixture.calls.lock().unwrap().is_empty());
        }
    }
}

fn assert_known_disposition(disposition: SyscallDisposition) {
    match disposition {
        SyscallDisposition::Operation(operation) => assert!(matches!(
            operation.family,
            SyscallFamily::Aio
                | SyscallFamily::Filesystem
                | SyscallFamily::DescriptorIo
                | SyscallFamily::Event
                | SyscallFamily::Memory
                | SyscallFamily::Network
                | SyscallFamily::TaskSignalTime
                | SyscallFamily::Ipc
                | SyscallFamily::Seccomp
        )),
        SyscallDisposition::Unsupported { name, .. } => assert!(!name.is_empty()),
        SyscallDisposition::Reserved { .. } => {}
    }
}

#[test]
fn syscall_unknown_enosys() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        for raw in 0..1024 {
            assert_known_disposition(SyscallDispatcher::route(architecture, raw).disposition);
        }
        let fixture = Fixture {
            calls: Arc::new(Mutex::new(Vec::new())),
        };
        let router = fixture.router(LinuxResult::Value(99));
        let mut cpu = Fixture::cpu(architecture, u64::MAX, 0);
        assert_eq!(router.dispatch(architecture, &mut cpu), RuntimeTrapOutcome::Continue,);
        assert_eq!(
            Fixture::result(&cpu),
            LinuxResult::Error(hl_linux::Errno::ENOSYS).encode(),
        );
        assert!(fixture.calls.lock().unwrap().is_empty());
    }
}
