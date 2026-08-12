use std::sync::{Arc, Mutex};

use crate::cpu::{CpuState, StoppedRegisterImage, StoppedRegisters, TraceSafepointPort, X86Prstatus};
use hl_linux::{GuestAccess, GuestArchitecture, GuestFault, GuestMemory, LinuxResult};
use hl_task::{
    PendingTarget, ProcessCredentials, ProcessLimits, RegistryConfig, SignalAction, SignalDisposition, SignalInfo,
    SignalNumber, TaskRegistry, TracePermission, TraceResume, TraceStop, TraceWait,
};

use crate::RuntimeProcessSyscalls;

use super::{PtraceCatalog, PtracePort, TraceExchange};

#[derive(Clone)]
struct Memory(Arc<Mutex<Vec<u8>>>);

impl GuestMemory for Memory {
    fn probe(&self, _: u64, length: usize, _: GuestAccess) -> Result<usize, GuestFault> {
        Ok(length)
    }

    fn read(&self, address: u64, destination: &mut [u8]) -> Result<usize, GuestFault> {
        let bytes = self.0.lock().unwrap();
        destination.copy_from_slice(&bytes[address as usize..address as usize + destination.len()]);
        Ok(destination.len())
    }

    fn write(&self, address: u64, source: &[u8]) -> Result<usize, GuestFault> {
        let mut bytes = self.0.lock().unwrap();
        bytes[address as usize..address as usize + source.len()].copy_from_slice(source);
        Ok(source.len())
    }
}

#[test]
fn catalog_exchange() {
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let (tracer, thread) = tasks
        .create_init(ProcessCredentials::new(0, 0, &[], 8).unwrap(), ProcessLimits::empty())
        .unwrap();
    let (tracee, _) = tasks
        .commit_fork_process(tasks.begin_fork_process(thread).unwrap())
        .unwrap();
    let exchange = TraceExchange::new(Arc::new(Memory(Arc::new(Mutex::new(vec![0; 32])))));
    let catalog = PtraceCatalog::default();
    catalog.register(tracee, Arc::clone(&exchange));
    let link = tasks.trace_attach(tracer, tracee, TracePermission::Granted).unwrap();
    catalog.attached(link, tracee).unwrap();
    let image = StoppedRegisterImage::new(StoppedRegisters::X86(X86Prstatus::capture(&CpuState::default(), 9)));
    exchange.publish(image.clone()).unwrap();
    assert_eq!(catalog.registers(link), Ok(image.clone()));
    assert_eq!(exchange.restore(), Ok(image));
    let event = tasks.trace_stop(tracee, TraceStop::SyscallEntry).unwrap();
    assert_eq!(catalog.wait_status(event), (5 << 8) | 0x7f);
}

#[test]
fn unregister_releases_process_memory() {
    let process = hl_task::ProcessId::from_wire(7, 1).unwrap();
    let memory: Arc<dyn GuestMemory + Send + Sync> = Arc::new(Memory(Arc::new(Mutex::new(vec![0; 32]))));
    let exchange = TraceExchange::new(Arc::clone(&memory));
    let catalog = PtraceCatalog::default();
    catalog.register(process, exchange);
    assert_eq!(catalog.permission(process, process), TracePermission::Granted);
    assert_eq!(Arc::strong_count(&memory), 2);

    catalog.unregister(process);

    assert_eq!(catalog.permission(process, process), TracePermission::Denied);
    assert_eq!(Arc::strong_count(&memory), 1);
}

#[test]
fn trace_me_lifecycle() {
    let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
    let (parent, parent_thread) = tasks
        .create_init(ProcessCredentials::new(0, 0, &[], 8).unwrap(), ProcessLimits::empty())
        .unwrap();
    let (child, child_thread) = tasks
        .commit_fork_process(tasks.begin_fork_process(parent_thread).unwrap())
        .unwrap();
    let memory = Memory(Arc::new(Mutex::new(vec![0; 32])));
    let catalog = Arc::new(PtraceCatalog::default());
    catalog.register(child, TraceExchange::new(Arc::new(memory.clone())));
    let child_calls = RuntimeProcessSyscalls::new(
        Arc::clone(&tasks),
        child,
        child_thread,
        memory.clone(),
        GuestArchitecture::X86_64,
    )
    .with_ptrace(catalog.clone());
    let parent_calls = RuntimeProcessSyscalls::new(
        Arc::clone(&tasks),
        parent,
        parent_thread,
        memory,
        GuestArchitecture::X86_64,
    )
    .with_ptrace(catalog.clone());

    assert_eq!(child_calls.ptrace([0, 0, 0, 0, 0, 0]), LinuxResult::Value(0));
    let event = tasks.trace_stop(child, TraceStop::Group(19)).unwrap();
    assert_eq!(tasks.trace_peek(parent, Some(child)), Ok(TraceWait::Event(event)));
    assert_eq!(
        parent_calls.ptrace([0x4200, u64::from(child.number()), 0, 1, 0, 0]),
        LinuxResult::Value(0),
    );
    assert_eq!(catalog.wait_status(event), ((19_u32) << 8) | 0x7f);
}

#[test]
fn signal_delivery_order() {
    let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
    let (parent, parent_thread) = tasks
        .create_init(ProcessCredentials::new(0, 0, &[], 8).unwrap(), ProcessLimits::empty())
        .unwrap();
    let (child, child_thread) = tasks
        .commit_fork_process(tasks.begin_fork_process(parent_thread).unwrap())
        .unwrap();
    let memory = Memory(Arc::new(Mutex::new(vec![0; 32])));
    let catalog = Arc::new(PtraceCatalog::default());
    catalog.register(child, TraceExchange::new(Arc::new(memory.clone())));
    let child_calls = RuntimeProcessSyscalls::new(
        Arc::clone(&tasks),
        child,
        child_thread,
        memory,
        GuestArchitecture::X86_64,
    )
    .with_ptrace(catalog.clone());
    let link = tasks.trace_me(child).unwrap();
    catalog.attached(link, child).unwrap();
    let signal = SignalNumber::new(4).unwrap();
    tasks
        .enqueue_signal(
            PendingTarget::Thread(child_thread),
            SignalInfo {
                code: 2,
                address: 0x401000,
                ..SignalInfo::bare(signal)
            },
        )
        .unwrap();

    let outcome = child_calls.deliver_signal_boundary().unwrap();
    let crate::SignalBoundaryOutcome::Trace { event, signal: 4 } = outcome else {
        panic!("fault signal did not stop first");
    };
    assert_eq!(tasks.trace_wait(parent, Some(child)), Ok(TraceWait::Event(event)));
    tasks.trace_resume(parent, link, TraceResume::Continue(None)).unwrap();
    assert_eq!(
        tasks.trace_take_resume(child, link),
        Ok(Some(TraceResume::Continue(None)))
    );
    child_calls.resolve_trace_signal(None).unwrap();
    assert_eq!(
        child_calls.deliver_signal_boundary(),
        Ok(crate::SignalBoundaryOutcome::None)
    );

    tasks
        .set_action(
            child,
            signal,
            SignalAction {
                disposition: SignalDisposition::Ignore,
                ..SignalAction::DEFAULT
            },
        )
        .unwrap();
    tasks
        .enqueue_signal(
            PendingTarget::Thread(child_thread),
            SignalInfo {
                code: 2,
                address: 0x402000,
                ..SignalInfo::bare(signal)
            },
        )
        .unwrap();
    let crate::SignalBoundaryOutcome::Trace { event, .. } = child_calls.deliver_signal_boundary().unwrap() else {
        panic!()
    };
    tasks.trace_wait(parent, Some(child)).unwrap();
    tasks
        .trace_resume(parent, link, TraceResume::Continue(Some(4)))
        .unwrap();
    tasks.trace_take_resume(child, link).unwrap();
    child_calls.resolve_trace_signal(Some(4)).unwrap();
    assert_eq!(
        child_calls.deliver_signal_boundary(),
        Ok(crate::SignalBoundaryOutcome::None)
    );

    tasks
        .enqueue_signal(
            PendingTarget::Thread(child_thread),
            SignalInfo {
                code: 2,
                address: 0x403000,
                ..SignalInfo::bare(signal)
            },
        )
        .unwrap();
    assert!(matches!(
        child_calls.deliver_signal_boundary(),
        Ok(crate::SignalBoundaryOutcome::Trace { event: next, .. }) if next != event,
    ));
}
