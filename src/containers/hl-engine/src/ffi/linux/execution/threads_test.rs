use std::sync::Arc;
use std::time::{Duration, Instant};

use hl_execution::{Aarch64CpuState, EXECUTION_SNAPSHOT_VERSION, ExecutionCpuSnapshot, ExecutionSnapshot};
use hl_runtime::{PreparedThread, RuntimeThreadError, RuntimeThreadPort};
use hl_runtime::{RouterDependencies, RuntimeSyscallRouter, RuntimeSyscallTrap};
use hl_task::{ProcessCredentials, ProcessLimits, RegistryConfig, TaskRegistry};

use super::threads::{ResumeReject, RunOwnership, ThreadSet};

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
    // The rotation is round-robin over the thread-id-ordered set, which is not the
    // order the threads were published in now that init no longer holds slot zero.
    let mut rotation = [(first, 0x1000_u64), (second, 0x2000), (third, 0x3000)];
    rotation.sort_by_key(|(thread, _)| *thread);
    assert_eq!(selected, [rotation[0], rotation[1], rotation[2], rotation[0]]);
    // The remainder walks that same cycle: the loop left `a` released, so the next
    // selections are `b` then `c`.
    let (a, b, c) = (rotation[0].0, rotation[1].0, rotation[2].0);
    assert!(!threads.is_only_runnable(a));
    let b_run = threads.next().unwrap();
    assert_eq!(b_run.thread, b);
    threads.park(b).unwrap();
    assert!(threads.is_parked(&b_run));
    assert_eq!(threads.release(&b_run), Err(RuntimeThreadError::Missing));
    let c_run = threads.next().unwrap();
    assert_eq!(c_run.thread, c);
    threads.park(c).unwrap();
    assert!(threads.is_only_runnable(a));
    threads.resume(b).unwrap();
    threads.resume(c).unwrap();
    assert!(!threads.is_only_runnable(a));

    threads.terminate(b).unwrap();
    assert_eq!(threads.terminate(b), Err(RuntimeThreadError::Missing));
    let run = threads.next().unwrap();
    assert_eq!(run.thread, a);
    threads.release(&run).unwrap();
    let run = threads.next().unwrap();
    assert_eq!(run.thread, c);
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
    assert!(first_run.interrupt.is_set());
    assert!(second_run.interrupt.is_set());
    threads.terminate(first).unwrap();
    assert!(!threads.is_empty());
    threads.terminate_all();
    assert!(!threads.is_empty());
    threads.release(&first_run).unwrap();
    threads.release(&second_run).unwrap();
    assert!(threads.is_empty());
}

#[test]
fn cancellation_before_first_machine_is_inherited() {
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let (process, thread) = tasks
        .create_init(
            ProcessCredentials::new(0, 0, &[], 32).unwrap(),
            ProcessLimits::default(),
        )
        .unwrap();
    let threads = ThreadSet::new(1).unwrap();

    // GuestExecutor publishes the empty ThreadSet before routing and staging
    // the initial machine. A concurrent stop in this window must be durable.
    threads.cancel_all(9);
    publish(&threads, process, thread, 0x1000, space());

    let run = threads.find(thread).unwrap();
    assert_eq!(run.cancellation.signal(), Some(9));
    assert!(run.interrupt.is_set());

    // Cancellation is idempotent and the first terminal signal wins.
    threads.cancel_all(15);
    assert_eq!(run.cancellation.signal(), Some(9));
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
    let mut parent_bytes = [0; 6];
    let mut child_bytes = [0; 6];
    first_space.guest_memory().read(64, &mut parent_bytes).unwrap();
    child_space.guest_memory().read(64, &mut child_bytes).unwrap();
    assert_eq!(&parent_bytes, b"parent");
    assert_eq!(&child_bytes, b"child!");
    // Bind each run to its thread rather than to rotation order, which follows
    // thread identity and so no longer matches the order the two were published.
    let taken = [threads.next().unwrap(), threads.next().unwrap()];
    let first_run = taken.iter().find(|run| run.thread == first).unwrap();
    let child_run = taken.iter().find(|run| run.thread == child).unwrap();
    assert!(!std::sync::Arc::ptr_eq(&first_run.space, &child_run.space));
    threads.terminate_group(child_run).unwrap();
    threads.release(first_run).unwrap();
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
    assert_eq!(threads.resume_run(&stale), Err(ResumeReject::Retired));
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
    // Claim the init thread's run by identity; rotation order follows thread
    // identity and would otherwise hand back the clone under the name `first`.
    let first = threads.claim(first, threads.find(first).unwrap().generation).unwrap();
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
    // Only the syscall-owned run is cancelled: the merely parked one is untouched.
    assert!(!second.cancellation.interruption().take_pending());
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

    // Take the run for `first` by identity; rotation order follows thread identity
    // and hands out the clone before init.
    let running = threads.claim(first, threads.find(first).unwrap().generation).unwrap();
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
    assert_eq!(
        threads.resume_run(&reclaimed),
        Err(ResumeReject::Live(Some(RunOwnership::Running)))
    );
    threads.release(&reclaimed).unwrap();

    let waiting = threads.claim(first, running.generation).unwrap();
    threads.park_syscall(&waiting).unwrap();
    threads.terminate(first).unwrap();
    assert_eq!(threads.resume_run(&waiting), Err(ResumeReject::Retired));

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
    let mut native = super::scheduler::NativePool::new(crate::activation::GuestIsa::Aarch64, &turn_plan(), None);
    let result = super::GuestExecutor::apply_turn(
        crate::activation::GuestIsa::Aarch64,
        &turn_plan(),
        &threads,
        &waiters,
        &mut native,
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
    let mut native = super::scheduler::NativePool::new(crate::activation::GuestIsa::Aarch64, &turn_plan(), None);
    let result = super::GuestExecutor::apply_turn(
        crate::activation::GuestIsa::Aarch64,
        &turn_plan(),
        &threads,
        &waiters,
        &mut native,
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
    let mut native = super::scheduler::NativePool::new(crate::activation::GuestIsa::Aarch64, &turn_plan(), None);
    let result = super::GuestExecutor::apply_turn(
        crate::activation::GuestIsa::Aarch64,
        &turn_plan(),
        &threads,
        &waiters,
        &mut native,
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

/// Terminating a thread that is still executing must interrupt both blocking
/// domains; cancellation alone never reaches translated code.
#[test]
fn terminate_interrupts_a_running_thread() {
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let (process, thread) = tasks
        .create_init(
            ProcessCredentials::new(0, 0, &[], 32).unwrap(),
            ProcessLimits::default(),
        )
        .unwrap();
    let threads = ThreadSet::new(1).unwrap();
    publish(&threads, process, thread, 0x1000, space());
    let run = threads.claim(thread, threads.find(thread).unwrap().generation).unwrap();
    assert!(!run.interrupt.is_set());

    RuntimeThreadPort::terminate(&threads, thread).unwrap();

    assert!(run.interrupt.is_set());
    assert_eq!(run.cancellation.signal(), Some(9));
}

/// A refused completion is only harmless when its thread is gone; one refused
/// for a still-registered generation strands a live thread and must be counted.
#[test]
fn lost_completion_separates_live_thread_from_reclaimed_one() {
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

    let reclaimed = threads.claim(first, threads.find(first).unwrap().generation).unwrap();
    threads.park_syscall(&reclaimed).unwrap();
    threads.terminate(first).unwrap();
    assert_eq!(threads.resume_run(&reclaimed), Err(ResumeReject::Retired));
    assert_eq!(threads.lost_completions(), 0);

    let live = threads.claim(second, threads.find(second).unwrap().generation).unwrap();
    threads.park_syscall(&live).unwrap();
    threads.abort_waiter(&live).unwrap();
    assert_eq!(
        threads.resume_run(&live),
        Err(ResumeReject::Live(Some(RunOwnership::Running)))
    );
    assert_eq!(threads.lost_completions(), 1);
    assert!(threads.find(second).is_some());
}

/// Releasing a stop gate must leave a syscall-owned peer parked: its waiter
/// still owes a completion, and clearing the park strands the run in `Waiter`.
#[test]
fn stop_gate_release_keeps_syscall_owned_peer_resumable() {
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
    let waiting = threads.claim(second, threads.find(second).unwrap().generation).unwrap();
    threads.park_syscall(&waiting).unwrap();
    let owner = threads.claim(first, threads.find(first).unwrap().generation).unwrap();

    assert!(threads.install_stop_gate(&owner, 1).unwrap());
    threads.process_control(control_event(process, 2, hl_task::ProcessControlAction::Continue));

    // The waiter lane now reports the completed syscall for `second`.
    assert_eq!(threads.resume_run(&waiting), Ok(()));
    threads.release(&waiting).unwrap();
    let mut resumed = Vec::new();
    while let Some(run) = threads.next() {
        resumed.push(run.thread);
        if resumed.len() > 2 {
            break;
        }
    }
    assert!(resumed.contains(&second), "syscall-owned peer was never rescheduled");
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

#[test]
fn scheduler_continuation_is_generation_qualified_and_solo() {
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
    let owner = threads.next().unwrap();
    let continuation = threads.continuation(&owner).unwrap();
    assert!(continuation.is_current());

    publish(&threads, process, second, 0x2000, shared);
    assert!(!continuation.is_current());
    assert!(threads.continuation(&owner).is_none());
}

#[test]
fn scheduler_continuation_denies_peer_resume_signal_cancel_control_and_retire() {
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
    let first_run = threads.claim(first, threads.find(first).unwrap().generation).unwrap();
    let _second_run = threads.claim(second, threads.find(second).unwrap().generation).unwrap();
    threads.park(second).unwrap();
    let peer_token = threads.continuation(&first_run).unwrap();
    threads.resume(second).unwrap();
    assert!(!peer_token.is_current());

    threads.terminate(second).unwrap();
    let signal_token = threads.continuation(&first_run).unwrap();
    first_run.interrupt.set(true).unwrap();
    assert!(!signal_token.is_current());
    first_run.interrupt.set(false).unwrap();

    let control_token = threads.continuation(&first_run).unwrap();
    threads.process_control(control_event(process, 1, hl_task::ProcessControlAction::Continue));
    assert!(!control_token.is_current());

    let cancel_token = threads.continuation(&first_run).unwrap();
    threads.cancel_all(9);
    assert!(!cancel_token.is_current());

    let retired = ThreadSet::new(1).unwrap();
    publish(&retired, process, first, 0x3000, space());
    let run = retired.next().unwrap();
    let retire_token = retired.continuation(&run).unwrap();
    retired.terminate(first).unwrap();
    assert!(!retire_token.is_current());
}

#[test]
fn queued_scheduler_transition_denies_continuation_before_taking_set_lock() {
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let (process, first) = tasks
        .create_init(
            ProcessCredentials::new(0, 0, &[], 32).unwrap(),
            ProcessLimits::default(),
        )
        .unwrap();
    let threads = Arc::new(ThreadSet::new(1).unwrap());
    publish(&threads, process, first, 0x1000, space());
    let run = threads.next().unwrap();
    let continuation = threads.continuation(&run).unwrap();
    let contender = Arc::clone(&threads);
    let transition = threads.with_state_lock_for_test(|| {
        let transition = std::thread::spawn(move || contender.release(&run));
        let deadline = Instant::now() + Duration::from_secs(1);
        while continuation.is_current() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(!continuation.is_current());
        transition
    });
    transition.join().unwrap().unwrap();
}

#[test]
fn saturated_scheduler_epoch_permanently_denies_continuation() {
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let (process, first) = tasks
        .create_init(
            ProcessCredentials::new(0, 0, &[], 32).unwrap(),
            ProcessLimits::default(),
        )
        .unwrap();
    let threads = ThreadSet::new(1).unwrap();
    publish(&threads, process, first, 0x1000, space());
    let run = threads.next().unwrap();
    threads.saturate_continuation_for_test();

    assert!(threads.continuation(&run).is_none());
    assert!(threads.continuation(&run).is_none());
}

#[test]
fn scheduler_continuation_rejects_wrong_process_identity() {
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let (process, first) = tasks
        .create_init(
            ProcessCredentials::new(0, 0, &[], 32).unwrap(),
            ProcessLimits::default(),
        )
        .unwrap();
    let (other_process, _) = tasks
        .commit_fork_process(tasks.begin_fork_process(first).unwrap())
        .unwrap();
    let threads = ThreadSet::new(1).unwrap();
    publish(&threads, process, first, 0x1000, space());
    let mut run = threads.next().unwrap();
    run.process = other_process;

    assert!(threads.continuation(&run).is_none());
}

#[test]
fn image_generation_replacement_invalidates_scheduler_continuation() {
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let (process, first) = tasks
        .create_init(
            ProcessCredentials::new(0, 0, &[], 32).unwrap(),
            ProcessLimits::default(),
        )
        .unwrap();
    let threads = ThreadSet::new(2).unwrap();
    publish(&threads, process, first, 0x1000, space());
    let run = threads.next().unwrap();
    let continuation = threads.continuation(&run).unwrap();
    let (router, cancellation) = context();
    let mut image = threads
        .prepare_image(first, first, router, cancellation, space(), snapshot(0x2000))
        .unwrap();

    image.publish().unwrap();
    assert!(!continuation.is_current());
    let replacement = threads.next().unwrap();
    assert_ne!(replacement.generation, run.generation);
    assert!(threads.continuation(&run).is_none());
    image.finish();
}

fn code_exit(status: i32) -> crate::engine::EngineExit {
    crate::engine::EngineExit {
        kind: crate::engine::ExitKind::Code,
        guest_status: status,
        detail: 0,
        fault: None,
    }
}

/// A pid namespace reports pid 1's status, not the last descendant reaped. Under
/// a pids limit init fails while its children live on, so without this the
/// survivors' success would replace init's failure.
#[test]
fn init_exit_becomes_the_session_status() {
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
    threads.terminate_group(&first_run).unwrap();
    threads.note_process_exit(first_run.process, code_exit(2));
    assert!(!threads.is_empty());

    let child_run = threads.claim(child, threads.find(child).unwrap().generation).unwrap();
    threads.terminate_group(&child_run).unwrap();
    threads.note_process_exit(child_run.process, code_exit(0));
    assert!(threads.is_empty());
    assert_eq!(threads.session_exit(code_exit(0)).guest_status, 2);
}

/// Only init decides the session status: a child that exits first must not
/// pre-empt the status init reports afterwards.
#[test]
fn child_exit_does_not_become_the_session_status() {
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

    let child_run = threads.claim(child, threads.find(child).unwrap().generation).unwrap();
    threads.terminate_group(&child_run).unwrap();
    threads.note_process_exit(child_run.process, code_exit(3));

    let first_run = threads.claim(first, threads.find(first).unwrap().generation).unwrap();
    threads.terminate_group(&first_run).unwrap();
    threads.note_process_exit(first_run.process, code_exit(0));
    assert!(threads.is_empty());
    assert_eq!(threads.session_exit(code_exit(7)).guest_status, 0);
}

/// A multithreaded init still reports its own status only once its last thread
/// has left, so an earlier sibling thread's exit cannot claim the session.
#[test]
fn init_status_waits_for_its_last_thread() {
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let (process, first) = tasks
        .create_init(
            ProcessCredentials::new(0, 0, &[], 32).unwrap(),
            ProcessLimits::default(),
        )
        .unwrap();
    let sibling = tasks
        .commit_clone_thread(tasks.begin_clone_thread(first).unwrap())
        .unwrap();
    let threads = ThreadSet::new(2).unwrap();
    publish(&threads, process, first, 0x1000, space());
    publish(&threads, process, sibling, 0x2000, space());

    let sibling_run = threads
        .claim(sibling, threads.find(sibling).unwrap().generation)
        .unwrap();
    threads.terminate_run(&sibling_run).unwrap();
    threads.note_process_exit(sibling_run.process, code_exit(5));
    assert_eq!(threads.session_exit(code_exit(9)).guest_status, 9);

    let first_run = threads.claim(first, threads.find(first).unwrap().generation).unwrap();
    threads.terminate_run(&first_run).unwrap();
    threads.note_process_exit(first_run.process, code_exit(4));
    assert_eq!(threads.session_exit(code_exit(9)).guest_status, 4);
}
