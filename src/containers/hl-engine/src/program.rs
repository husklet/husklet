//! Process entry composition for the production Rust execution path.

use crate::activation::{ActivationStreams, GuestIsa};
use crate::composition::{ActivationChannel, CompositionError, EngineBackend, GuestMachine, RuntimeServices};
use crate::engine::{EngineError, EngineExit, ExitKind, Workspace, WorkspaceId};
use crate::launch_plan::{ConfigOrigin, DiagnosticsMode, Material, MaterialError, RuntimePlan};
use crate::options::Options;
use crate::runtime_machine::{HostServices, RustRuntimeFactory};
use hl_runtime::{RuntimeAssembly, RuntimeAssemblyConfig, RuntimeDomain, RuntimeExecPort, RuntimeForkPort};
use std::path::Path;
use std::sync::Arc;

const CONFIG_LIMIT: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgramError {
    Usage,
    Unsupported,
    Input,
    Configuration,
    Composition(CompositionError),
    Engine(EngineError),
}

impl ProgramError {
    #[must_use]
    pub const fn status(self) -> i32 {
        match self {
            Self::Usage => 64,
            Self::Unsupported => 69,
            Self::Input | Self::Configuration => 65,
            Self::Composition(_) | Self::Engine(_) => 125,
        }
    }
}

pub struct Program;

#[cfg(target_os = "linux")]
struct SessionWatch<'a, M: GuestMachine + 'static, W: Workspace> {
    backend: &'a EngineBackend<M, W>,
    health: crate::native::AuthorityHealth,
    done: &'a std::sync::atomic::AtomicBool,
}

#[cfg(target_os = "linux")]
impl<M: GuestMachine + 'static, W: Workspace> SessionWatch<'_, M, W> {
    fn run(&self) -> Result<(), EngineError> {
        self.health.monitor(self.done, || {
            let _ = self.backend.stop(crate::engine::StopRequest::Force);
        })
    }
}

impl Program {
    #[cfg(target_os = "linux")]
    fn validate_projected(worker: &mut crate::native::AuthorityWorker) -> Result<(), ProgramError> {
        let handle = worker
            .open_file(1)
            .map_err(|_| ProgramError::Engine(EngineError::AuthorityFailed))?;
        let info = worker
            .file_info(handle)
            .map_err(|_| ProgramError::Engine(EngineError::AuthorityFailed));
        let closed = worker
            .close_file(handle)
            .map_err(|_| ProgramError::Engine(EngineError::AuthorityFailed));
        let info = info?;
        closed?;
        if info.size == 0
            || info.device == 0
            || info.inode == 0
            || info.mode & libc::S_IFMT != libc::S_IFREG
            || info.mode & 0o111 == 0
        {
            return Err(ProgramError::Engine(EngineError::AuthorityFailed));
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn wait_authorized<M: GuestMachine + 'static, W: Workspace>(
        backend: &EngineBackend<M, W>,
        health: crate::native::AuthorityHealth,
        stop: crate::native::AuthorityHealth,
    ) -> Result<EngineExit, ProgramError> {
        let done = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|scope| {
            let watch = SessionWatch {
                backend,
                health,
                done: &done,
            };
            let watcher = scope.spawn(move || watch.run());
            let exit = backend.wait().map_err(ProgramError::Engine);
            done.store(true, std::sync::atomic::Ordering::Release);
            stop.stop().map_err(ProgramError::Engine)?;
            let watched = watcher
                .join()
                .map_err(|_| ProgramError::Engine(EngineError::AuthorityFailed))?;
            watched.map_err(ProgramError::Engine)?;
            exit
        })
    }

    /// Runs one process-scoped invocation through the Rust runtime composition.
    pub fn run(arguments: Vec<String>) -> Result<EngineExit, ProgramError> {
        Self::run_authorized(arguments, None, None)
    }

    pub fn run_authorized(
        arguments: Vec<String>,
        authority_descriptor: Option<i32>,
        health_descriptor: Option<i32>,
    ) -> Result<EngineExit, ProgramError> {
        let isa = Selection::from_arguments(&arguments)?;
        let route = crate::cli::Route::parse(arguments.iter().cloned());
        let plan = match route {
            crate::cli::Route::Config { path } => PlanSource::config(&path)?,
            crate::cli::Route::Guest => PlanSource::guest(&arguments)?,
            crate::cli::Route::Server | crate::cli::Route::Client => {
                return Err(ProgramError::Unsupported);
            }
        };
        let requires_authority = authority_descriptor.is_some() || plan.options.get("HL_UNTRUSTED").is_some();
        let authority = match (requires_authority, authority_descriptor, health_descriptor) {
            (false, None, None) => None,
            (true, Some(descriptor), Some(health)) => {
                let mut worker =
                    crate::native::AuthorityWorker::inherit(descriptor, health).map_err(ProgramError::Engine)?;
                worker.enter(|| ()).map_err(ProgramError::Engine)?;
                Self::validate_projected(&mut worker)?;
                Some(Arc::new(std::sync::Mutex::new(worker)))
            }
            _ => return Err(ProgramError::Engine(EngineError::AuthorityFailed)),
        };
        let entropy = authority
            .as_ref()
            .map(|_| crate::native::GuestExecutor::prepare_entropy())
            .transpose()
            .map_err(ProgramError::Engine)?;
        let health = authority
            .as_ref()
            .map(|worker| {
                let worker = worker
                    .lock()
                    .map_err(|_| ProgramError::Engine(EngineError::Synchronization))?;
                Ok::<_, ProgramError>((
                    worker.health().map_err(ProgramError::Engine)?,
                    worker.health().map_err(ProgramError::Engine)?,
                ))
            })
            .transpose()?;
        if authority.is_some() {
            crate::native::HostConfinement::apply().map_err(ProgramError::Engine)?;
        }
        let result = Self::execute(isa, plan, authority.as_ref(), entropy, health);
        if let Some(worker) = authority {
            worker
                .lock()
                .map_err(|_| ProgramError::Engine(EngineError::Synchronization))?
                .close()
                .map_err(ProgramError::Engine)?;
        }
        result
    }

    #[must_use]
    pub const fn exit_status(exit: EngineExit) -> i32 {
        match exit.kind {
            ExitKind::Code => exit.guest_status,
            ExitKind::Signal => 128_i32.saturating_add(exit.guest_status),
            ExitKind::Fault | ExitKind::EngineError => 125,
        }
    }
}

struct Selection;

impl Selection {
    fn from_arguments(arguments: &[String]) -> Result<GuestIsa, ProgramError> {
        if let Some(index) = arguments.iter().position(|argument| argument == "--guest-isa") {
            return arguments
                .get(index + 1)
                .and_then(|value| Self::named(value))
                .ok_or(ProgramError::Usage);
        }
        let executable = arguments
            .first()
            .and_then(|argument| Path::new(argument).file_name())
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if executable.contains("aarch64") {
            return Ok(GuestIsa::Aarch64);
        }
        if executable.contains("x86_64") {
            return Ok(GuestIsa::X86_64);
        }
        #[cfg(target_arch = "aarch64")]
        {
            Ok(GuestIsa::Aarch64)
        }
        #[cfg(target_arch = "x86_64")]
        {
            Ok(GuestIsa::X86_64)
        }
        #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
        {
            Err(ProgramError::Unsupported)
        }
    }

    fn named(value: &str) -> Option<GuestIsa> {
        match value {
            "aarch64" | "arm64" => Some(GuestIsa::Aarch64),
            "x86_64" | "amd64" => Some(GuestIsa::X86_64),
            _ => None,
        }
    }
}

struct PlanSource;

impl PlanSource {
    fn guest(arguments: &[String]) -> Result<RuntimePlan, ProgramError> {
        let mut guest = Vec::new();
        let mut options = Options::default();
        let mut index = 1;
        while index < arguments.len() {
            if arguments[index] == "--guest-isa" {
                index = index.checked_add(2).ok_or(ProgramError::Usage)?;
                continue;
            }
            if arguments[index] == "--engine-option" {
                let assignment = arguments.get(index + 1).ok_or(ProgramError::Usage)?;
                let (name, value) = assignment.split_once('=').ok_or(ProgramError::Usage)?;
                options
                    .set(name, value, true)
                    .map_err(|_| ProgramError::Configuration)?;
                index = index.checked_add(2).ok_or(ProgramError::Usage)?;
                continue;
            }
            guest.extend(arguments[index..].iter().map(|argument| argument.as_bytes().to_vec()));
            break;
        }
        if guest.is_empty() {
            return Err(ProgramError::Usage);
        }
        Ok(RuntimePlan {
            rootfs: None,
            executable_host: Some(guest[0].clone()),
            arguments: guest,
            environment: Vec::new(),
            result_path: None,
            options,
        })
    }

    fn config(path: &str) -> Result<RuntimePlan, ProgramError> {
        let metadata = std::fs::metadata(path).map_err(|_| ProgramError::Input)?;
        if metadata.len() > CONFIG_LIMIT {
            return Err(ProgramError::Input);
        }
        let wire = std::fs::read(path).map_err(|_| ProgramError::Input)?;
        Material::from_validated_wire(
            &wire,
            ConfigOrigin::File(path.as_bytes().to_vec()),
            ActivationStreams::default(),
            None,
            DiagnosticsMode::Disabled,
        )
        .map(|material| material.plan)
        .map_err(|_: MaterialError| ProgramError::Configuration)
    }
}

#[derive(Default)]
struct Activation;

impl ActivationChannel for Activation {
    fn send(&self, _: &[u8]) -> Result<(), CompositionError> {
        Ok(())
    }

    fn receive(&self, _: usize) -> Result<Vec<u8>, CompositionError> {
        Ok(Vec::new())
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

#[derive(Default)]
struct LinuxServices;

impl HostServices for LinuxServices {
    fn exec_port(&self, _: &RuntimeAssembly) -> Result<Option<Arc<dyn RuntimeExecPort>>, CompositionError> {
        Ok(None)
    }

    fn fork_port(&self, _: &RuntimeAssembly) -> Result<Option<Arc<dyn RuntimeForkPort>>, CompositionError> {
        Ok(None)
    }

    fn validate(&self, assembly: &RuntimeAssembly) -> Result<(), CompositionError> {
        assembly
            .require(RuntimeDomain::Task)
            .map_err(|_| CompositionError::RuntimeConstruction)
    }
}

impl Program {
    #[cfg(target_os = "linux")]
    fn execute(
        isa: GuestIsa,
        plan: RuntimePlan,
        authority: Option<&Arc<std::sync::Mutex<crate::native::AuthorityWorker>>>,
        entropy: Option<[u8; 16]>,
        health: Option<(crate::native::AuthorityHealth, crate::native::AuthorityHealth)>,
    ) -> Result<EngineExit, ProgramError> {
        let executor = match authority {
            Some(authority) => crate::native::GuestExecutor::authorized(
                Arc::clone(authority),
                entropy.ok_or(ProgramError::Engine(EngineError::LaunchFailed))?,
            ),
            None => crate::native::GuestExecutor::default(),
        };
        let factory = RustRuntimeFactory::new(
            Arc::new(executor),
            Arc::new(LinuxServices),
            RuntimeAssemblyConfig::default(),
        );
        let services = RuntimeServices {
            activation: Arc::new(Activation),
            checkpoint_sink: None,
            checkpoint_source: None,
        };
        let backend = EngineBackend::construct(isa, plan, services, &factory, WorkspacePort)
            .map_err(ProgramError::Composition)?;
        backend.start().map_err(ProgramError::Engine)?;
        let exit = match authority {
            Some(_) => {
                let (monitor, stop) = health.ok_or(ProgramError::Engine(EngineError::AuthorityFailed))?;
                Self::wait_authorized(&backend, monitor, stop)?
            }
            None => backend.wait().map_err(ProgramError::Engine)?,
        };
        backend.destroy().map_err(ProgramError::Engine)?;
        Ok(exit)
    }

    #[cfg(not(target_os = "linux"))]
    fn execute(
        _: GuestIsa,
        _: RuntimePlan,
        _: Option<&Arc<std::sync::Mutex<crate::native::AuthorityWorker>>>,
        _: Option<[u8; 16]>,
        _: Option<(crate::native::AuthorityHealth, crate::native::AuthorityHealth)>,
    ) -> Result<EngineExit, ProgramError> {
        Err(ProgramError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_isa_overrides() {
        let arguments = vec![
            "hl-aarch64".to_owned(),
            "--guest-isa".to_owned(),
            "amd64".to_owned(),
            "guest".to_owned(),
        ];
        assert_eq!(Selection::from_arguments(&arguments), Ok(GuestIsa::X86_64));
    }

    #[test]
    fn unsupported_routes_fail() {
        assert_eq!(
            Program::run(vec!["hl-engine".to_owned(), "--server".to_owned()]),
            Err(ProgramError::Unsupported)
        );
    }

    #[test]
    fn exit_mapping_matches() {
        assert_eq!(
            Program::exit_status(EngineExit {
                kind: ExitKind::Signal,
                guest_status: 9,
                detail: 0,
                fault: None,
            }),
            137
        );
    }

    #[test]
    fn direct_guest_accepts_typed_engine_options() {
        let arguments = vec![
            "hl-engine".to_owned(),
            "--guest-isa".to_owned(),
            "aarch64".to_owned(),
            "--engine-option".to_owned(),
            "HL_NATIVE_EXECUTION=1".to_owned(),
            "guest".to_owned(),
            "argument".to_owned(),
        ];
        let plan = PlanSource::guest(&arguments).unwrap();
        assert_eq!(plan.options.get("HL_NATIVE_EXECUTION"), Some("1"));
        assert_eq!(plan.arguments, vec![b"guest".to_vec(), b"argument".to_vec()]);
    }

    #[test]
    fn direct_guest_rejects_unknown_engine_options() {
        let arguments = vec![
            "hl-engine".to_owned(),
            "--engine-option".to_owned(),
            "HL_UNKNOWN=1".to_owned(),
            "guest".to_owned(),
        ];
        assert_eq!(
            PlanSource::guest(&arguments).map(drop),
            Err(ProgramError::Configuration)
        );
    }
}
