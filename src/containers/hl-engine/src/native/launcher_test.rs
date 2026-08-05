use super::*;
use crate::activation::{ActivationDescriptor, ActivationStreams};
use crate::launch_plan::{ConfigOrigin, DiagnosticsMode};
use crate::native_host::{AuthorityChannel, HostError, ProcessId, ProcessSyscalls};

struct Authority;

impl AuthorityAccess for Authority {
    fn open(&self, domain: [u64; 2]) -> Result<AuthorityChannel, EngineError> {
        assert_eq!(domain, [1, 0]);
        AuthorityChannel::new(
            HostDescriptor::new(12).unwrap(),
            HostDescriptor::new(13).unwrap(),
            HostDescriptor::new(14).unwrap(),
            [3, 4],
            4096,
            8,
        )
    }
}

#[derive(Default)]
struct Files {
    created: Mutex<Vec<(PathBuf, Vec<u8>)>>,
    removed: Mutex<Vec<PathBuf>>,
}

impl WorkspaceFiles for Files {
    fn create(&self, path: &Path, wire: &[u8]) -> Result<(), EngineError> {
        self.created.lock().unwrap().push((path.to_owned(), wire.to_vec()));
        Ok(())
    }
    fn remove(&self, path: &Path) -> Result<(), EngineError> {
        self.removed.lock().unwrap().push(path.to_owned());
        Ok(())
    }
}

#[derive(Default)]
struct Processes {
    requests: Mutex<Vec<SpawnRequest>>,
    closed: Mutex<Vec<u64>>,
    signals: Mutex<Vec<ProcessSignal>>,
    block_wait: std::sync::atomic::AtomicBool,
    wait_started: (Mutex<bool>, std::sync::Condvar),
    wait_release: (Mutex<bool>, std::sync::Condvar),
}

impl ProcessSyscalls for Processes {
    fn spawn(&self, request: &SpawnRequest) -> Result<(ProcessId, u64), HostError> {
        self.requests.lock().unwrap().push(request.clone());
        Ok((ProcessId::new(31)?, 44))
    }
    fn close_process(&self, token: u64) {
        self.closed.lock().unwrap().push(token);
    }
    fn wait(&self, _: ProcessId) -> Result<Option<ChildExit>, HostError> {
        Ok(Some(ChildExit::Code(0)))
    }
    fn wait_blocking(&self, _: ProcessId) -> Result<ChildExit, HostError> {
        if self.block_wait.load(std::sync::atomic::Ordering::SeqCst) {
            *self.wait_started.0.lock().unwrap() = true;
            self.wait_started.1.notify_all();
            let mut released = self.wait_release.0.lock().unwrap();
            while !*released {
                released = self.wait_release.1.wait(released).unwrap();
            }
        }
        Ok(ChildExit::Code(23))
    }
    fn signal(&self, _: ProcessId, _: ProcessSignal) -> Result<(), HostError> {
        Ok(())
    }
    fn signal_group(&self, _: ProcessId, signal: ProcessSignal) -> Result<(), HostError> {
        self.signals.lock().unwrap().push(signal);
        Ok(())
    }
}

#[test]
fn terminate_remains_available_while_wait_reaps() {
    let files = Arc::new(Files::default());
    let workspace = ProcessWorkspace::new(PathBuf::from("/staging"), Arc::clone(&files)).unwrap();
    let processes = Arc::new(Processes::default());
    processes.block_wait.store(true, std::sync::atomic::Ordering::SeqCst);
    let launcher = Arc::new(
        workspace
            .launcher(PathBuf::from("/engine"), Arc::clone(&processes))
            .unwrap(),
    );
    let workspace_id = workspace.prepare_material(&material()).unwrap();
    let process = launcher
        .launch_material(GuestIsa::Aarch64, &material(), workspace_id)
        .unwrap();
    let waiter = {
        let launcher = Arc::clone(&launcher);
        std::thread::spawn(move || launcher.wait(process))
    };
    let mut started = processes.wait_started.0.lock().unwrap();
    while !*started {
        started = processes.wait_started.1.wait(started).unwrap();
    }
    drop(started);

    launcher.terminate(process, StopRequest::Force).unwrap();
    *processes.wait_release.0.lock().unwrap() = true;
    processes.wait_release.1.notify_all();

    assert_eq!(waiter.join().unwrap().unwrap().guest_status, 23);
    assert_eq!(*processes.signals.lock().unwrap(), [ProcessSignal::Kill]);
}

fn material() -> LaunchMaterial {
    let mut wire = vec![0; 192 + 8];
    for (offset, value) in [(0, 0x484c_4346_u32), (4, 8), (8, 192), (12, 1), (108, 1)] {
        wire[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    wire[152..160].copy_from_slice(&1_u64.to_le_bytes());
    wire[193..200].copy_from_slice(b"guest\0\0");
    LaunchMaterial::from_validated_wire(
        &wire,
        ConfigOrigin::File(b"/original/launch.bin".to_vec()),
        ActivationStreams {
            input: ActivationDescriptor::new(8).unwrap(),
            output: ActivationDescriptor::new(9).unwrap(),
            error: ActivationDescriptor::INHERIT,
        },
        Some(10),
        DiagnosticsMode::Disabled,
    )
    .unwrap()
}

fn sandbox_material() -> LaunchMaterial {
    let mut material = material();
    material.sandbox = crate::launch_plan::SandboxMode::Confined;
    material
}

#[test]
fn material_wire_spawn() {
    let files = Arc::new(Files::default());
    let workspace = ProcessWorkspace::new(PathBuf::from("/staging"), Arc::clone(&files)).unwrap();
    let processes = Arc::new(Processes::default());
    let launcher = workspace
        .launcher(PathBuf::from("/engine"), Arc::clone(&processes))
        .unwrap();
    let material = material();
    let workspace_id = workspace.prepare_material(&material).unwrap();
    assert_eq!(files.created.lock().unwrap()[0].1, material.wire);
    let process = launcher
        .launch_material(GuestIsa::Aarch64, &material, workspace_id)
        .unwrap();
    let request = processes.requests.lock().unwrap()[0].clone();
    assert_eq!(request.program.as_bytes(), b"/engine/hl-aarch64");
    assert_eq!(request.process_group, ProcessGroup::New);
    assert_eq!(request.file_actions.len(), 3);
    assert_eq!(launcher.wait(process).unwrap().guest_status, 23);
    workspace.cleanup(workspace_id).unwrap();
    assert_eq!(files.removed.lock().unwrap().len(), 1);
    assert_eq!(workspace.cleanup(workspace_id), Err(EngineError::WorkspaceFailed));
}

#[test]
fn authority_missing() {
    let files = Arc::new(Files::default());
    let workspace = ProcessWorkspace::new(PathBuf::from("/staging"), Arc::clone(&files)).unwrap();
    let processes = Arc::new(Processes::default());
    let launcher = workspace
        .launcher(PathBuf::from("/engine"), Arc::clone(&processes))
        .unwrap();
    let material = sandbox_material();
    let workspace_id = workspace.prepare_material(&material).unwrap();
    assert_eq!(
        launcher.launch_material(GuestIsa::Aarch64, &material, workspace_id),
        Err(EngineError::AuthorityFailed)
    );
    assert!(processes.requests.lock().unwrap().is_empty());
}

#[test]
fn channel_inheritance() {
    let files = Arc::new(Files::default());
    let workspace = ProcessWorkspace::new(PathBuf::from("/staging"), Arc::clone(&files)).unwrap();
    let processes = Arc::new(Processes::default());
    let launcher = workspace
        .launcher_authorized(PathBuf::from("/engine"), Arc::clone(&processes), Arc::new(Authority))
        .unwrap();
    let material = sandbox_material();
    let workspace_id = workspace.prepare_material(&material).unwrap();
    launcher
        .launch_material(GuestIsa::Aarch64, &material, workspace_id)
        .unwrap();
    let request = processes.requests.lock().unwrap()[0].clone();
    assert!(
        request
            .file_actions
            .contains(&FileAction::Inherit(HostDescriptor::new(12).unwrap()))
    );
    assert!(
        request
            .file_actions
            .contains(&FileAction::Inherit(HostDescriptor::new(13).unwrap()))
    );
    let environment = request
        .environment
        .iter()
        .map(|value| value.to_bytes())
        .collect::<Vec<_>>();
    assert!(environment.contains(&b"HL_AUTHORITY_HEALTH_FD=13".as_slice()));
    assert!(environment.contains(&b"HL_AUTHORITY_FD=12".as_slice()));
    assert!(
        !environment
            .iter()
            .any(|value| value.starts_with(b"HL_AUTHORITY_SESSION="))
    );
    assert!(
        !environment
            .iter()
            .any(|value| value.starts_with(b"HL_AUTHORITY_FRAME_LIMIT="))
    );
}
