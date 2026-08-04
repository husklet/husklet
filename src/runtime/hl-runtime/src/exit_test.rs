use super::*;

use std::sync::atomic::{AtomicBool, Ordering};

use hl_task::{ProcessCredentials, ProcessLimits, RegistryConfig, TaskRegistry};

struct Role {
    name: &'static str,
    events: Arc<Mutex<Vec<&'static str>>>,
    fail_once: AtomicBool,
}

struct Stage {
    name: &'static str,
    events: Arc<Mutex<Vec<&'static str>>>,
    fail: bool,
    published: bool,
}

struct Finalizer {
    events: Arc<Mutex<Vec<&'static str>>>,
    statuses: Mutex<Vec<ExitStatus>>,
}

struct FailingFinalizer;

impl Role {
    fn stable(events: &Arc<Mutex<Vec<&'static str>>>) -> Arc<dyn ExitParticipant> {
        Arc::new(Self {
            name: "role",
            events: Arc::clone(events),
            fail_once: AtomicBool::new(false),
        })
    }
}

struct PrepareFailure;

impl ExitParticipant for PrepareFailure {
    fn prepare(&self, _: ProcessId, _: &[ThreadId]) -> Result<Box<dyn PreparedExitParticipant>, ExitRuntimeError> {
        Err(ExitRuntimeError::Failed)
    }
}

impl ExitParticipant for Role {
    fn prepare(&self, _: ProcessId, _: &[ThreadId]) -> Result<Box<dyn PreparedExitParticipant>, ExitRuntimeError> {
        self.events.lock().unwrap().push("prepare");
        Ok(Box::new(Stage {
            name: self.name,
            events: self.events.clone(),
            fail: self.fail_once.swap(false, Ordering::AcqRel),
            published: false,
        }))
    }
}

impl PreparedExitParticipant for Stage {
    fn publish(&mut self) -> Result<(), ExitRuntimeError> {
        if self.fail {
            self.events.lock().unwrap().push("failure");
            return Err(ExitRuntimeError::Failed);
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

impl TaskExitFinalizer for FailingFinalizer {
    fn finalize(&self, _: ProcessId, _: &[ThreadId], _: ExitStatus) -> Result<(), ExitRuntimeError> {
        Err(ExitRuntimeError::Failed)
    }
}

impl TaskExitFinalizer for Finalizer {
    fn finalize(&self, _: ProcessId, _: &[ThreadId], status: ExitStatus) -> Result<(), ExitRuntimeError> {
        self.events.lock().unwrap().push("zombie");
        self.statuses.lock().unwrap().push(status);
        Ok(())
    }
}

#[test]
fn failure_stable_status() {
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let credentials = ProcessCredentials::new(1, 1, &[], 4).unwrap();
    let (process, thread) = tasks.create_init(credentials, ProcessLimits::empty()).unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let robust = Arc::new(Role {
        name: "robust",
        events: events.clone(),
        fail_once: AtomicBool::new(false),
    });
    let descriptors = Arc::new(Role {
        name: "descriptors",
        events: events.clone(),
        fail_once: AtomicBool::new(true),
    });
    let ipc = Arc::new(Role {
        name: "ipc",
        events: events.clone(),
        fail_once: AtomicBool::new(false),
    });
    let memory = Arc::new(Role {
        name: "memory",
        events: events.clone(),
        fail_once: AtomicBool::new(false),
    });
    let locks = Arc::new(Role {
        name: "locks",
        events: events.clone(),
        fail_once: AtomicBool::new(false),
    });
    let finalizer = Arc::new(Finalizer {
        events: events.clone(),
        statuses: Mutex::new(Vec::new()),
    });
    let runtime = ExitRuntime::new(robust, descriptors, ipc, memory, locks, finalizer.clone());
    let status = ExitStatus::Signal {
        signal: 11,
        dumped_core: true,
    };
    assert_eq!(runtime.exit(process, &[thread], status), Err(ExitRuntimeError::Failed),);
    assert!(!events.lock().unwrap().contains(&"zombie"));
    runtime.exit(process, &[thread], status).unwrap();
    runtime.exit(process, &[thread], status).unwrap();
    assert_eq!(finalizer.statuses.lock().unwrap().as_slice(), &[status]);
    let events = events.lock().unwrap();
    let zombie = events.iter().position(|event| *event == "zombie").unwrap();
    assert!(events[zombie + 1..].iter().all(|event| *event == "finish"));
}

#[test]
fn finalizer_failure_reverts() {
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let credentials = ProcessCredentials::new(1, 1, &[], 4).unwrap();
    let (process, thread) = tasks.create_init(credentials, ProcessLimits::empty()).unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let role = |name| {
        Arc::new(Role {
            name,
            events: Arc::clone(&events),
            fail_once: AtomicBool::new(false),
        })
    };
    let runtime = ExitRuntime::new(
        role("robust"),
        role("descriptors"),
        role("ipc"),
        role("memory"),
        role("locks"),
        Arc::new(FailingFinalizer),
    );

    assert_eq!(
        runtime.exit(process, &[thread], ExitStatus::Code(7)),
        Err(ExitRuntimeError::Failed),
    );
    let events = events.lock().unwrap();
    assert_eq!(
        &events[events.len() - 5..],
        &["memory", "locks", "ipc", "descriptors", "robust"],
    );
}

#[test]
fn prepare_failure_matrix() {
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let credentials = ProcessCredentials::new(1, 1, &[], 4).unwrap();
    let (process, thread) = tasks.create_init(credentials, ProcessLimits::empty()).unwrap();
    for failure in 0..5 {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut roles: [Arc<dyn ExitParticipant>; 5] = [
            Role::stable(&events),
            Role::stable(&events),
            Role::stable(&events),
            Role::stable(&events),
            Role::stable(&events),
        ];
        roles[failure] = Arc::new(PrepareFailure);
        let finalizer = Arc::new(Finalizer {
            events: Arc::clone(&events),
            statuses: Mutex::new(Vec::new()),
        });
        let runtime = ExitRuntime::new(
            Arc::clone(&roles[0]),
            Arc::clone(&roles[1]),
            Arc::clone(&roles[2]),
            Arc::clone(&roles[3]),
            Arc::clone(&roles[4]),
            finalizer,
        );
        assert_eq!(
            runtime.exit(process, &[thread], ExitStatus::Code(1)),
            Err(ExitRuntimeError::Failed),
        );
        assert!(!events.lock().unwrap().contains(&"zombie"));
    }
}

#[test]
fn publish_failure_matrix() {
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let credentials = ProcessCredentials::new(1, 1, &[], 4).unwrap();
    let (process, thread) = tasks.create_init(credentials, ProcessLimits::empty()).unwrap();
    for failure in 0..5 {
        let events = Arc::new(Mutex::new(Vec::new()));
        let role = |index| {
            Arc::new(Role {
                name: "role",
                events: Arc::clone(&events),
                fail_once: AtomicBool::new(index == failure),
            })
        };
        let finalizer = Arc::new(Finalizer {
            events: Arc::clone(&events),
            statuses: Mutex::new(Vec::new()),
        });
        let runtime = ExitRuntime::new(role(0), role(1), role(2), role(3), role(4), finalizer);
        assert_eq!(
            runtime.exit(process, &[thread], ExitStatus::Code(1)),
            Err(ExitRuntimeError::Failed),
        );
        let events = events.lock().unwrap();
        assert!(events.contains(&"failure"));
        assert!(!events.contains(&"zombie"));
    }
}
