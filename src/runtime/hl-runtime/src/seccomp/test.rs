use std::sync::Mutex;
use std::sync::{Arc, Barrier};
use std::thread;

use hl_linux::{
    BpfInstruction, BpfProgram, GuestAccess, GuestFault, GuestMemory, LinuxResult, SeccompAction, SeccompData,
    SeccompDecision, SeccompPolicy, SeccompSyscalls, SyscallOperation,
};
use hl_task::{ProcessCredentials, ProcessLimits, RegistryConfig, TaskRegistry, ThreadId};

use super::{Control, ControlError, PrctlPort, RuntimeSyscalls};

#[derive(Clone)]
struct Memory(Arc<Mutex<Vec<u8>>>);

impl GuestMemory for Memory {
    fn probe(&self, address: u64, length: usize, access: GuestAccess) -> Result<usize, GuestFault> {
        if address as usize + length > self.0.lock().unwrap().len() {
            Err(GuestFault { address, access })
        } else {
            Ok(length)
        }
    }
    fn read(&self, address: u64, output: &mut [u8]) -> Result<usize, GuestFault> {
        self.probe(address, output.len(), GuestAccess::Read)?;
        output.copy_from_slice(&self.0.lock().unwrap()[address as usize..address as usize + output.len()]);
        Ok(output.len())
    }
    fn write(&self, address: u64, input: &[u8]) -> Result<usize, GuestFault> {
        self.probe(address, input.len(), GuestAccess::Write)?;
        self.0.lock().unwrap()[address as usize..address as usize + input.len()].copy_from_slice(input);
        Ok(input.len())
    }
}

struct Fixture;

impl Fixture {
    fn task_threads(count: usize) -> (TaskRegistry, Vec<ThreadId>) {
        let registry = TaskRegistry::new(RegistryConfig {
            max_processes: 4,
            max_threads: count + 2,
            max_groups: 4,
            max_pending_signals: 8,
            online_cpus: 1,
        })
        .unwrap();
        let credentials = ProcessCredentials::new(0, 0, &[], 4).unwrap();
        let (_, source) = registry.create_init(credentials, ProcessLimits::default()).unwrap();
        let mut threads = vec![source];
        for _ in 1..count {
            let plan = registry.begin_clone_thread(source).unwrap();
            threads.push(registry.commit_clone_thread(plan).unwrap());
        }
        (registry, threads)
    }

    fn program(action: u32) -> BpfProgram {
        BpfProgram::new(vec![BpfInstruction {
            code: 0x06,
            jump_true: 0,
            jump_false: 0,
            value: action,
        }])
        .unwrap()
    }

    fn data() -> SeccompData {
        SeccompData {
            number: 10,
            architecture: 0xc000_003e,
            instruction_pointer: 0x1234,
            arguments: [0; 6],
        }
    }
}

#[test]
fn tsync_thread_atomically() {
    let (_registry, threads) = Fixture::task_threads(3);
    let control = Control::new(4).unwrap();
    for thread in &threads {
        control.register(*thread).unwrap();
        control.lock_privileges(*thread).unwrap();
    }
    let plan = SeccompPolicy::install_plan(Fixture::program(0x0005_0007), 0x01).unwrap();
    let transaction = control.begin_install(threads[0], &threads, plan, false).unwrap();
    for thread in &threads {
        assert_eq!(
            control.evaluate(*thread, Fixture::data()).unwrap(),
            SeccompDecision::Continue,
        );
    }
    control.commit_install(transaction).unwrap();
    for thread in &threads {
        assert_eq!(
            control.evaluate(*thread, Fixture::data()).unwrap(),
            SeccompDecision::ReturnErrno(7),
        );
    }
}

#[test]
fn rollback_policy_unchanged() {
    let (_registry, threads) = Fixture::task_threads(2);
    let control = Control::new(2).unwrap();
    control.register(threads[0]).unwrap();
    control.register(threads[1]).unwrap();
    let plan = SeccompPolicy::install_plan(Fixture::program(0x0005_0001), 0x01).unwrap();
    assert!(matches!(
        control.begin_install(threads[0], &threads, plan.clone(), false),
        Err(ControlError::Policy(hl_linux::SeccompPolicyError::PermissionDenied,)),
    ));
    assert_eq!(
        control.evaluate(threads[0], Fixture::data()).unwrap(),
        SeccompDecision::Continue,
    );
    control.lock_privileges(threads[0]).unwrap();
    let transaction = control.begin_install(threads[0], &threads, plan, false).unwrap();
    control.rollback_install(transaction);
    assert_eq!(
        control.evaluate(threads[1], Fixture::data()).unwrap(),
        SeccompDecision::Continue,
    );
}

#[test]
fn concurrent_partial_commit() {
    let (_registry, threads) = Fixture::task_threads(2);
    let control = Arc::new(Control::new(3).unwrap());
    for thread in &threads {
        control.register(*thread).unwrap();
        control.lock_privileges(*thread).unwrap();
    }
    let plan = SeccompPolicy::install_plan(Fixture::program(0x0005_0002), 0x01).unwrap();
    let transaction = control.begin_install(threads[0], &threads, plan, false).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let worker_control = Arc::clone(&control);
    let worker_barrier = Arc::clone(&barrier);
    let changed = threads[1];
    let worker = thread::spawn(move || {
        worker_barrier.wait();
        worker_control.exec(changed).unwrap();
    });
    barrier.wait();
    worker.join().unwrap();
    assert_eq!(control.commit_install(transaction), Err(ControlError::Conflict),);
    for thread in &threads {
        assert_eq!(
            control.evaluate(*thread, Fixture::data()).unwrap(),
            SeccompDecision::Continue,
        );
    }
}

#[test]
fn fork_preserves_policy() {
    let (_registry, threads) = Fixture::task_threads(2);
    let control = Control::new(2).unwrap();
    control.register(threads[0]).unwrap();
    control.lock_privileges(threads[0]).unwrap();
    let plan = SeccompPolicy::install_plan(Fixture::program(0x0003_0042), 0).unwrap();
    let transaction = control.begin_install(threads[0], &[], plan, false).unwrap();
    control.commit_install(transaction).unwrap();
    control.fork(threads[0], threads[1]).unwrap();
    control.exec(threads[1]).unwrap();
    let decision = control.evaluate(threads[1], Fixture::data()).unwrap();
    assert!(matches!(
        decision,
        SeccompDecision::Trap(plan)
            if plan.error == 0x42
                && plan.signal == 31
                && plan.call_address == 0x1234
    ));
}

#[test]
fn listener_transaction_commits() {
    let (_registry, threads) = Fixture::task_threads(1);
    let control = Control::new(1).unwrap();
    control.register(threads[0]).unwrap();
    control.lock_privileges(threads[0]).unwrap();
    let plan = SeccompPolicy::install_plan(Fixture::program(SeccompAction::Allow { data: 0 }.raw()), 0x08).unwrap();
    let transaction = control.begin_install(threads[0], &[], plan, false).unwrap();
    assert_eq!(transaction.listener().unwrap().owner, threads[0]);
    control.rollback_install(transaction);
    assert_eq!(control.snapshot().policies[0].1.filter_count(), 0);
}

#[test]
fn syscall_filter_installs() {
    let (registry, threads) = Fixture::task_threads(1);
    let registry = Arc::new(registry);
    let process = registry.snapshot().init.unwrap();
    let control = Arc::new(Control::new(1).unwrap());
    control.register(threads[0]).unwrap();
    control.lock_privileges(threads[0]).unwrap();
    let memory = Memory(Arc::new(Mutex::new(vec![0; 128])));
    memory.write(16, &1_u16.to_le_bytes()).unwrap();
    memory.write(24, &64_u64.to_le_bytes()).unwrap();
    memory.write(64, &[0x06, 0, 0, 0, 7, 0, 5, 0]).unwrap();
    let mut syscalls = RuntimeSyscalls::new(Arc::clone(&control), registry, process, threads[0], memory);
    assert_eq!(
        syscalls.handle(
            SyscallOperation {
                canonical_number: 277,
                name: "seccomp",
                family: hl_linux::SyscallFamily::Seccomp,
            },
            [1, 0, 16, 0, 0, 0]
        ),
        LinuxResult::Value(0)
    );
    assert_eq!(
        control.evaluate(threads[0], Fixture::data()).unwrap(),
        SeccompDecision::ReturnErrno(7),
    );
}

#[test]
fn prctl_mode_reports_default_baseline_filter() {
    let (registry, threads) = Fixture::task_threads(1);
    let registry = Arc::new(registry);
    let process = registry.snapshot().init.unwrap();
    let control = Arc::new(Control::new(1).unwrap());
    control.register(threads[0]).unwrap();
    let memory = Memory(Arc::new(Mutex::new(vec![0; 128])));
    let syscalls = RuntimeSyscalls::new(control, registry, process, threads[0], memory);

    assert_eq!(PrctlPort::mode(&syscalls), LinuxResult::Value(2));
}

#[test]
fn disabled_baseline_reports_disabled_without_changing_policy() {
    let (registry, threads) = Fixture::task_threads(1);
    let registry = Arc::new(registry);
    let process = registry.snapshot().init.unwrap();
    let control = Arc::new(Control::new(1).unwrap());
    control.register(threads[0]).unwrap();
    let memory = Memory(Arc::new(Mutex::new(vec![0; 128])));
    let syscalls = RuntimeSyscalls::new(Arc::clone(&control), registry, process, threads[0], memory)
        .with_baseline(hl_linux::SeccompBaseline::Disabled);

    assert_eq!(PrctlPort::mode(&syscalls), LinuxResult::Value(0));
    assert_eq!(
        control.status(threads[0], hl_linux::SeccompBaseline::Disabled).unwrap(),
        hl_linux::SeccompStatus {
            mode: hl_linux::SeccompMode::Disabled,
            filters: 0,
        },
    );
    assert_eq!(control.evaluate(threads[0], Fixture::data()).unwrap(), SeccompDecision::Continue);
    control.enable_strict(threads[0]).unwrap();
    assert_eq!(PrctlPort::mode(&syscalls), LinuxResult::Value(1));
    assert_eq!(
        control
            .status(threads[0], hl_linux::SeccompBaseline::Container)
            .unwrap(),
        hl_linux::SeccompStatus {
            mode: hl_linux::SeccompMode::Strict,
            filters: 0,
        },
    );
}

#[test]
fn user_notification_action_is_available() {
    let (registry, threads) = Fixture::task_threads(1);
    let registry = Arc::new(registry);
    let process = registry.snapshot().init.unwrap();
    let control = Arc::new(Control::new(1).unwrap());
    control.register(threads[0]).unwrap();
    let memory = Memory(Arc::new(Mutex::new(vec![0; 128])));
    memory.write(32, &0x7fc0_0000_u32.to_le_bytes()).unwrap();
    let mut syscalls = RuntimeSyscalls::new(control, registry, process, threads[0], memory.clone());

    assert_eq!(
        SeccompSyscalls::handle(
            &mut syscalls,
            SyscallOperation {
                canonical_number: 277,
                name: "seccomp",
                family: hl_linux::SyscallFamily::Seccomp,
            },
            [2, 0, 32, 0, 0, 0],
        ),
        LinuxResult::Value(0)
    );
    memory.write(32, &0x0005_000d_u32.to_le_bytes()).unwrap();
    assert_eq!(
        SeccompSyscalls::handle(
            &mut syscalls,
            SyscallOperation {
                canonical_number: 277,
                name: "seccomp",
                family: hl_linux::SyscallFamily::Seccomp,
            },
            [2, 0, 32, 0, 0, 0],
        ),
        LinuxResult::Error(hl_linux::Errno::from_raw(95))
    );
}

#[test]
fn copy_precedes_authority() {
    let (registry, threads) = Fixture::task_threads(1);
    let registry = Arc::new(registry);
    let process = registry.snapshot().init.unwrap();
    let control = Arc::new(Control::new(1).unwrap());
    control.register(threads[0]).unwrap();
    let memory = Memory(Arc::new(Mutex::new(vec![0; 32])));
    let mut syscalls = RuntimeSyscalls::new(control, registry, process, threads[0], memory);
    assert_eq!(
        syscalls.handle(
            SyscallOperation {
                canonical_number: 277,
                name: "seccomp",
                family: hl_linux::SyscallFamily::Seccomp,
            },
            [1, 0, 64, 0, 0, 0]
        ),
        LinuxResult::Error(hl_linux::Errno::EFAULT)
    );
}
