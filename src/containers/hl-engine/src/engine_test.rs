use super::*;
use crate::options::Options;
use std::sync::{Condvar, Mutex};
use std::thread;

#[derive(Default)]
struct FakeState {
    launched_isa: Option<GuestIsa>,
    launch_entered: bool,
    launch_released: bool,
    launch_fails: bool,
    launch_error: Option<EngineError>,
    wait_calls: usize,
    wait_fails: bool,
    stops: Vec<StopRequest>,
}

#[derive(Default)]
struct FakeLauncher {
    state: Mutex<FakeState>,
    changed: Condvar,
}

impl FakeLauncher {
    fn release_launch(&self) {
        let mut state = self.state.lock().unwrap();
        state.launch_released = true;
        self.changed.notify_all();
    }

    fn wait_until_launch(&self) {
        let mut state = self.state.lock().unwrap();
        while !state.launch_entered {
            state = self.changed.wait(state).unwrap();
        }
    }
}

impl Launcher for Arc<FakeLauncher> {
    fn launch(&self, isa: GuestIsa, _: &RuntimeLaunchPlan, _: WorkspaceId) -> Result<ProcessId, EngineError> {
        let mut state = self.state.lock().unwrap();
        state.launched_isa = Some(isa);
        state.launch_entered = true;
        self.changed.notify_all();
        while !state.launch_released {
            state = self.changed.wait(state).unwrap();
        }
        if let Some(error) = state.launch_error {
            Err(error)
        } else if state.launch_fails {
            Err(EngineError::LaunchFailed)
        } else {
            Ok(ProcessId(41))
        }
    }

    fn wait(&self, _: ProcessId) -> Result<EngineExit, EngineError> {
        let mut state = self.state.lock().unwrap();
        state.wait_calls += 1;
        if state.wait_fails {
            return Err(EngineError::WaitFailed);
        }
        let signal = state.stops.last().map_or(0, |request| request.signal());
        Ok(if signal == 0 {
            EngineExit {
                kind: ExitKind::Code,
                guest_status: 23,
                detail: 0,
                fault: None,
            }
        } else {
            EngineExit {
                kind: ExitKind::Signal,
                guest_status: signal,
                detail: 0,
                fault: None,
            }
        })
    }

    fn terminate(&self, _: ProcessId, request: StopRequest) -> Result<(), EngineError> {
        self.state.lock().unwrap().stops.push(request);
        Ok(())
    }
}

#[test]
fn launch_error_preserved() {
    let (engine, launcher, _) = Fixture::engine(GuestIsa::X86_64);
    let error = EngineError::Load(hl_loader::LoadError::InvalidInterpreter);
    {
        let mut state = launcher.state.lock().unwrap();
        state.launch_released = true;
        state.launch_error = Some(error);
    }
    assert_eq!(engine.start(), Err(error));
}

#[derive(Default)]
struct FakeWorkspace {
    prepared: Mutex<usize>,
    cleaned: Mutex<usize>,
}

impl Workspace for Arc<FakeWorkspace> {
    fn prepare(&self) -> Result<WorkspaceId, EngineError> {
        *self.prepared.lock().unwrap() += 1;
        Ok(WorkspaceId(7))
    }

    fn cleanup(&self, _: WorkspaceId) -> Result<(), EngineError> {
        *self.cleaned.lock().unwrap() += 1;
        Ok(())
    }
}

fn plan() -> RuntimeLaunchPlan {
    RuntimeLaunchPlan {
        rootfs: None,
        executable_host: None,
        arguments: vec![b"guest".to_vec()],
        environment: Vec::new(),
        result_path: None,
        options: Options::default(),
    }
}

struct Fixture;

impl Fixture {
    fn engine(
        isa: GuestIsa,
    ) -> (
        Engine<Arc<FakeLauncher>, Arc<FakeWorkspace>>,
        Arc<FakeLauncher>,
        Arc<FakeWorkspace>,
    ) {
        let launcher = Arc::new(FakeLauncher::default());
        let workspaces = Arc::new(FakeWorkspace::default());
        (
            Engine::new(isa, plan(), Arc::clone(&launcher), Arc::clone(&workspaces)),
            launcher,
            workspaces,
        )
    }
}

#[test]
fn both_isas_are() {
    for isa in [GuestIsa::Aarch64, GuestIsa::X86_64] {
        let (engine, launcher, _) = Fixture::engine(isa);
        launcher.release_launch();
        engine.start().unwrap();
        assert_eq!(launcher.state.lock().unwrap().launched_isa, Some(isa));
    }
}

#[test]
fn stop_during_start() {
    let (engine, launcher, workspaces) = Fixture::engine(GuestIsa::Aarch64);
    let runner = {
        let engine = engine.clone();
        thread::spawn(move || engine.start())
    };
    launcher.wait_until_launch();
    engine.terminate(StopRequest::Force).unwrap();
    launcher.release_launch();
    runner.join().unwrap().unwrap();
    assert_eq!(engine.phase().unwrap(), EnginePhase::Stopping);
    assert_eq!(engine.wait().unwrap().guest_status, 9);
    assert_eq!(*workspaces.cleaned.lock().unwrap(), 1);
}

#[test]
fn wait_is_idempotent() {
    let (engine, launcher, workspaces) = Fixture::engine(GuestIsa::X86_64);
    launcher.release_launch();
    engine.start().unwrap();
    let first = engine.wait().unwrap();
    let second = engine.wait().unwrap();
    assert_eq!(first, second);
    assert_eq!(launcher.state.lock().unwrap().wait_calls, 1);
    assert_eq!(*workspaces.cleaned.lock().unwrap(), 1);
}

#[test]
fn pre_start_stop() {
    let (engine, launcher, _) = Fixture::engine(GuestIsa::Aarch64);
    engine.terminate(StopRequest::Interrupt).unwrap();
    engine.destroy().unwrap();
    assert_eq!(engine.phase().unwrap(), EnginePhase::Destroyed);
    assert_eq!(launcher.state.lock().unwrap().launched_isa, None);
}

#[test]
fn launch_failure_cleans() {
    let (engine, launcher, workspaces) = Fixture::engine(GuestIsa::Aarch64);
    {
        let mut state = launcher.state.lock().unwrap();
        state.launch_fails = true;
        state.launch_released = true;
    }
    assert_eq!(engine.start(), Err(EngineError::LaunchFailed));
    assert_eq!(engine.phase().unwrap(), EnginePhase::Exited);
    assert_eq!(*workspaces.cleaned.lock().unwrap(), 1);
}

#[test]
fn concurrent_waiters_share() {
    let (engine, launcher, _) = Fixture::engine(GuestIsa::X86_64);
    launcher.release_launch();
    engine.start().unwrap();
    let first = {
        let engine = engine.clone();
        thread::spawn(move || engine.wait())
    };
    let second = {
        let engine = engine.clone();
        thread::spawn(move || engine.wait())
    };
    assert_eq!(first.join().unwrap(), second.join().unwrap());
    assert_eq!(launcher.state.lock().unwrap().wait_calls, 1);
}

#[test]
fn wait_failure_is() {
    let (engine, launcher, _) = Fixture::engine(GuestIsa::Aarch64);
    {
        let mut state = launcher.state.lock().unwrap();
        state.launch_released = true;
        state.wait_fails = true;
    }
    engine.start().unwrap();
    assert_eq!(engine.wait(), Err(EngineError::WaitFailed));
    assert_eq!(engine.wait(), Err(EngineError::WaitFailed));
    assert_eq!(launcher.state.lock().unwrap().wait_calls, 1);
}

#[test]
fn destroy_during_start() {
    let (engine, launcher, workspaces) = Fixture::engine(GuestIsa::Aarch64);
    let runner = {
        let engine = engine.clone();
        thread::spawn(move || engine.start())
    };
    launcher.wait_until_launch();
    let destroyer = {
        let engine = engine.clone();
        thread::spawn(move || engine.destroy())
    };
    launcher.release_launch();
    runner.join().unwrap().unwrap();
    let exit = destroyer.join().unwrap().unwrap().unwrap();
    assert_eq!(exit.guest_status, 9);
    assert_eq!(engine.phase().unwrap(), EnginePhase::Destroyed);
    assert_eq!(*workspaces.cleaned.lock().unwrap(), 1);
}

#[test]
fn destroy_during_failed() {
    let (engine, launcher, workspaces) = Fixture::engine(GuestIsa::X86_64);
    launcher.state.lock().unwrap().launch_fails = true;
    let runner = {
        let engine = engine.clone();
        thread::spawn(move || engine.start())
    };
    launcher.wait_until_launch();
    let destroyer = {
        let engine = engine.clone();
        thread::spawn(move || engine.destroy())
    };
    while engine.phase().unwrap() != EnginePhase::Stopping {
        thread::yield_now();
    }
    launcher.release_launch();
    assert_eq!(runner.join().unwrap(), Err(EngineError::LaunchFailed));
    assert_eq!(destroyer.join().unwrap(), Err(EngineError::LaunchFailed));
    assert_eq!(engine.phase().unwrap(), EnginePhase::Destroyed);
    assert_eq!(*workspaces.cleaned.lock().unwrap(), 1);
}
