//! Linux process composition without claiming translated guest execution.

use crate::activation::GuestIsa;
use crate::engine::{
    Engine, EngineError, EngineExit, ExitKind, Launcher, ProcessId as EngineProcessId, StopRequest, Workspace,
    WorkspaceId,
};
use crate::launch_plan::{LaunchMaterial, RuntimeLaunchPlan};
use crate::native_host::{
    AuthorityAccess, ChildExit, FileAction, HostDescriptor, ProcessGroup, ProcessHandle, ProcessSignal,
    ProcessSyscalls, SpawnRequest,
};
use std::collections::BTreeMap;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const MAXIMUM_WORKSPACE_PATH: usize = 4096;

pub struct Selection;

impl Selection {
    pub fn compose<S: ProcessSyscalls, F: WorkspaceFiles>(
        isa: GuestIsa,
        material: LaunchMaterial,
        staging: PathBuf,
        binaries: PathBuf,
        files: Arc<F>,
        processes: Arc<S>,
    ) -> Result<Engine<ProcessLauncher<S>, ProcessWorkspace<F>>, EngineError> {
        let workspace = ProcessWorkspace::new(staging, files)?;
        let launcher = workspace.launcher(binaries, processes)?;
        Ok(Engine::new_material(isa, material, launcher, workspace))
    }

    pub fn compose_authorized<S: ProcessSyscalls, F: WorkspaceFiles>(
        isa: GuestIsa,
        material: LaunchMaterial,
        staging: PathBuf,
        binaries: PathBuf,
        files: Arc<F>,
        processes: Arc<S>,
        authority: Arc<dyn AuthorityAccess>,
    ) -> Result<Engine<ProcessLauncher<S>, ProcessWorkspace<F>>, EngineError> {
        let workspace = ProcessWorkspace::new(staging, files)?;
        let launcher = workspace.launcher_authorized(binaries, processes, authority)?;
        Ok(Engine::new_material(isa, material, launcher, workspace))
    }
}

pub trait WorkspaceFiles: Send + Sync {
    fn create(&self, path: &Path, wire: &[u8]) -> Result<(), EngineError>;
    fn remove(&self, path: &Path) -> Result<(), EngineError>;
}

#[derive(Default)]
pub struct StandardWorkspaceFiles;

impl WorkspaceFiles for StandardWorkspaceFiles {
    fn create(&self, path: &Path, wire: &[u8]) -> Result<(), EngineError> {
        std::fs::create_dir(path).map_err(|_| EngineError::WorkspaceFailed)?;
        let config = path.join("launch.bin");
        let result = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(config)
            .and_then(|mut file| std::io::Write::write_all(&mut file, wire));
        if result.is_err() {
            let _ = std::fs::remove_dir_all(path);
            return Err(EngineError::WorkspaceFailed);
        }
        Ok(())
    }

    fn remove(&self, path: &Path) -> Result<(), EngineError> {
        std::fs::remove_dir_all(path).map_err(|_| EngineError::WorkspaceFailed)
    }
}

struct WorkspaceState {
    next: u64,
    paths: BTreeMap<u64, PathBuf>,
}

pub struct ProcessWorkspace<F> {
    root: PathBuf,
    files: Arc<F>,
    state: Arc<Mutex<WorkspaceState>>,
}

impl<F> Clone for ProcessWorkspace<F> {
    fn clone(&self) -> Self {
        Self {
            root: self.root.clone(),
            files: Arc::clone(&self.files),
            state: Arc::clone(&self.state),
        }
    }
}

impl<F: WorkspaceFiles> ProcessWorkspace<F> {
    pub fn new(root: PathBuf, files: Arc<F>) -> Result<Self, EngineError> {
        if !root.is_absolute() || root.as_os_str().len() > MAXIMUM_WORKSPACE_PATH {
            return Err(EngineError::WorkspaceFailed);
        }
        Ok(Self {
            root,
            files,
            state: Arc::new(Mutex::new(WorkspaceState {
                next: 1,
                paths: BTreeMap::new(),
            })),
        })
    }

    pub fn launcher<S: ProcessSyscalls>(
        &self,
        binaries: PathBuf,
        processes: Arc<S>,
    ) -> Result<ProcessLauncher<S>, EngineError> {
        if !binaries.is_absolute() || binaries.as_os_str().len() > MAXIMUM_WORKSPACE_PATH {
            return Err(EngineError::LaunchFailed);
        }
        Ok(ProcessLauncher {
            binaries,
            processes,
            workspaces: Arc::clone(&self.state),
            children: Mutex::new(BTreeMap::new()),
            next_failure: Mutex::new(u64::MAX),
            authority: None,
        })
    }

    pub fn launcher_authorized<S: ProcessSyscalls>(
        &self,
        binaries: PathBuf,
        processes: Arc<S>,
        authority: Arc<dyn AuthorityAccess>,
    ) -> Result<ProcessLauncher<S>, EngineError> {
        let mut launcher = self.launcher(binaries, processes)?;
        launcher.authority = Some(authority);
        Ok(launcher)
    }
}

impl<F: WorkspaceFiles> Workspace for ProcessWorkspace<F> {
    fn prepare(&self) -> Result<WorkspaceId, EngineError> {
        Err(EngineError::WorkspaceFailed)
    }

    fn prepare_material(&self, material: &LaunchMaterial) -> Result<WorkspaceId, EngineError> {
        let (identifier, path) = {
            let mut state = self.state.lock().map_err(|_| EngineError::Synchronization)?;
            let identifier = state.next;
            state.next = state.next.checked_add(1).ok_or(EngineError::WorkspaceFailed)?;
            (identifier, self.root.join(format!("engine-{identifier:016x}")))
        };
        if path.as_os_str().len() > MAXIMUM_WORKSPACE_PATH {
            return Err(EngineError::WorkspaceFailed);
        }
        self.files.create(&path, &material.wire)?;
        self.state
            .lock()
            .map_err(|_| EngineError::Synchronization)?
            .paths
            .insert(identifier, path);
        Ok(WorkspaceId(identifier))
    }

    fn cleanup(&self, workspace: WorkspaceId) -> Result<(), EngineError> {
        let path = self
            .state
            .lock()
            .map_err(|_| EngineError::Synchronization)?
            .paths
            .remove(&workspace.0)
            .ok_or(EngineError::WorkspaceFailed)?;
        self.files.remove(&path)
    }
}

pub struct ProcessLauncher<S: ProcessSyscalls> {
    binaries: PathBuf,
    processes: Arc<S>,
    workspaces: Arc<Mutex<WorkspaceState>>,
    children: Mutex<BTreeMap<u64, Child<S>>>,
    next_failure: Mutex<u64>,
    authority: Option<Arc<dyn AuthorityAccess>>,
}

enum Child<S: ProcessSyscalls> {
    Process(Arc<ProcessHandle<S>>),
    Immediate(EngineExit),
}

impl<S: ProcessSyscalls> ProcessLauncher<S> {
    fn authority(
        &self,
        material: &LaunchMaterial,
        actions: &mut Vec<FileAction>,
        environment: &mut Vec<CString>,
    ) -> Result<Option<crate::native::AuthorityChannel>, EngineError> {
        if !material.sandbox.requires_authority() {
            return Ok(None);
        }
        let authority = self
            .authority
            .as_ref()
            .ok_or(EngineError::AuthorityFailed)?
            .open(material.process_domain)?;
        actions.push(FileAction::Inherit(authority.descriptor()));
        actions.push(FileAction::Inherit(authority.health()));
        environment.push(
            CString::new(format!("HL_AUTHORITY_FD={}", authority.descriptor().raw()))
                .map_err(|_| EngineError::AuthorityFailed)?,
        );
        environment.push(
            CString::new(format!("HL_AUTHORITY_HEALTH_FD={}", authority.health().raw()))
                .map_err(|_| EngineError::AuthorityFailed)?,
        );
        Ok(Some(authority))
    }

    fn request(
        &self,
        isa: GuestIsa,
        material: &LaunchMaterial,
        workspace: WorkspaceId,
    ) -> Result<(SpawnRequest, Option<crate::native::AuthorityChannel>), EngineError> {
        let path = self
            .workspaces
            .lock()
            .map_err(|_| EngineError::Synchronization)?
            .paths
            .get(&workspace.0)
            .cloned()
            .ok_or(EngineError::WorkspaceFailed)?;
        let program = self.binaries.join(isa.engine_stem());
        let mut file_actions = Vec::new();
        for (descriptor, target) in [
            (material.streams.input.abi_value(), 0),
            (material.streams.output.abi_value(), 1),
            (material.streams.error.abi_value(), 2),
        ] {
            Self::append_stream_action(&mut file_actions, descriptor, target)?;
        }
        let mut environment = Vec::new();
        let authority = self.authority(material, &mut file_actions, &mut environment)?;
        if let Some(channel) = material.activation_channel {
            let descriptor = HostDescriptor::new(channel.get()).map_err(|_| EngineError::LaunchFailed)?;
            file_actions.push(FileAction::Inherit(descriptor));
            environment.push(
                CString::new(format!("HL_ACTIVATION_FD={}", channel.get())).map_err(|_| EngineError::LaunchFailed)?,
            );
        }
        Ok((
            SpawnRequest {
                program: Self::cstring(&program)?,
                arguments: vec![
                    CString::new("--configfile").map_err(|_| EngineError::LaunchFailed)?,
                    Self::cstring(&path.join("launch.bin"))?,
                ],
                environment,
                process_group: ProcessGroup::New,
                file_actions,
            },
            authority,
        ))
    }

    fn cstring(path: &Path) -> Result<CString, EngineError> {
        CString::new(path.as_os_str().as_bytes()).map_err(|_| EngineError::LaunchFailed)
    }

    fn append_stream_action(actions: &mut Vec<FileAction>, descriptor: u64, target: i32) -> Result<(), EngineError> {
        if descriptor == 0 {
            return Ok(());
        }
        actions.push(FileAction::Duplicate {
            source: HostDescriptor::new(descriptor as i32).map_err(|_| EngineError::LaunchFailed)?,
            target: HostDescriptor::new(target).map_err(|_| EngineError::LaunchFailed)?,
        });
        Ok(())
    }
}

impl<S: ProcessSyscalls> Launcher for ProcessLauncher<S> {
    fn launch(&self, _: GuestIsa, _: &RuntimeLaunchPlan, _: WorkspaceId) -> Result<EngineProcessId, EngineError> {
        Err(EngineError::LaunchFailed)
    }

    fn launch_material(
        &self,
        isa: GuestIsa,
        material: &LaunchMaterial,
        workspace: WorkspaceId,
    ) -> Result<EngineProcessId, EngineError> {
        let (request, authority_channel) = self.request(isa, material, workspace)?;
        let process = match ProcessHandle::spawn(Arc::clone(&self.processes), &request) {
            Ok(process) => process,
            Err(crate::native_host::HostError::NotFound) => {
                if let (Some(access), Some(channel)) = (&self.authority, authority_channel) {
                    access.rollback(channel);
                }
                let mut next = self.next_failure.lock().map_err(|_| EngineError::Synchronization)?;
                let identifier = *next;
                *next = next.checked_sub(1).ok_or(EngineError::LaunchFailed)?;
                self.children.lock().map_err(|_| EngineError::Synchronization)?.insert(
                    identifier,
                    Child::Immediate(EngineExit {
                        kind: ExitKind::Code,
                        guest_status: 127,
                        detail: 0,
                        fault: None,
                    }),
                );
                return Ok(EngineProcessId(identifier));
            }
            Err(_) => {
                if let (Some(access), Some(channel)) = (&self.authority, authority_channel) {
                    access.rollback(channel);
                }
                return Err(EngineError::LaunchFailed);
            }
        };
        if let (Some(access), Some(channel)) = (&self.authority, authority_channel)
            && let Err(error) = access.commit(channel)
        {
            let _ = process.signal(ProcessSignal::Kill);
            return Err(error);
        }
        let identifier = u64::from(process.id().get());
        self.children
            .lock()
            .map_err(|_| EngineError::Synchronization)?
            .insert(identifier, Child::Process(Arc::new(process)));
        Ok(EngineProcessId(identifier))
    }

    fn wait(&self, process: EngineProcessId) -> Result<EngineExit, EngineError> {
        let child = {
            let mut children = self.children.lock().map_err(|_| EngineError::Synchronization)?;
            match children.get(&process.0) {
                Some(Child::Process(child)) => Arc::clone(child),
                Some(Child::Immediate(exit)) => {
                    let exit = *exit;
                    children.remove(&process.0);
                    return Ok(exit);
                }
                None => return Err(EngineError::WaitFailed),
            }
        };
        let exit = match child.wait_blocking().map_err(|_| EngineError::WaitFailed)? {
            ChildExit::Code(code) => Ok(EngineExit {
                kind: ExitKind::Code,
                guest_status: i32::from(code),
                detail: 0,
                fault: None,
            }),
            ChildExit::Signal(signal) => Ok(EngineExit {
                kind: ExitKind::Signal,
                guest_status: i32::from(signal),
                detail: 0,
                fault: None,
            }),
        };
        self.children
            .lock()
            .map_err(|_| EngineError::Synchronization)?
            .remove(&process.0);
        exit
    }

    fn terminate(&self, process: EngineProcessId, request: StopRequest) -> Result<(), EngineError> {
        let children = self.children.lock().map_err(|_| EngineError::Synchronization)?;
        let child = children.get(&process.0).ok_or(EngineError::StopFailed)?;
        let Child::Process(child) = child else {
            return Err(EngineError::Busy);
        };
        let signal = match request {
            StopRequest::Interrupt => ProcessSignal::Interrupt,
            StopRequest::Force => ProcessSignal::Kill,
            StopRequest::Signal(15) => ProcessSignal::Terminate,
            StopRequest::Signal(_) => return Err(EngineError::Unsupported),
        };
        child.signal_group(signal).map_err(|_| EngineError::StopFailed)
    }
}

#[cfg(test)]
#[path = "launcher_test.rs"]
mod tests;
