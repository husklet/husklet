use super::*;

use std::sync::atomic::{AtomicBool, Ordering};

struct Cleanup(Mutex<Vec<(hl_task::ThreadId, hl_task::RobustListRegistration)>>);

struct ExitRole {
    name: &'static str,
    events: Arc<Mutex<Vec<&'static str>>>,
    fail: AtomicBool,
}

struct ExitStage {
    name: &'static str,
    events: Arc<Mutex<Vec<&'static str>>>,
    fail: bool,
    published: bool,
}

struct TaskFinalizer {
    tasks: Arc<hl_task::TaskRegistry>,
    events: Arc<Mutex<Vec<&'static str>>>,
}

struct OwnerExit(Mutex<Vec<hl_task::ThreadId>>);

impl hl_task::RobustExitCleanup for Cleanup {
    type Error = ();

    fn cleanup(
        &self,
        _: hl_task::ProcessId,
        thread: hl_task::ThreadId,
        registration: hl_task::RobustListRegistration,
    ) -> Result<(), ()> {
        self.0.lock().unwrap().push((thread, registration));
        Ok(())
    }
}

impl crate::ExitParticipant for ExitRole {
    fn prepare(
        &self,
        _: hl_task::ProcessId,
        _: &[hl_task::ThreadId],
    ) -> Result<Box<dyn crate::PreparedExitParticipant>, crate::ExitRuntimeError> {
        Ok(Box::new(ExitStage {
            name: self.name,
            events: Arc::clone(&self.events),
            fail: self.fail.swap(false, Ordering::AcqRel),
            published: false,
        }))
    }
}

impl crate::PreparedExitParticipant for ExitStage {
    fn publish(&mut self) -> Result<(), crate::ExitRuntimeError> {
        if self.fail {
            return Err(crate::ExitRuntimeError::Failed);
        }
        self.events.lock().unwrap().push(self.name);
        self.published = true;
        Ok(())
    }

    fn rollback(&mut self) {
        if self.published {
            self.events.lock().unwrap().push(self.name);
            self.published = false;
        }
    }

    fn finish(&mut self) {
        self.events.lock().unwrap().push("finish");
    }
}

impl crate::TaskExitFinalizer for TaskFinalizer {
    fn finalize(
        &self,
        process: hl_task::ProcessId,
        _: &[hl_task::ThreadId],
        status: hl_task::ExitStatus,
    ) -> Result<(), crate::ExitRuntimeError> {
        self.events.lock().unwrap().push("zombie");
        self.tasks
            .exit_process(process, status)
            .map_err(|_| crate::ExitRuntimeError::Failed)
    }
}

impl crate::RuntimeFutexPort for OwnerExit {
    fn execute(&self, _: hl_task::ProcessId, _: hl_task::ThreadId, _: hl_linux::FutexPlan) -> LinuxResult {
        LinuxResult::Error(hl_linux::Errno::ENOSYS)
    }

    fn owner_exit(&self, thread: hl_task::ThreadId) {
        self.0.lock().unwrap().push(thread);
    }
}

#[test]
fn round_values_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture, fixture.thread);
        assert_eq!(
            runtime.handle(Fixture::operation("set_robust_list"), [0x8001, 24, 0, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        assert_eq!(
            runtime.handle(Fixture::operation("get_robust_list"), [0, 32, 40, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        assert_eq!(fixture.memory.0.lock().unwrap()[32..40], 0x8001_u64.to_le_bytes());
        assert_eq!(fixture.memory.0.lock().unwrap()[40..48], 24_u64.to_le_bytes());
        assert_eq!(
            runtime.handle(Fixture::operation("set_robust_list"), [0x9000, 23, 0, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EINVAL),
        );
        fixture.memory.put(64, &[0xaa; 8]);
        assert_eq!(
            runtime.handle(Fixture::operation("get_robust_list"), [0, 64, 252, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EFAULT),
        );
        assert_eq!(fixture.memory.0.lock().unwrap()[64..72], [0xaa; 8]);
    }
}

#[test]
fn exit_exactly_once() {
    let fixture = Fixture::new();
    let fork = fixture.tasks.begin_fork_process(fixture.thread).unwrap();
    let child = fork.process();
    let child_thread = fork.thread();
    fixture.tasks.commit_fork_process(fork).unwrap();
    let registration = hl_task::RobustListRegistration::new(0x8000);
    fixture.tasks.set_robust_list(child_thread, registration).unwrap();
    let mut unsupported = RuntimeProcessSyscalls::new(
        fixture.tasks.clone(),
        child,
        child_thread,
        fixture.memory.clone(),
        GuestArchitecture::X86_64,
    );
    assert_eq!(
        unsupported.handle(Fixture::operation("exit"), [0; 6]),
        LinuxResult::Error(hl_linux::Errno::ENOSYS),
    );
    assert_eq!(fixture.tasks.robust_list(child_thread).unwrap(), Some(registration));

    let cleanup = Arc::new(Cleanup(Mutex::new(Vec::new())));
    let mut runtime = RuntimeProcessSyscalls::new(
        fixture.tasks.clone(),
        child,
        child_thread,
        fixture.memory.clone(),
        GuestArchitecture::X86_64,
    )
    .with_robust_exit(cleanup.clone());
    assert_eq!(
        runtime.handle(Fixture::operation("exit"), [0; 6]),
        LinuxResult::Value(0)
    );
    assert_eq!(*cleanup.0.lock().unwrap(), [(child_thread, registration)]);
}

#[test]
fn exit_thread_order() {
    let fixture = Fixture::new();
    let fork = fixture.tasks.begin_fork_process(fixture.thread).unwrap();
    let child = fork.process();
    let leader = fork.thread();
    fixture.tasks.commit_fork_process(fork).unwrap();
    let clone = fixture.tasks.begin_clone_thread(leader).unwrap();
    let worker = clone.thread();
    fixture.tasks.commit_clone_thread(clone).unwrap();
    let leader_registration = hl_task::RobustListRegistration::new(0x1000);
    let worker_registration = hl_task::RobustListRegistration::new(0x2000);
    fixture.tasks.set_robust_list(leader, leader_registration).unwrap();
    fixture.tasks.set_robust_list(worker, worker_registration).unwrap();
    let cleanup = Arc::new(Cleanup(Mutex::new(Vec::new())));
    let mut runtime = RuntimeProcessSyscalls::new(
        fixture.tasks.clone(),
        child,
        leader,
        fixture.memory.clone(),
        GuestArchitecture::Aarch64,
    )
    .with_robust_exit(cleanup.clone());
    assert_eq!(
        runtime.handle(Fixture::operation("exit_group"), [0; 6]),
        LinuxResult::Value(0),
    );
    assert_eq!(
        *cleanup.0.lock().unwrap(),
        [(leader, leader_registration), (worker, worker_registration)],
    );
}

#[test]
fn seccomp_kill_lifecycle() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let fork = fixture.tasks.begin_fork_process(fixture.thread).unwrap();
        let child = fork.process();
        let leader = fork.thread();
        fixture.tasks.commit_fork_process(fork).unwrap();
        let clone = fixture.tasks.begin_clone_thread(leader).unwrap();
        let worker = clone.thread();
        fixture.tasks.commit_clone_thread(clone).unwrap();

        let worker_runtime = fixture.runtime_for(architecture, child, worker);
        worker_runtime
            .terminate_seccomp(hl_linux::SeccompKillScope::Thread, 31)
            .unwrap();
        let live = fixture.tasks.snapshot();
        let process = live.processes.iter().find(|entry| entry.id == child).unwrap();
        assert_eq!(process.threads, [leader]);
        assert_eq!(process.lifecycle, hl_task::ProcessLifecycle::Running);
        assert!(!live.threads.iter().any(|entry| entry.id == worker));

        let leader_runtime = fixture.runtime_for(architecture, child, leader);
        leader_runtime
            .terminate_seccomp(hl_linux::SeccompKillScope::Process, 31)
            .unwrap();
        let dead = fixture.tasks.snapshot();
        let process = dead.processes.iter().find(|entry| entry.id == child).unwrap();
        assert_eq!(process.lifecycle, hl_task::ProcessLifecycle::Zombie);
        assert_eq!(
            process.exit_status,
            Some(hl_task::ExitStatus::Signal {
                signal: 31,
                dumped_core: true
            }),
        );
        assert!(!dead.threads.iter().any(|entry| entry.process == child));
    }
}

#[test]
fn coordinated_exit_order() {
    let fixture = Fixture::new();
    let fork = fixture.tasks.begin_fork_process(fixture.thread).unwrap();
    let child = fork.process();
    let leader = fork.thread();
    fixture.tasks.commit_fork_process(fork).unwrap();
    let clone = fixture.tasks.begin_clone_thread(leader).unwrap();
    let worker = clone.thread();
    fixture.tasks.commit_clone_thread(clone).unwrap();
    let registration = hl_task::RobustListRegistration::new(0x8000);
    fixture.tasks.set_robust_list(leader, registration).unwrap();

    let events = Arc::new(Mutex::new(Vec::new()));
    let role = |name, fail| {
        Arc::new(ExitRole {
            name,
            events: Arc::clone(&events),
            fail: AtomicBool::new(fail),
        })
    };
    let runtime = Arc::new(crate::ExitRuntime::new(
        role("robust", false),
        role("descriptors", false),
        role("ipc", false),
        role("memory", false),
        role("locks", false),
        Arc::new(TaskFinalizer {
            tasks: Arc::clone(&fixture.tasks),
            events: Arc::clone(&events),
        }),
    ));
    let legacy = Arc::new(Cleanup(Mutex::new(Vec::new())));
    let owners = Arc::new(OwnerExit(Mutex::new(Vec::new())));
    let mut syscalls = RuntimeProcessSyscalls::new(
        Arc::clone(&fixture.tasks),
        child,
        leader,
        fixture.memory.clone(),
        GuestArchitecture::X86_64,
    )
    .with_robust_exit(legacy.clone())
    .with_futex_port(owners.clone())
    .with_exit_runtime(runtime);

    assert_eq!(
        syscalls.handle(Fixture::operation("exit_group"), [7, 0, 0, 0, 0, 0]),
        LinuxResult::Value(0),
    );
    assert!(legacy.0.lock().unwrap().is_empty());
    assert_eq!(*owners.0.lock().unwrap(), [leader, worker],);
    assert_eq!(
        &events.lock().unwrap()[..6],
        &["robust", "descriptors", "ipc", "locks", "memory", "zombie"],
    );

    assert_eq!(
        syscalls.handle(Fixture::operation("exit_group"), [7, 0, 0, 0, 0, 0]),
        LinuxResult::Value(0),
    );
    assert_eq!(owners.0.lock().unwrap().len(), 2);
}

#[test]
fn coordinated_failure_reverts() {
    let fixture = Fixture::new();
    let fork = fixture.tasks.begin_fork_process(fixture.thread).unwrap();
    let child = fork.process();
    let leader = fork.thread();
    fixture.tasks.commit_fork_process(fork).unwrap();
    let registration = hl_task::RobustListRegistration::new(0x9000);
    fixture.tasks.set_robust_list(leader, registration).unwrap();

    let events = Arc::new(Mutex::new(Vec::new()));
    let role = |name, fail| {
        Arc::new(ExitRole {
            name,
            events: Arc::clone(&events),
            fail: AtomicBool::new(fail),
        })
    };
    let runtime = Arc::new(crate::ExitRuntime::new(
        role("robust", false),
        role("descriptors", true),
        role("ipc", false),
        role("memory", false),
        role("locks", false),
        Arc::new(TaskFinalizer {
            tasks: Arc::clone(&fixture.tasks),
            events: Arc::clone(&events),
        }),
    ));
    let legacy = Arc::new(Cleanup(Mutex::new(Vec::new())));
    let owners = Arc::new(OwnerExit(Mutex::new(Vec::new())));
    let mut syscalls = RuntimeProcessSyscalls::new(
        Arc::clone(&fixture.tasks),
        child,
        leader,
        fixture.memory.clone(),
        GuestArchitecture::Aarch64,
    )
    .with_robust_exit(legacy.clone())
    .with_futex_port(owners.clone())
    .with_exit_runtime(runtime);

    assert_eq!(
        syscalls.handle(Fixture::operation("exit_group"), [0; 6]),
        LinuxResult::Error(hl_linux::Errno::EINVAL),
    );
    assert_eq!(fixture.tasks.robust_list(leader).unwrap(), Some(registration),);
    assert!(owners.0.lock().unwrap().is_empty());
    assert!(legacy.0.lock().unwrap().is_empty());
    assert_eq!(*events.lock().unwrap(), ["robust", "robust"],);
}
