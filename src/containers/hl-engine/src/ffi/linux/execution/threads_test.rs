use std::sync::Arc;

use hl_execution::{Aarch64CpuState, EXECUTION_SNAPSHOT_VERSION, ExecutionCpuSnapshot, ExecutionSnapshot};
use hl_runtime::{PreparedThread, RuntimeThreadError, RuntimeThreadPort};
use hl_runtime::{RouterDependencies, RuntimeSyscallRouter, RuntimeSyscallTrap};
use hl_task::{ProcessCredentials, ProcessLimits, RegistryConfig, TaskRegistry};

use super::threads::ThreadSet;

struct TestMemory;
struct TestPort;

macro_rules! test_port {
    ($trait_name:ident) => {
        impl hl_linux::$trait_name for TestPort {
            fn handle(&mut self, _: hl_linux::SyscallOperation, _: [u64; 6]) -> hl_linux::LinuxResult {
                hl_linux::LinuxResult::Error(hl_linux::Errno::ENOSYS)
            }
        }
    };
}

test_port!(FilesystemSyscalls);
test_port!(DescriptorIoSyscalls);
test_port!(EventSyscalls);
test_port!(AioSyscalls);
test_port!(MemorySyscalls);
test_port!(NetworkSyscalls);
test_port!(TaskSignalTimeSyscalls);
test_port!(IpcSyscalls);
test_port!(SeccompSyscalls);

impl hl_linux::GuestMemory for TestMemory {
    fn probe(&self, _: u64, length: usize, _: hl_linux::GuestAccess) -> Result<usize, hl_linux::GuestFault> {
        Ok(length)
    }

    fn read(&self, _: u64, destination: &mut [u8]) -> Result<usize, hl_linux::GuestFault> {
        destination.fill(0);
        Ok(destination.len())
    }

    fn write(&self, _: u64, source: &[u8]) -> Result<usize, hl_linux::GuestFault> {
        Ok(source.len())
    }
}

fn context() -> (
    std::sync::Arc<RuntimeSyscallRouter>,
    std::sync::Arc<super::readiness::Cancellation>,
) {
    context_with_trace(true)
}

fn context_with_trace(
    trace: bool,
) -> (
    std::sync::Arc<RuntimeSyscallRouter>,
    std::sync::Arc<super::readiness::Cancellation>,
) {
    let dependencies = RouterDependencies {
        aio: Box::new(TestPort),
        architecture_memory: Box::new(TestMemory),
        process_fork: None,
        thread_clone: None,
        filesystem: Box::new(TestPort),
        descriptor_io: Box::new(TestPort),
        event: Box::new(TestPort),
        memory: Box::new(TestPort),
        network: Box::new(TestPort),
        task_signal_time: Box::new(TestPort),
        ipc: Box::new(TestPort),
        seccomp: Box::new(TestPort),
    };
    let router = RuntimeSyscallRouter::new(dependencies);
    let router = if trace { router.with_trace(4) } else { router };
    (
        std::sync::Arc::new(router),
        std::sync::Arc::new(super::readiness::Cancellation::new().unwrap()),
    )
}

fn space() -> std::sync::Arc<super::space::AddressSpace> {
    let arena = std::sync::Arc::new(super::VirtualMemory::reserve(4096).unwrap());
    let mappings = std::sync::Arc::new(hl_memory::MappingCoordinator::new(super::MappingHostAdapter::new(
        std::sync::Arc::clone(&arena),
    )));
    super::space::AddressSpace::new(arena, mappings)
}

fn mapped_space(identity: u64) -> std::sync::Arc<super::space::AddressSpace> {
    let shared = std::sync::Arc::new(hl_memory::SharedObjectStore::new(hl_memory::SharedLimits::default()).unwrap());
    let arena = std::sync::Arc::new(
        super::VirtualMemory::reserve(8192)
            .unwrap()
            .with_shared_store(std::sync::Arc::clone(&shared))
            .with_snapshot_backings(),
    );
    let mappings = std::sync::Arc::new(hl_memory::MappingCoordinator::with_shared_space(
        super::MappingHostAdapter::new(std::sync::Arc::clone(&arena)),
        shared,
        hl_memory::AddressSpaceId {
            slot: identity,
            generation: 1,
        },
    ));
    mappings
        .map(hl_memory::MapRequest {
            placement: hl_memory::Placement::Fixed(hl_isa::GuestAddress::new(0)),
            length: 4096,
            alignment: 4096,
            protection: hl_memory::Protection::READ.union(hl_memory::Protection::WRITE),
            backing: hl_memory::Backing::Anonymous {
                identity,
                shared: false,
            },
            backing_offset: 0,
        })
        .unwrap();
    super::space::AddressSpace::new(arena, mappings)
}

fn publish(
    threads: &ThreadSet,
    process: hl_task::ProcessId,
    thread: hl_task::ThreadId,
    pc: u64,
    space: std::sync::Arc<super::space::AddressSpace>,
) {
    let (router, cancellation) = context();
    threads.prepare(thread, process, router, cancellation, space).unwrap();
    threads.stage(thread, snapshot(pc)).unwrap().publish();
}

fn snapshot(pc: u64) -> ExecutionSnapshot {
    let mut cpu = Aarch64CpuState::default();
    cpu.pc = pc;
    ExecutionSnapshot {
        version: EXECUTION_SNAPSHOT_VERSION,
        cpu: ExecutionCpuSnapshot::Aarch64(cpu),
        cache_epoch: 1,
        fault: None,
    }
}

fn syscall_snapshot(pc: u64, number: u64) -> ExecutionSnapshot {
    let mut snapshot = snapshot(pc);
    let ExecutionCpuSnapshot::Aarch64(cpu) = &mut snapshot.cpu else {
        unreachable!()
    };
    cpu.registers[8] = number;
    snapshot
}

fn machine_pc(run: &super::threads::ThreadRun) -> u64 {
    run.machine.freeze().unwrap();
    let snapshot = run.machine.snapshot().unwrap();
    run.machine.thaw().unwrap();
    match snapshot.cpu {
        ExecutionCpuSnapshot::Aarch64(cpu) => cpu.pc,
        ExecutionCpuSnapshot::X86_64(_) => unreachable!(),
    }
}

#[test]
fn image_transaction() {
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let (process, first) = tasks
        .create_init(
            ProcessCredentials::new(0, 0, &[], 32).unwrap(),
            ProcessLimits::default(),
        )
        .unwrap();
    let second = tasks
        .commit_clone_thread(tasks.begin_clone_thread(first).unwrap())
        .unwrap();
    let threads = ThreadSet::new(3).unwrap();
    let old = space();
    publish(&threads, process, first, 0x1000, std::sync::Arc::clone(&old));
    publish(&threads, process, second, 0x2000, old);
    let first_interrupt = threads.find(first).unwrap().interrupt;
    let second_interrupt = threads.find(second).unwrap().interrupt;
    assert!(!std::sync::Arc::ptr_eq(&first_interrupt, &second_interrupt));
    let replacement = space();
    let (router, cancellation) = context();
    let _caller = threads.claim(second, threads.find(second).unwrap().generation).unwrap();
    let peer = threads.claim(first, threads.find(first).unwrap().generation).unwrap();
    let mut image = threads
        .prepare_image(
            second,
            first,
            router,
            cancellation,
            std::sync::Arc::clone(&replacement),
            snapshot(0x3000),
        )
        .unwrap();
    assert_eq!(image.publish(), Err(RuntimeThreadError::Invalid));
    threads.release(&peer).unwrap();
    image.publish().unwrap();
    assert!(std::sync::Arc::ptr_eq(
        &threads.find(first).unwrap().interrupt,
        &second_interrupt,
    ));
    assert_eq!(machine_pc(&threads.find(first).unwrap()), 0x3000);
    assert!(threads.find(second).is_none());
    image.rollback();
    assert!(std::sync::Arc::ptr_eq(
        &threads.find(first).unwrap().interrupt,
        &first_interrupt,
    ));
    assert!(std::sync::Arc::ptr_eq(
        &threads.find(second).unwrap().interrupt,
        &second_interrupt,
    ));
    assert_eq!(machine_pc(&threads.find(first).unwrap()), 0x1000);
    assert!(threads.find(second).is_some());
    image.publish().unwrap();
    image.finish();
    drop(image);
    assert!(std::sync::Arc::ptr_eq(
        &threads.find(first).unwrap().interrupt,
        &second_interrupt,
    ));
    assert!(std::sync::Arc::ptr_eq(
        &threads.find(first).unwrap().space,
        &replacement
    ));
    assert!(threads.find(second).is_none());
}

#[test]
fn interrupt_registration_follows_stage_lifecycle() {
    let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
    let (process, thread) = tasks
        .create_init(ProcessCredentials::new(0, 0, &[], 8).unwrap(), ProcessLimits::default())
        .unwrap();
    let threads = ThreadSet::with_tasks(1, Arc::clone(&tasks)).unwrap();
    let (router, cancellation) = context();
    threads.prepare(thread, process, router, cancellation, space()).unwrap();

    let staged = threads.stage(thread, snapshot(0x1000)).unwrap();
    drop(staged);
    assert!(threads.find(thread).is_none());

    threads.stage(thread, snapshot(0x2000)).unwrap().publish();
    let published = threads.find(thread).unwrap().interrupt;
    assert!(matches!(
        threads.stage(thread, snapshot(0x3000)),
        Err(RuntimeThreadError::Duplicate)
    ));
    assert!(Arc::ptr_eq(&threads.find(thread).unwrap().interrupt, &published));
    threads.terminate(thread).unwrap();
    assert!(threads.find(thread).is_none());
}

#[test]
fn fork_interrupt_activates_at_task_commit() {
    let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
    let (parent, source) = tasks
        .create_init(ProcessCredentials::new(0, 0, &[], 8).unwrap(), ProcessLimits::default())
        .unwrap();
    let plan = tasks.begin_fork_process(source).unwrap();
    let (child, thread) = (plan.process(), plan.thread());
    let threads = ThreadSet::with_tasks(2, Arc::clone(&tasks)).unwrap();
    let (router, cancellation) = context();
    threads.prepare(thread, child, router, cancellation, space()).unwrap();
    let mut staged = threads.stage_fork(&plan, snapshot(0x2000)).unwrap();

    staged.activate_fork(&plan).unwrap();
    Box::new(staged).publish();

    assert!(threads.find(thread).is_some());
    assert!(tasks.snapshot().processes.iter().any(|process| process.id == child));
    assert!(tasks.snapshot().processes.iter().any(|process| process.id == parent));
}

#[test]
fn fork_activation_rejects_mismatched_process() {
    let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
    let (parent, source) = tasks
        .create_init(ProcessCredentials::new(0, 0, &[], 8).unwrap(), ProcessLimits::default())
        .unwrap();
    let plan = tasks.begin_fork_process(source).unwrap();
    let thread = plan.thread();
    let threads = ThreadSet::with_tasks(2, Arc::clone(&tasks)).unwrap();
    let (router, cancellation) = context();
    threads.prepare(thread, parent, router, cancellation, space()).unwrap();
    let mut staged = threads.stage_fork(&plan, snapshot(0x2000)).unwrap();

    assert_eq!(staged.activate_fork(&plan), Err(RuntimeThreadError::Invalid));
    drop(staged);
    threads.discard(thread);
    tasks.rollback_fork_process(plan).unwrap();
}

#[test]
fn deterministic_rotation() {
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let (process, first) = tasks
        .create_init(
            ProcessCredentials::new(0, 0, &[], 32).unwrap(),
            ProcessLimits::default(),
        )
        .unwrap();
    let second = tasks
        .commit_clone_thread(tasks.begin_clone_thread(first).unwrap())
        .unwrap();
    let third = tasks
        .commit_clone_thread(tasks.begin_clone_thread(first).unwrap())
        .unwrap();
    let threads = ThreadSet::new(3).unwrap();
    let space = space();
    publish(&threads, process, first, 0x1000, std::sync::Arc::clone(&space));
    publish(&threads, process, second, 0x2000, std::sync::Arc::clone(&space));
    publish(&threads, process, third, 0x3000, std::sync::Arc::clone(&space));
    let (router, cancellation) = context();
    assert_eq!(
        threads.prepare(first, process, router, cancellation, space),
        Err(RuntimeThreadError::Duplicate)
    );

    let mut selected = Vec::new();
    for _ in 0..4 {
        let run = threads.next().unwrap();
        assert!(std::sync::Arc::ptr_eq(
            &run.interrupt,
            &threads.find(run.thread).unwrap().interrupt,
        ));
        run.machine.freeze().unwrap();
        let pc = match run.machine.snapshot().unwrap().cpu {
            ExecutionCpuSnapshot::Aarch64(cpu) => cpu.pc,
            ExecutionCpuSnapshot::X86_64(_) => unreachable!(),
        };
        run.machine.thaw().unwrap();
        selected.push((run.thread, pc));
        threads.release(&run).unwrap();
    }
    assert_eq!(
        selected,
        [(first, 0x1000), (second, 0x2000), (third, 0x3000), (first, 0x1000),]
    );
    assert!(!threads.is_only_runnable(first));
    let second_run = threads.next().unwrap();
    assert_eq!(second_run.thread, second);
    threads.park(second).unwrap();
    assert!(threads.is_parked(&second_run));
    assert_eq!(threads.release(&second_run), Err(RuntimeThreadError::Missing));
    let third_run = threads.next().unwrap();
    assert_eq!(third_run.thread, third);
    threads.park(third).unwrap();
    assert!(threads.is_only_runnable(first));
    threads.resume(second).unwrap();
    threads.resume(third).unwrap();
    assert!(!threads.is_only_runnable(first));

    threads.terminate(second).unwrap();
    assert_eq!(threads.terminate(second), Err(RuntimeThreadError::Missing));
    let run = threads.next().unwrap();
    assert_eq!(run.thread, first);
    threads.release(&run).unwrap();
    let run = threads.next().unwrap();
    assert_eq!(run.thread, third);
    threads.release(&run).unwrap();
}

#[test]
fn capacity_is_bounded() {
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let (process, first) = tasks
        .create_init(
            ProcessCredentials::new(0, 0, &[], 32).unwrap(),
            ProcessLimits::default(),
        )
        .unwrap();
    let second = tasks
        .commit_clone_thread(tasks.begin_clone_thread(first).unwrap())
        .unwrap();
    let threads = ThreadSet::new(1).unwrap();
    let space = space();
    publish(&threads, process, first, 0x1000, std::sync::Arc::clone(&space));
    let (router, cancellation) = context();
    assert_eq!(
        threads.prepare(second, process, router, cancellation, space),
        Err(RuntimeThreadError::Capacity)
    );
    assert_eq!(ThreadSet::new(0).err(), Some(RuntimeThreadError::Capacity));
}

#[test]
fn context_isolation() {
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let (process, first) = tasks
        .create_init(
            ProcessCredentials::new(0, 0, &[], 32).unwrap(),
            ProcessLimits::default(),
        )
        .unwrap();
    let second = tasks
        .commit_clone_thread(tasks.begin_clone_thread(first).unwrap())
        .unwrap();
    let threads = ThreadSet::new(2).unwrap();
    let space = space();
    publish(&threads, process, first, 0x1000, std::sync::Arc::clone(&space));
    publish(&threads, process, second, 0x2000, space);
    let first_run = threads.next().unwrap();
    let second_run = threads.next().unwrap();
    assert!(std::sync::Arc::ptr_eq(&first_run.space, &second_run.space));
    let mut first_cpu = Aarch64CpuState::default();
    first_cpu.registers[8] = 172;
    let mut first_cpu = ExecutionCpuSnapshot::Aarch64(first_cpu);
    first_run
        .router
        .dispatch(hl_isa::GuestArchitecture::Aarch64, &mut first_cpu);
    let mut second_cpu = Aarch64CpuState::default();
    second_cpu.registers[8] = 178;
    let mut second_cpu = ExecutionCpuSnapshot::Aarch64(second_cpu);
    second_run
        .router
        .dispatch(hl_isa::GuestArchitecture::Aarch64, &mut second_cpu);
    assert_eq!(first_run.router.trace().unwrap()[0].name, "getpid");
    assert_eq!(second_run.router.trace().unwrap()[0].name, "gettid");
    threads.cancel_all(9);
    assert_eq!(first_run.cancellation.signal(), Some(9));
    assert_eq!(second_run.cancellation.signal(), Some(9));
    threads.terminate(first).unwrap();
    assert!(!threads.is_empty());
    threads.terminate_all();
    assert!(!threads.is_empty());
    threads.release(&first_run).unwrap();
    threads.release(&second_run).unwrap();
    assert!(threads.is_empty());
}

#[test]
fn publication_is_local() {
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let (process, first) = tasks
        .create_init(
            ProcessCredentials::new(0, 0, &[], 32).unwrap(),
            ProcessLimits::default(),
        )
        .unwrap();
    let second = tasks
        .commit_clone_thread(tasks.begin_clone_thread(first).unwrap())
        .unwrap();
    let threads = ThreadSet::new(2).unwrap();
    let space = space();
    publish(&threads, process, first, 0x1000, std::sync::Arc::clone(&space));
    publish(&threads, process, second, 0x2000, space);
    let old_first = threads.find(first).unwrap().router;
    let old_second = threads.find(second).unwrap().router;
    let replacement = context().0;
    threads
        .replace_router(first, std::sync::Arc::clone(&replacement))
        .unwrap();
    assert!(std::sync::Arc::ptr_eq(
        &threads.find(first).unwrap().router,
        &replacement,
    ));
    assert!(std::sync::Arc::ptr_eq(
        &threads.find(second).unwrap().router,
        &old_second,
    ));
    assert!(!std::sync::Arc::ptr_eq(&old_first, &replacement));
}

#[test]
fn spaces_isolate_writes() {
    use hl_linux::GuestMemory;
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let (process, first) = tasks
        .create_init(
            ProcessCredentials::new(0, 0, &[], 32).unwrap(),
            ProcessLimits::default(),
        )
        .unwrap();
    let (child_process, child) = tasks
        .commit_fork_process(tasks.begin_fork_process(first).unwrap())
        .unwrap();
    let first_space = mapped_space(1);
    first_space.guest_memory().write(64, b"before").unwrap();
    let child_space = first_space
        .fork_snapshot(hl_memory::AddressSpaceId { slot: 2, generation: 1 })
        .unwrap();
    let threads = ThreadSet::new(2).unwrap();
    publish(&threads, process, first, 0x1000, std::sync::Arc::clone(&first_space));
    publish(
        &threads,
        child_process,
        child,
        0x1000,
        std::sync::Arc::clone(&child_space),
    );
    first_space.guest_memory().write(64, b"parent").unwrap();
    child_space.guest_memory().write(64, b"child!").unwrap();
    let mut parent = [0; 6];
    let mut child = [0; 6];
    first_space.guest_memory().read(64, &mut parent).unwrap();
    child_space.guest_memory().read(64, &mut child).unwrap();
    assert_eq!(&parent, b"parent");
    assert_eq!(&child, b"child!");
    let first_run = threads.next().unwrap();
    let child_run = threads.next().unwrap();
    assert!(!std::sync::Arc::ptr_eq(&first_run.space, &child_run.space));
    threads.terminate_group(&child_run).unwrap();
    threads.release(&first_run).unwrap();
    let first_run = threads.next().unwrap();
    assert_eq!(first_run.thread, first);
    threads.terminate_group(&first_run).unwrap();
    assert!(threads.is_empty());
}

#[test]
fn process_exit_does_not_reclaim_another_process_owner() {
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let (process, first) = tasks
        .create_init(
            ProcessCredentials::new(0, 0, &[], 32).unwrap(),
            ProcessLimits::default(),
        )
        .unwrap();
    let (child_process, child) = tasks
        .commit_fork_process(tasks.begin_fork_process(first).unwrap())
        .unwrap();
    let threads = ThreadSet::new(2).unwrap();
    publish(&threads, process, first, 0x1000, space());
    publish(&threads, child_process, child, 0x2000, space());
    let first_run = threads.claim(first, threads.find(first).unwrap().generation).unwrap();
    let child_run = threads.claim(child, threads.find(child).unwrap().generation).unwrap();

    threads.terminate_group(&child_run).unwrap();
    assert!(threads.next().is_none());
    threads.release(&first_run).unwrap();
    assert_eq!(threads.next().unwrap().thread, first);
}

#[test]
fn terminate_all_defers_running_reclamation() {
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let (process, thread) = tasks
        .create_init(
            ProcessCredentials::new(0, 0, &[], 32).unwrap(),
            ProcessLimits::default(),
        )
        .unwrap();
    let threads = ThreadSet::new(1).unwrap();
    publish(&threads, process, thread, 0x1000, space());
    let run = threads.next().unwrap();

    threads.terminate_all();
    assert!(threads.find(thread).is_some());
    assert!(threads.next().is_none());
    threads.release(&run).unwrap();
    assert!(threads.find(thread).is_none());
}

#[test]
fn group_termination_cancels_only_retired_runs() {
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let (process, first) = tasks
        .create_init(
            ProcessCredentials::new(0, 0, &[], 32).unwrap(),
            ProcessLimits::default(),
        )
        .unwrap();
    let second = tasks
        .commit_clone_thread(tasks.begin_clone_thread(first).unwrap())
        .unwrap();
    let (unrelated_process, unrelated) = tasks
        .commit_fork_process(tasks.begin_fork_process(first).unwrap())
        .unwrap();
    let threads = ThreadSet::new(3).unwrap();
    let group = space();
    publish(&threads, process, first, 0x1000, std::sync::Arc::clone(&group));
    publish(&threads, process, second, 0x2000, group);
    publish(&threads, unrelated_process, unrelated, 0x3000, space());
    let first = threads.claim(first, threads.find(first).unwrap().generation).unwrap();
    let second = threads.find(second).unwrap();
    let unrelated = threads.find(unrelated).unwrap();

    threads.terminate_group(&first).unwrap();

    assert_eq!(first.cancellation.signal(), Some(9));
    assert_eq!(second.cancellation.signal(), Some(9));
    assert_eq!(unrelated.cancellation.signal(), None);
    assert_eq!(threads.next().unwrap().thread, unrelated.thread);
}

#[test]
fn stale_run_cannot_resume_replacement() {
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let (process, thread) = tasks
        .create_init(
            ProcessCredentials::new(0, 0, &[], 32).unwrap(),
            ProcessLimits::default(),
        )
        .unwrap();
    let threads = ThreadSet::new(1).unwrap();
    publish(&threads, process, thread, 0x1000, space());
    let stale = threads.next().unwrap();
    threads.park(thread).unwrap();
    threads.terminate(thread).unwrap();

    publish(&threads, process, thread, 0x2000, space());
    let replacement = threads.next().unwrap();
    assert_ne!(stale.generation, replacement.generation);
    threads.park_syscall(&replacement).unwrap();
    assert_eq!(threads.resume_run(&stale), Err(RuntimeThreadError::Missing));
    assert_eq!(threads.resume_run(&replacement), Ok(()));
}

#[test]
fn process_control_cancels_one_syscall_owned_run() {
    let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
    let (process, first) = tasks
        .create_init(
            ProcessCredentials::new(0, 0, &[], 32).unwrap(),
            ProcessLimits::default(),
        )
        .unwrap();
    let second = tasks
        .commit_clone_thread(tasks.begin_clone_thread(first).unwrap())
        .unwrap();
    let threads = ThreadSet::with_tasks(2, Arc::clone(&tasks)).unwrap();
    publish(&threads, process, first, 0x1000, space());
    publish(&threads, process, second, 0x2000, space());
    let first = threads.next().unwrap();
    let second = threads.find(second).unwrap();
    threads.park_syscall(&first).unwrap();
    assert_eq!(
        tasks
            .snapshot()
            .threads
            .iter()
            .find(|thread| thread.id == first.thread)
            .unwrap()
            .lifecycle,
        hl_task::ThreadLifecycle::Blocked,
    );
    threads.resume_run(&first).unwrap();
    assert_eq!(
        tasks
            .snapshot()
            .threads
            .iter()
            .find(|thread| thread.id == first.thread)
            .unwrap()
            .lifecycle,
        hl_task::ThreadLifecycle::Runnable,
    );
    threads.release(&first).unwrap();
    let first = threads.claim(first.thread, first.generation).unwrap();
    threads.park_syscall(&first).unwrap();
    let second = threads.claim(second.thread, second.generation).unwrap();
    threads.park(second.thread).unwrap();

    assert!(threads.cancel_parked_process(process));
    assert_eq!(first.cancellation.signal(), None);
    assert!(first.cancellation.interruption().take_pending());
    assert_eq!(second.cancellation.signal(), None);
}

#[test]
fn handled_signal_interrupts_parked_syscall() {
    let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
    let (process, thread) = tasks
        .create_init(
            ProcessCredentials::new(0, 0, &[], 32).unwrap(),
            ProcessLimits::default(),
        )
        .unwrap();
    let signal = hl_task::SignalNumber::new(14).unwrap();
    tasks
        .set_action(
            process,
            signal,
            hl_task::SignalAction {
                disposition: hl_task::SignalDisposition::Handler(1),
                flags: 0,
                restorer: 0,
                mask: hl_task::SignalMask::from_bits(0),
            },
        )
        .unwrap();
    let threads = ThreadSet::with_tasks(1, Arc::clone(&tasks)).unwrap();
    publish(&threads, process, thread, 0x1000, space());
    let run = threads.next().unwrap();
    threads.park_syscall(&run).unwrap();
    tasks
        .enqueue_signal(
            hl_task::PendingTarget::Process(process),
            hl_task::SignalInfo::bare(signal),
        )
        .unwrap();

    threads.interrupt_signals();

    assert!(run.cancellation.interruption().take_pending());
}

#[test]
fn run_ownership_transfers_once_and_rejects_stale_claims() {
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let (process, first) = tasks
        .create_init(
            ProcessCredentials::new(0, 0, &[], 32).unwrap(),
            ProcessLimits::default(),
        )
        .unwrap();
    let second = tasks
        .commit_clone_thread(tasks.begin_clone_thread(first).unwrap())
        .unwrap();
    let threads = ThreadSet::new(2).unwrap();
    let shared = space();
    publish(&threads, process, first, 0x1000, Arc::clone(&shared));
    publish(&threads, process, second, 0x2000, shared);

    let running = threads.next().unwrap();
    assert_eq!(running.thread, first);
    assert!(matches!(
        threads.claim(first, running.generation),
        Err(RuntimeThreadError::Missing)
    ));
    assert!(matches!(
        threads.claim(first, running.generation + 1),
        Err(RuntimeThreadError::Missing)
    ));
    threads.release(&running).unwrap();
    assert_eq!(threads.release(&running), Err(RuntimeThreadError::Missing));
    let reclaimed = threads.claim(first, running.generation).unwrap();
    assert!(Arc::ptr_eq(&running.machine, &reclaimed.machine));
    threads.park_syscall(&reclaimed).unwrap();
    assert_eq!(threads.resume_run(&reclaimed), Ok(()));
    assert_eq!(threads.resume_run(&reclaimed), Err(RuntimeThreadError::Missing));
    threads.release(&reclaimed).unwrap();

    let waiting = threads.claim(first, running.generation).unwrap();
    threads.park_syscall(&waiting).unwrap();
    threads.terminate(first).unwrap();
    assert_eq!(threads.resume_run(&waiting), Err(RuntimeThreadError::Missing));

    let running = threads.claim(second, threads.find(second).unwrap().generation).unwrap();
    threads.terminate(second).unwrap();
    assert!(threads.find(second).is_some());
    assert_eq!(threads.release(&running), Ok(()));
    assert_eq!(threads.release(&running), Err(RuntimeThreadError::Missing));
    assert!(threads.find(second).is_none());
    assert!(threads.next().is_none());
}

#[test]
fn failed_turn_returns_exact_running_owner() {
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let (process, thread) = tasks
        .create_init(
            ProcessCredentials::new(0, 0, &[], 32).unwrap(),
            ProcessLimits::default(),
        )
        .unwrap();
    let threads = ThreadSet::new(1).unwrap();
    publish(&threads, process, thread, 0x1000, space());
    let run = threads.next().unwrap();
    let generation = run.generation;

    assert!(super::GuestExecutor::apply_error(&threads, run, crate::engine::EngineError::WaitFailed).is_err());

    let reclaimed = threads.next().unwrap();
    assert_eq!((reclaimed.thread, reclaimed.generation), (thread, generation));
    threads.release(&reclaimed).unwrap();
}

#[test]
fn rejected_waiter_reclaims_only_exact_generation() {
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let (process, thread) = tasks
        .create_init(
            ProcessCredentials::new(0, 0, &[], 32).unwrap(),
            ProcessLimits::default(),
        )
        .unwrap();
    let threads = ThreadSet::new(1).unwrap();
    publish(&threads, process, thread, 0x1000, space());
    let run = threads.next().unwrap();
    threads.park_syscall(&run).unwrap();
    let mut stale = threads.find(thread).unwrap();
    stale.generation = stale.generation.wrapping_add(1);

    assert_eq!(threads.abort_waiter(&stale), Err(RuntimeThreadError::Missing));
    assert!(threads.next().is_none());
    assert_eq!(threads.abort_waiter(&run), Ok(()));
    threads.release(&run).unwrap();
    assert_eq!(threads.next().unwrap().generation, run.generation);
}

#[test]
fn waiter_abort_never_reclaims_running_owner() {
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let (process, thread) = tasks
        .create_init(
            ProcessCredentials::new(0, 0, &[], 32).unwrap(),
            ProcessLimits::default(),
        )
        .unwrap();
    let threads = ThreadSet::new(1).unwrap();
    publish(&threads, process, thread, 0x1000, space());
    let run = threads.next().unwrap();

    assert_eq!(threads.abort_waiter(&run), Err(RuntimeThreadError::Missing));
    assert!(threads.next().is_none());
    threads.release(&run).unwrap();
    assert_eq!(threads.next().unwrap().generation, run.generation);
}

fn turn_plan() -> crate::launch_plan::RuntimeLaunchPlan {
    crate::launch_plan::RuntimeLaunchPlan {
        rootfs: None,
        executable_host: None,
        arguments: Vec::new(),
        environment: Vec::new(),
        result_path: None,
        options: crate::options::Options::default(),
    }
}

#[test]
fn replace_error_releases_running_owner() {
    let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
    let (process, thread) = tasks
        .create_init(
            ProcessCredentials::new(0, 0, &[], 32).unwrap(),
            ProcessLimits::default(),
        )
        .unwrap();
    let threads = ThreadSet::with_tasks(1, Arc::clone(&tasks)).unwrap();
    publish(&threads, process, thread, 0x1000, space());
    let run = threads.next().unwrap();
    let generation = run.generation;
    let waiters = super::waiter::Pool::new(&tasks).unwrap();
    let result = super::GuestExecutor::apply_turn(
        crate::activation::GuestIsa::Aarch64,
        &turn_plan(),
        &threads,
        &waiters,
        super::scheduler::TurnResult {
            run,
            action: super::scheduler::TurnAction::Replace(u64::MAX),
        },
    );
    assert!(result.is_err());
    assert_eq!(threads.next().unwrap().generation, generation);
    waiters.stop();
}

#[test]
fn trace_error_releases_running_owner() {
    let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
    let (process, thread) = tasks
        .create_init(
            ProcessCredentials::new(0, 0, &[], 32).unwrap(),
            ProcessLimits::default(),
        )
        .unwrap();
    let threads = ThreadSet::with_tasks(1, Arc::clone(&tasks)).unwrap();
    publish(&threads, process, thread, 0x1000, space());
    let run = threads.next().unwrap();
    let generation = run.generation;
    run.machine.freeze().unwrap();
    let waiters = super::waiter::Pool::new(&tasks).unwrap();
    let result = super::GuestExecutor::apply_turn(
        crate::activation::GuestIsa::Aarch64,
        &turn_plan(),
        &threads,
        &waiters,
        super::scheduler::TurnResult {
            run,
            action: super::scheduler::TurnAction::Dispatch,
        },
    );
    assert!(result.is_err());
    assert_eq!(threads.next().unwrap().generation, generation);
    waiters.stop();
}

#[test]
fn rejected_waiter_submission_restores_owner() {
    let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
    let (process, thread) = tasks
        .create_init(
            ProcessCredentials::new(0, 0, &[], 32).unwrap(),
            ProcessLimits::default(),
        )
        .unwrap();
    let threads = ThreadSet::with_tasks(1, Arc::clone(&tasks)).unwrap();
    let (router, cancellation) = context_with_trace(false);
    threads.prepare(thread, process, router, cancellation, space()).unwrap();
    threads.stage(thread, syscall_snapshot(0x1000, 63)).unwrap().publish();
    let run = threads.next().unwrap();
    let generation = run.generation;
    let waiters = super::waiter::Pool::new(&tasks).unwrap();
    waiters.reject_next();
    let result = super::GuestExecutor::apply_turn(
        crate::activation::GuestIsa::Aarch64,
        &turn_plan(),
        &threads,
        &waiters,
        super::scheduler::TurnResult {
            run,
            action: super::scheduler::TurnAction::Dispatch,
        },
    );
    assert!(result.is_err());
    assert_eq!(threads.next().unwrap().generation, generation);
    waiters.stop();
}

#[test]
fn acknowledge_error_releases_running_owner() {
    let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
    let (process, first) = tasks
        .create_init(
            ProcessCredentials::new(0, 0, &[], 32).unwrap(),
            ProcessLimits::default(),
        )
        .unwrap();
    let thread = tasks
        .commit_clone_thread(tasks.begin_clone_thread(first).unwrap())
        .unwrap();
    let threads = ThreadSet::with_tasks(1, Arc::clone(&tasks)).unwrap();
    publish(&threads, process, thread, 0x1000, space());
    let run = threads.next().unwrap();
    let generation = run.generation;
    tasks.exit_thread(thread, hl_task::ExitStatus::Code(0)).unwrap();

    assert!(threads.acknowledge_interrupt(thread).is_err());
    assert!(super::GuestExecutor::apply_error(&threads, run, crate::engine::EngineError::WaitFailed).is_err());
    assert_eq!(threads.next().unwrap().generation, generation);
}

fn control_event(
    process: hl_task::ProcessId,
    epoch: u64,
    action: hl_task::ProcessControlAction,
) -> hl_task::SignalActivityEvent {
    hl_task::SignalActivityEvent {
        control_epoch: epoch,
        kind: hl_task::SignalActivityKind::ProcessControl { process, action },
    }
}

#[test]
fn control_event_before_stop_install_bypasses_stale_gate() {
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let (process, thread) = tasks
        .create_init(
            ProcessCredentials::new(0, 0, &[], 32).unwrap(),
            ProcessLimits::default(),
        )
        .unwrap();
    let threads = ThreadSet::new(1).unwrap();
    publish(&threads, process, thread, 0x1000, space());
    let run = threads.next().unwrap();

    threads.process_control(control_event(process, 2, hl_task::ProcessControlAction::Continue));
    assert!(!threads.install_stop_gate(&run, 1).unwrap());
    threads.release(&run).unwrap();

    assert_eq!(threads.next().unwrap().generation, run.generation);
}

#[test]
fn stop_install_before_control_event_releases_exact_gate() {
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let (process, thread) = tasks
        .create_init(
            ProcessCredentials::new(0, 0, &[], 32).unwrap(),
            ProcessLimits::default(),
        )
        .unwrap();
    let threads = ThreadSet::new(1).unwrap();
    publish(&threads, process, thread, 0x1000, space());
    let run = threads.find(thread).unwrap();

    assert!(threads.install_stop_gate(&run, 1).unwrap());
    assert!(threads.next().is_none());
    threads.process_control(control_event(process, 2, hl_task::ProcessControlAction::Kill));

    assert_eq!(threads.next().unwrap().generation, run.generation);
}

#[test]
fn stop_gate_consumes_running_owner_after_peer_quiescence() {
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let (process, first) = tasks
        .create_init(
            ProcessCredentials::new(0, 0, &[], 32).unwrap(),
            ProcessLimits::default(),
        )
        .unwrap();
    let second = tasks
        .commit_clone_thread(tasks.begin_clone_thread(first).unwrap())
        .unwrap();
    let threads = ThreadSet::new(2).unwrap();
    let shared = space();
    publish(&threads, process, first, 0x1000, Arc::clone(&shared));
    publish(&threads, process, second, 0x2000, shared);
    let owner = threads.claim(first, threads.find(first).unwrap().generation).unwrap();
    let peer = threads.claim(second, threads.find(second).unwrap().generation).unwrap();

    assert_eq!(threads.install_stop_gate(&owner, 1), Err(RuntimeThreadError::Invalid));
    threads.release(&peer).unwrap();
    assert!(threads.install_stop_gate(&owner, 1).unwrap());
    assert!(threads.next().is_none());
    threads.process_control(control_event(process, 2, hl_task::ProcessControlAction::Continue));
    assert!(threads.next().is_some());
}

#[test]
fn group_ownership_does_not_follow_space_pointer() {
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let (first_process, first) = tasks
        .create_init(
            ProcessCredentials::new(0, 0, &[], 32).unwrap(),
            ProcessLimits::default(),
        )
        .unwrap();
    let (second_process, second) = tasks
        .commit_fork_process(tasks.begin_fork_process(first).unwrap())
        .unwrap();
    let threads = ThreadSet::new(2).unwrap();
    let shared_pointer = space();
    publish(
        &threads,
        first_process,
        first,
        0x1000,
        std::sync::Arc::clone(&shared_pointer),
    );
    publish(&threads, second_process, second, 0x2000, shared_pointer);

    let first_run = threads.claim(first, threads.find(first).unwrap().generation).unwrap();
    threads.terminate_group(&first_run).unwrap();
    assert!(threads.find(first).is_none());
    assert!(threads.find(second).is_some());
}
