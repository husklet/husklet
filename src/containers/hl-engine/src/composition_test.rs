use super::*;
use crate::engine::{ExitKind, WorkspaceId};
use crate::options::Options;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::thread;

#[derive(Default)]
struct Channel {
    messages: Mutex<Vec<Vec<u8>>>,
}

impl ActivationChannel for Channel {
    fn send(&self, message: &[u8]) -> Result<(), CompositionError> {
        self.messages.lock().unwrap().push(message.to_vec());
        Ok(())
    }

    fn receive(&self, maximum: usize) -> Result<Vec<u8>, CompositionError> {
        Ok(self
            .messages
            .lock()
            .unwrap()
            .last()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .take(maximum)
            .collect())
    }
}

#[derive(Default)]
struct Checkpoints {
    image: Mutex<Vec<u8>>,
}

impl CheckpointSink for Checkpoints {
    fn replace(&self, image: &[u8]) -> Result<(), CompositionError> {
        *self.image.lock().unwrap() = image.to_vec();
        Ok(())
    }
}

impl CheckpointSource for Checkpoints {
    fn read(&self, maximum: usize) -> Result<Vec<u8>, CompositionError> {
        Ok(self.image.lock().unwrap().iter().copied().take(maximum).collect())
    }
}

#[derive(Default)]
struct MachineState {
    started: bool,
    released: bool,
    stops: Vec<StopRequest>,
}

#[derive(Clone, Default)]
struct Machine {
    state: Arc<(Mutex<MachineState>, Condvar)>,
}

impl Machine {
    fn release(&self) {
        let (state, changed) = &*self.state;
        state.lock().unwrap().released = true;
        changed.notify_all();
    }
}

impl GuestMachine for Machine {
    fn start(&self) -> Result<(), EngineError> {
        let (state, changed) = &*self.state;
        let mut state = state.lock().unwrap();
        state.started = true;
        changed.notify_all();
        while !state.released {
            state = changed.wait(state).unwrap();
        }
        Ok(())
    }

    fn wait(&self) -> Result<EngineExit, EngineError> {
        let state = self.state.0.lock().unwrap();
        let signal = state.stops.last().map_or(0, |request| request.signal());
        Ok(EngineExit {
            kind: if signal == 0 { ExitKind::Code } else { ExitKind::Signal },
            guest_status: signal,
            detail: 0,
            fault: None,
        })
    }

    fn stop(&self, request: StopRequest) -> Result<(), EngineError> {
        let (state, changed) = &*self.state;
        let mut state = state.lock().unwrap();
        state.stops.push(request);
        state.released = true;
        changed.notify_all();
        Ok(())
    }
}

#[derive(Default)]
struct FactoryState {
    observed: Vec<(GuestIsa, Vec<Vec<u8>>)>,
    events: Vec<&'static str>,
    fail_at: Option<usize>,
}

impl FactoryState {
    fn dropped(domain: &str) -> &'static str {
        match domain {
            "memory" => "drop-memory",
            "descriptors" => "drop-descriptors",
            _ => "drop-tasks",
        }
    }

    fn construct_domains(&mut self) -> Result<(), CompositionError> {
        let domains = ["memory", "descriptors", "tasks"];
        for (index, domain) in domains.into_iter().enumerate() {
            self.events.push(domain);
            if self.fail_at == Some(index) {
                self.rollback(&domains[..=index]);
                return Err(CompositionError::RuntimeConstruction);
            }
        }
        Ok(())
    }

    fn rollback(&mut self, domains: &[&'static str]) {
        for domain in domains.iter().rev() {
            self.events.push(Self::dropped(domain));
        }
    }
}

struct Factory {
    state: Mutex<FactoryState>,
    machine: Machine,
}

impl Factory {
    fn new(machine: Machine) -> Self {
        Self {
            state: Mutex::new(FactoryState::default()),
            machine,
        }
    }
}

impl RuntimeFactory for Factory {
    type Machine = Machine;

    fn construct(&self, request: RuntimeConstruction<'_>) -> Result<Self::Machine, CompositionError> {
        let mut state = self.state.lock().unwrap();
        state.observed.push((request.isa, request.plan.arguments.clone()));
        state.construct_domains()?;
        drop(state);
        request.services.activation.send(b"constructed")?;
        if let Some(sink) = &request.services.checkpoint_sink {
            sink.replace(b"checkpoint")?;
        }
        if let Some(source) = &request.services.checkpoint_source {
            let _ = source.read(1024)?;
        }
        Ok(self.machine.clone())
    }
}

#[derive(Clone, Copy)]
struct WorkspacePort;

impl Workspace for WorkspacePort {
    fn prepare(&self) -> Result<WorkspaceId, EngineError> {
        Ok(WorkspaceId(1))
    }

    fn cleanup(&self, _: WorkspaceId) -> Result<(), EngineError> {
        Ok(())
    }
}

struct Fixture;

impl Fixture {
    fn plan(arguments: &[&[u8]]) -> RuntimeLaunchPlan {
        RuntimeLaunchPlan {
            rootfs: None,
            executable_host: None,
            arguments: arguments.iter().map(|argument| argument.to_vec()).collect(),
            environment: Vec::new(),
            result_path: None,
            options: Options::default(),
        }
    }

    fn services() -> (RuntimeServices, Arc<Channel>, Arc<Checkpoints>) {
        let activation = Arc::new(Channel::default());
        let checkpoints = Arc::new(Checkpoints::default());
        (
            RuntimeServices {
                activation: activation.clone(),
                executable_authority: None,
                checkpoint_sink: Some(checkpoints.clone()),
                checkpoint_source: Some(checkpoints.clone()),
                streams: StandardStreams::default(),
            },
            activation,
            checkpoints,
        )
    }
}

#[test]
fn validates_required_checkpoint() {
    let machine = Machine::default();
    let factory = Factory::new(machine);
    let mut plan = Fixture::plan(&[b"guest"]);
    plan.options.set("HL_CHECKPOINT", "1", true).unwrap();
    let services = RuntimeServices {
        activation: Arc::new(Channel::default()),
        executable_authority: None,
        checkpoint_sink: None,
        checkpoint_source: None,
        streams: StandardStreams::default(),
    };
    let result = EngineBackend::construct(GuestIsa::Aarch64, plan, services, &factory, WorkspacePort);
    assert!(matches!(result, Err(CompositionError::MissingCheckpointSink)));
    assert!(factory.state.lock().unwrap().observed.is_empty());
}

#[test]
fn factory_failure_rolls() {
    let factory = Factory::new(Machine::default());
    factory.state.lock().unwrap().fail_at = Some(2);
    let (services, _, _) = Fixture::services();
    let result = EngineBackend::construct(
        GuestIsa::X86_64,
        Fixture::plan(&[b"guest"]),
        services,
        &factory,
        WorkspacePort,
    );
    assert!(matches!(result, Err(CompositionError::RuntimeConstruction)));
    assert_eq!(
        factory.state.lock().unwrap().events,
        [
            "memory",
            "descriptors",
            "tasks",
            "drop-tasks",
            "drop-descriptors",
            "drop-memory"
        ]
    );
}

#[test]
fn dual_engines_keep() {
    let arm_machine = Machine::default();
    let x86_machine = Machine::default();
    let arm_factory = Factory::new(arm_machine.clone());
    let x86_factory = Factory::new(x86_machine.clone());
    let (arm_services, _, _) = Fixture::services();
    let (x86_services, _, _) = Fixture::services();
    let arm = EngineBackend::construct(
        GuestIsa::Aarch64,
        Fixture::plan(&[b"arm"]),
        arm_services,
        &arm_factory,
        WorkspacePort,
    )
    .unwrap();
    let x86 = EngineBackend::construct(
        GuestIsa::X86_64,
        Fixture::plan(&[b"x86"]),
        x86_services,
        &x86_factory,
        WorkspacePort,
    )
    .unwrap();
    arm_machine.release();
    x86_machine.release();
    arm.start().unwrap();
    x86.start().unwrap();
    arm.stop(StopRequest::Interrupt).unwrap();
    assert_eq!(arm.wait().unwrap().guest_status, 2);
    assert_eq!(x86.wait().unwrap().guest_status, 0);
    assert_eq!(arm_factory.state.lock().unwrap().observed[0].1, [b"arm".to_vec()]);
    assert_eq!(x86_factory.state.lock().unwrap().observed[0].1, [b"x86".to_vec()]);
}

#[test]
fn concurrent_start_stop() {
    let machine = Machine::default();
    let factory = Factory::new(machine.clone());
    let (services, activation, checkpoints) = Fixture::services();
    let backend = Arc::new(
        EngineBackend::construct(
            GuestIsa::Aarch64,
            Fixture::plan(&[b"guest", b"--flag"]),
            services,
            &factory,
            WorkspacePort,
        )
        .unwrap(),
    );
    assert_eq!(
        activation.messages.lock().unwrap().as_slice(),
        [b"constructed".to_vec()]
    );
    assert_eq!(checkpoints.image.lock().unwrap().as_slice(), b"checkpoint");
    let starter = {
        let backend = Arc::clone(&backend);
        thread::spawn(move || backend.start())
    };
    while !machine.state.0.lock().unwrap().started {
        thread::yield_now();
    }
    backend.stop(StopRequest::Force).unwrap();
    starter.join().unwrap().unwrap();
    assert_eq!(backend.wait().unwrap().guest_status, 9);
}

struct FaultMachine {
    panic: bool,
    waits: AtomicUsize,
}

impl GuestMachine for FaultMachine {
    fn start(&self) -> Result<(), EngineError> {
        assert!(!self.panic, "worker panic probe");
        Err(EngineError::LaunchFailed)
    }

    fn wait(&self) -> Result<EngineExit, EngineError> {
        self.waits.fetch_add(1, Ordering::AcqRel);
        unreachable!("failed start must not publish a machine wait")
    }

    fn stop(&self, _: StopRequest) -> Result<(), EngineError> {
        Ok(())
    }
}

#[test]
fn worker_failure_joins() {
    for (panic, expected) in [(false, EngineError::LaunchFailed), (true, EngineError::WaitFailed)] {
        let machine = Arc::new(FaultMachine {
            panic,
            waits: AtomicUsize::new(0),
        });
        let launcher = MachineLauncher {
            machine: Arc::clone(&machine),
            worker: Mutex::new(None),
        };
        launcher
            .launch(GuestIsa::Aarch64, &Fixture::plan(&[]), WorkspaceId(1))
            .unwrap();
        assert_eq!(launcher.wait(ProcessId(1)), Err(expected));
        assert_eq!(launcher.wait(ProcessId(1)), Err(EngineError::Busy));
        assert_eq!(machine.waits.load(Ordering::Acquire), 0);
    }
}

#[test]
fn launcher_drop_joins() {
    let machine = Arc::new(Machine::default());
    let launcher = MachineLauncher {
        machine: Arc::clone(&machine),
        worker: Mutex::new(None),
    };
    launcher
        .launch(GuestIsa::Aarch64, &Fixture::plan(&[]), WorkspaceId(1))
        .unwrap();
    while !machine.state.0.lock().unwrap().started {
        thread::yield_now();
    }
    drop(launcher);
    assert_eq!(machine.state.0.lock().unwrap().stops, [StopRequest::Force]);
}
