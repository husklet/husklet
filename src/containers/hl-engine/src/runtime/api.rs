//! Owned public lifecycle for one staged production runtime.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

mod execution;
use execution::{ProductionFactory, ProductionMachine};

use hl_runtime::{RuntimeAssembly, RuntimeDomain, RuntimeExecPort, RuntimeForkPort};

use crate::activation::GuestIsa;
use crate::composition::{ActivationChannel, CompositionError, EngineBackend, RuntimeServices};
use crate::engine::{EngineError, EngineExit, StopRequest};
use crate::launch_plan::RuntimePlan;
use crate::options::Options;
use crate::runtime_machine::HostServices;

mod workspace;

use workspace::OwnedWorkspace;
pub use workspace::{BaseSystem, Input, Rootfs};

pub use crate::ffi::linux::execution::network::CheckpointRuntime;

#[derive(Clone, Debug)]
pub struct Builder {
    isa: GuestIsa,
    executable: PathBuf,
    inputs: Vec<Input>,
    options: Vec<(String, String)>,
    arguments: Vec<Vec<u8>>,
    environment: Vec<(Vec<u8>, Vec<u8>)>,
    entry: Option<PathBuf>,
    rootfs: Option<Rootfs>,
    base_system: BaseSystem,
    trace: Option<PathBuf>,
    guest_executable: Option<PathBuf>,
}

impl Builder {
    fn initial_working_directory(rooted: bool) -> Result<PathBuf, EngineError> {
        if rooted {
            Ok(PathBuf::from("/"))
        } else {
            std::env::current_dir().map_err(|_| EngineError::WorkspaceFailed)
        }
    }

    #[must_use]
    pub fn new(isa: GuestIsa, executable: impl Into<PathBuf>) -> Self {
        Self {
            isa,
            executable: executable.into(),
            inputs: Vec::new(),
            options: Vec::new(),
            arguments: Vec::new(),
            environment: Vec::new(),
            entry: None,
            rootfs: None,
            base_system: BaseSystem::linux(),
            trace: None,
            guest_executable: None,
        }
    }

    #[must_use]
    pub fn with_input(mut self, input: Input) -> Self {
        self.inputs.push(input);
        self
    }

    #[must_use]
    pub fn with_option(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.options.push((name.into(), value.into()));
        self
    }

    #[must_use]
    pub fn with_argument(mut self, value: impl Into<Vec<u8>>) -> Self {
        self.arguments.push(value.into());
        self
    }

    #[must_use]
    pub fn with_environment(mut self, name: impl AsRef<[u8]>, value: impl AsRef<[u8]>) -> Self {
        self.environment.push((name.as_ref().to_vec(), value.as_ref().to_vec()));
        self
    }

    #[must_use]
    pub fn with_entry(mut self, relative: impl Into<PathBuf>) -> Self {
        self.entry = Some(relative.into());
        self
    }

    #[must_use]
    pub fn with_rootfs(mut self, rootfs: Rootfs) -> Self {
        self.rootfs = Some(rootfs);
        self
    }

    #[must_use]
    pub fn with_base_system(mut self, system: BaseSystem) -> Self {
        self.base_system = system;
        self
    }

    #[must_use]
    pub fn with_trace(mut self, path: impl Into<PathBuf>) -> Self {
        self.trace = Some(path.into());
        self
    }

    #[must_use]
    pub fn with_guest_executable(mut self, path: impl Into<PathBuf>) -> Self {
        self.guest_executable = Some(path.into());
        self
    }

    pub fn build(self) -> Result<Engine, EngineError> {
        let Some(environment) = Self::environment(self.environment) else {
            return Err(EngineError::LaunchFailed);
        };
        let workspace = OwnedWorkspace::create().map_err(|error| {
            hl_log::hl_error!(hl_log::tag::EXEC, "engine workspace creation failed: error={error:?}");
            error
        })?;
        let prepared = match workspace.prepare_rootfs(&self.executable, self.rootfs, self.guest_executable.as_deref()) {
            Ok(value) => value,
            Err(error) => return workspace.fail(error),
        };
        if prepared.rootfs.is_none()
            && let Err(error) = workspace.stage_base_system(self.base_system)
        {
            return workspace.fail(error);
        }
        for input in self.inputs {
            if let Err(error) = workspace.stage_input(input) {
                return workspace.fail(error);
            }
        }
        let guest_launch = self.entry.as_ref().map_or_else(
            || prepared.guest_entry.clone(),
            |relative| Path::new("/").join(relative),
        );
        let launch = self
            .entry
            .map_or(prepared.executable.clone(), |relative| workspace.root.join(relative));
        let filesystem_root = prepared.rootfs.as_ref().unwrap_or(&workspace.root);
        if fs::create_dir_all(filesystem_root.join("tmp")).is_err() {
            return workspace.fail(EngineError::WorkspaceFailed);
        }
        let default_working = !self.options.iter().any(|(name, _)| name == "HL_CWD");
        let working = if default_working {
            let path = match Self::initial_working_directory(prepared.rootfs.is_some()) {
                Ok(path) => path,
                Err(error) => return workspace.fail(error),
            };
            if let Err(error) = workspace.stage_working_directory(filesystem_root, &path) {
                return workspace.fail(error);
            }
            Some(path)
        } else {
            None
        };
        let rootfs = prepared.rootfs.map(|path| path.as_os_str().as_encoded_bytes().to_vec());
        let workspace_text = workspace.root.to_string_lossy();
        let mut options = Options::default();
        for (name, value) in self.options {
            let value = value.replace("{workspace}", &workspace_text);
            if options.set(&name, &value, true).is_err() {
                return workspace.fail(EngineError::LaunchFailed);
            }
        }
        if let Some(working) = working
            && options
                .set_bytes("HL_CWD", working.as_os_str().as_encoded_bytes(), true)
                .is_err()
        {
            return workspace.fail(EngineError::LaunchFailed);
        }
        let plan = RuntimePlan {
            rootfs,
            executable_host: Some(launch.as_os_str().as_encoded_bytes().to_vec()),
            arguments: std::iter::once(guest_launch.as_os_str().as_encoded_bytes().to_vec())
                .chain(self.arguments.into_iter().map(|value| {
                    String::from_utf8_lossy(&value)
                        .replace("{workspace}", &workspace_text)
                        .into_bytes()
                }))
                .collect(),
            environment,
            result_path: self.trace.map(|path| path.as_os_str().as_encoded_bytes().to_vec()),
            options,
        };
        let factory = ProductionFactory;
        let services = RuntimeServices {
            activation: Arc::new(Activation),
            checkpoint_sink: None,
            checkpoint_source: None,
            streams: crate::composition::StandardStreams::default(),
        };
        let Ok(backend) = EngineBackend::construct(self.isa, plan, services, &factory, workspace.clone()) else {
            return workspace.fail(EngineError::LaunchFailed);
        };
        Ok(Engine {
            backend,
            workspace,
            terminal: None,
        })
    }

    fn environment(values: Vec<(Vec<u8>, Vec<u8>)>) -> Option<Vec<Vec<u8>>> {
        if values.len() > 4096 {
            return None;
        }
        let mut bytes = 0_usize;
        let mut records = Vec::with_capacity(values.len());
        for (name, value) in values {
            if name.is_empty() || name.contains(&b'=') || name.contains(&0) || value.contains(&0) {
                return None;
            }
            let Some(length) = name.len().checked_add(value.len()).and_then(|size| size.checked_add(2)) else {
                return None;
            };
            let Some(total) = bytes.checked_add(length) else {
                return None;
            };
            if total > 64 * 1024 * 1024 {
                return None;
            }
            bytes = total;
            let mut record = name;
            record.push(b'=');
            record.extend(value);
            records.push(record);
        }
        Some(records)
    }
}

type Backend = EngineBackend<ProductionMachine, OwnedWorkspace>;

pub struct Engine {
    backend: Backend,
    workspace: OwnedWorkspace,
    terminal: Option<Arc<crate::composition::Terminal>>,
}

impl Engine {
    /// Constructs the production Rust runtime from a fully resolved launch plan.
    ///
    /// Container composition uses this boundary after it has resolved images,
    /// mounts, networking, identity, and resource policy. The engine retains
    /// ownership of runtime construction and native execution.
    pub fn from_plan(isa: GuestIsa, plan: RuntimePlan) -> Result<Self, EngineError> {
        Self::with_streams(isa, plan, crate::composition::StandardStreams::default())
    }

    /// Constructs a runtime whose Linux descriptors 0, 1, and 2 use the supplied process streams.
    pub fn with_streams(
        isa: GuestIsa,
        plan: RuntimePlan,
        streams: crate::composition::StandardStreams,
    ) -> Result<Self, EngineError> {
        let services = RuntimeServices {
            activation: Arc::new(Activation),
            checkpoint_sink: None,
            checkpoint_source: None,
            streams,
        };
        Self::construct(isa, plan, services)
    }

    /// Constructs a runtime with application-owned durable checkpoint transport.
    pub fn with_checkpoint(
        isa: GuestIsa,
        plan: RuntimePlan,
        streams: crate::composition::StandardStreams,
        sink: Arc<dyn crate::composition::CheckpointSink>,
        source: Arc<dyn crate::composition::CheckpointSource>,
    ) -> Result<Self, EngineError> {
        let services = RuntimeServices {
            activation: Arc::new(Activation),
            checkpoint_sink: Some(sink),
            checkpoint_source: Some(source),
            streams,
        };
        Self::construct(isa, plan, services)
    }

    fn construct(isa: GuestIsa, plan: RuntimePlan, services: RuntimeServices) -> Result<Self, EngineError> {
        let terminal = services.streams.terminal();
        let workspace = OwnedWorkspace::create().map_err(|error| {
            hl_log::hl_error!(hl_log::tag::EXEC, "engine workspace creation failed: error={error:?}");
            error
        })?;
        let factory = ProductionFactory;
        let backend = EngineBackend::construct(isa, plan, services, &factory, workspace.clone())
            .map_err(|_| EngineError::LaunchFailed)?;
        Ok(Self {
            backend,
            workspace,
            terminal,
        })
    }

    pub fn start(&self) -> Result<(), EngineError> {
        self.backend.start()
    }
    pub fn wait(&self) -> Result<EngineExit, EngineError> {
        self.backend.wait()
    }
    pub fn stop(&self, request: StopRequest) -> Result<(), EngineError> {
        self.backend.stop(request)
    }
    pub fn checkpoint_supported(&self) -> Result<(), EngineError> {
        self.backend.checkpoint_supported()
    }
    pub fn capture_checkpoint(&self) -> Result<(), EngineError> {
        self.backend.capture_checkpoint()
    }
    pub fn resize_terminal(&self, rows: u16, columns: u16) -> Result<(), EngineError> {
        self.terminal
            .as_ref()
            .ok_or(EngineError::Unsupported)?
            .resize(rows, columns)
            .map_err(|_| EngineError::LaunchFailed)
    }
    pub fn destroy(&self) -> Result<Option<EngineExit>, EngineError> {
        self.backend.destroy()
    }
    #[must_use]
    pub fn workspace(&self) -> &Path {
        &self.workspace.root
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.backend.destroy();
    }
}

struct Activation;
impl ActivationChannel for Activation {
    fn send(&self, _: &[u8]) -> Result<(), CompositionError> {
        Ok(())
    }
    fn receive(&self, _: usize) -> Result<Vec<u8>, CompositionError> {
        Ok(Vec::new())
    }
}

struct Services;
impl HostServices for Services {
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

#[cfg(test)]
mod tests {
    use super::Builder;
    use std::path::Path;

    #[test]
    fn initial_working_directory_matches_launch_mode() {
        assert_eq!(Builder::initial_working_directory(true).unwrap(), Path::new("/"));
        assert_eq!(
            Builder::initial_working_directory(false).unwrap(),
            std::env::current_dir().unwrap()
        );
    }

    #[test]
    fn environment_exact() {
        assert_eq!(
            Builder::environment(vec![(b"TZ".to_vec(), b"UTC\xff".to_vec())]),
            Some(vec![b"TZ=UTC\xff".to_vec()]),
        );
        assert_eq!(Builder::environment(Vec::new()), Some(Vec::new()));
    }

    #[test]
    fn environment_invalid() {
        for name in [b"".as_slice(), b"A=B", b"A\0B"] {
            assert_eq!(Builder::environment(vec![(name.to_vec(), b"x".to_vec())]), None);
        }
        assert_eq!(Builder::environment(vec![(b"A".to_vec(), b"x\0y".to_vec())]), None);
        assert_eq!(Builder::environment(vec![(b"A".to_vec(), Vec::new()); 4097]), None);
    }
}
