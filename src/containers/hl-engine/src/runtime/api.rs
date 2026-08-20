//! Owned public lifecycle for one staged production runtime.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

mod execution;
pub(crate) use execution::{ProductionFactory, ProductionMachine};

#[cfg(unix)]
mod checkpoint;
#[cfg(unix)]
pub use checkpoint::authority::CheckpointAuthorityHandle;

#[cfg(unix)]
mod member;
#[cfg(unix)]
pub use member::{MemberExit, RestoredMember};

#[cfg(unix)]
mod line_discipline;
mod terminal;
#[cfg(unix)]
pub use terminal::MemberTerminal;

use crate::activation::GuestIsa;
use crate::composition::{CompositionError, EngineBackend, RuntimeServices};
use crate::engine::{EngineError, EngineExit, StopRequest};
use crate::launcher::plan::RuntimePlan;
use crate::options::Options;

mod workspace;

use workspace::OwnedWorkspace;
pub use workspace::{Input, Rootfs};

#[derive(Clone, Debug)]
pub struct Builder {
    isa: GuestIsa,
    executable: PathBuf,
    inputs: Vec<Input>,
    arguments: Vec<Vec<u8>>,
    rootfs: Option<Rootfs>,
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
            arguments: Vec::new(),
            rootfs: None,
        }
    }

    #[must_use]
    pub fn with_input(mut self, input: Input) -> Self {
        self.inputs.push(input);
        self
    }

    #[must_use]
    pub fn with_argument(mut self, value: impl Into<Vec<u8>>) -> Self {
        self.arguments.push(value.into());
        self
    }

    #[must_use]
    pub fn with_rootfs(mut self, rootfs: Rootfs) -> Self {
        self.rootfs = Some(rootfs);
        self
    }

    pub fn build(self) -> Result<Engine, EngineError> {
        let workspace = OwnedWorkspace::create().inspect_err(|error| {
            hl_log::hl_error!(hl_log::tag::EXEC, "engine workspace creation failed: error={error:?}");
        })?;
        let prepared = match workspace.prepare_rootfs(&self.executable, self.rootfs, None) {
            Ok(value) => value,
            Err(error) => return workspace.fail(error),
        };
        if prepared.rootfs.is_none()
            && let Err(error) = workspace.stage_base_system()
        {
            return workspace.fail(error);
        }
        for input in self.inputs {
            if let Err(error) = workspace.stage_input(input) {
                return workspace.fail(error);
            }
        }
        let guest_launch = prepared.guest_entry;
        let launch = prepared.executable;
        let filesystem_root = prepared.rootfs.as_ref().unwrap_or(&workspace.root);
        if fs::create_dir_all(filesystem_root.join("tmp")).is_err() {
            return workspace.fail(EngineError::WorkspaceFailed);
        }
        let working = match Self::initial_working_directory(prepared.rootfs.is_some()) {
            Ok(path) => path,
            Err(error) => return workspace.fail(error),
        };
        if let Err(error) = workspace.stage_working_directory(filesystem_root, &working) {
            return workspace.fail(error);
        }
        let rootfs = prepared.rootfs.map(|path| path.as_os_str().as_encoded_bytes().to_vec());
        let workspace_text = workspace.root.to_string_lossy();
        let mut options = Options::default();
        if options
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
            environment: Vec::new(),
            result_path: None,
            options,
        };
        let factory = ProductionFactory;
        let services = RuntimeServices {
            checkpoint_sink: None,
            checkpoint_source: None,
            #[cfg(unix)]
            checkpoint_channel: None,
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

    /// Constructs a runtime with optional standard-stream policy.
    ///
    /// On Unix, a terminal request is attached to an owned native PTY. Other
    /// hosts return [`EngineError::Unsupported`] rather than silently
    /// inheriting the supervisor's descriptors.
    pub fn with_streams(
        isa: GuestIsa,
        plan: RuntimePlan,
        streams: crate::composition::StandardStreams,
    ) -> Result<Self, EngineError> {
        let services = RuntimeServices {
            checkpoint_sink: None,
            checkpoint_source: None,
            #[cfg(unix)]
            checkpoint_channel: None,
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
            checkpoint_sink: Some(sink),
            checkpoint_source: Some(source),
            #[cfg(unix)]
            checkpoint_channel: None,
            streams,
        };
        Self::construct(isa, plan, services)
    }

    /// Constructs a runtime that joins an existing process domain's freeze.
    ///
    /// The session shares the coordinator's broker socket and trigger word, so the
    /// coordinator's generation bump reaches its safepoint gates and its guest
    /// processes commit into the coordinator's store. It owns no checkpoint image
    /// and cannot be captured on its own.
    ///
    /// # Errors
    /// Returns the composition refusal that prevented construction.
    #[cfg(unix)]
    pub fn with_checkpoint_channel(
        isa: GuestIsa,
        plan: RuntimePlan,
        streams: crate::composition::StandardStreams,
        channel: crate::composition::CheckpointChannel,
    ) -> Result<Self, EngineError> {
        let services = RuntimeServices {
            checkpoint_sink: None,
            checkpoint_source: None,
            checkpoint_channel: Some(channel),
            streams,
        };
        Self::construct(isa, plan, services)
    }

    fn construct(isa: GuestIsa, plan: RuntimePlan, services: RuntimeServices) -> Result<Self, EngineError> {
        let terminal = services.streams.terminal();
        let workspace = OwnedWorkspace::create().inspect_err(|error| {
            hl_log::hl_error!(hl_log::tag::EXEC, "engine workspace creation failed: error={error:?}");
        })?;
        let factory = ProductionFactory;
        let backend = EngineBackend::construct(isa, plan, services, &factory, workspace.clone()).map_err(|error| {
            hl_log::hl_error!(hl_log::tag::EXEC, "engine composition failed: error={error:?}");
            Self::launch_error(error)
        })?;
        Ok(Self {
            backend,
            workspace,
            terminal,
        })
    }

    /// Maps a composition refusal onto the public engine error, retaining the
    /// originating cause for every case the boundary does not translate.
    fn launch_error(error: CompositionError) -> EngineError {
        if error == CompositionError::UnsupportedTerminal {
            EngineError::Unsupported
        } else {
            EngineError::CompositionFailed(error)
        }
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
    /// The container-namespace pid of the guest process this engine launched, once it has one.
    ///
    /// A whole-image checkpoint names each captured member by this number and a restore re-forks it
    /// under the same one, so it is the only identity of a launched guest that survives a capture.
    #[must_use]
    pub fn guest_pid(&self) -> Option<std::num::NonZeroI32> {
        self.backend.guest_pid()
    }
    /// One member of the process tree this engine restored, by the guest pid its image names it by.
    ///
    /// This is how a host reaches a process a whole-image restore re-forked: the restore produces one
    /// engine handle for a tree of many, and this addresses one of them. `None` for an engine that
    /// started fresh, and for any guest pid the restore did not announce -- in which case a caller
    /// must refuse to present the member as live rather than start a replacement for it.
    #[cfg(unix)]
    #[must_use]
    pub fn restored_member(&self, guest_pid: std::num::NonZeroI32) -> Option<crate::runtime::RestoredMember> {
        self.backend.restored_member(guest_pid)
    }
    /// Registers the terminal one sealed member will reattach to when this engine restores it.
    ///
    /// The producer of a restored member's I/O is necessarily the host, and necessarily earlier than any
    /// attachment: the member asks for its terminal during its own descriptor restore, long before a pane
    /// exists to ask on its behalf. So this must be called before [`Self::start`], once per member whose
    /// session the host intends to be able to reattach.
    ///
    /// # Errors
    /// Returns [`EngineError::Unsupported`] when this engine coordinates no checkpoint.
    #[cfg(unix)]
    pub fn provide_member_terminal(
        &self,
        guest_pid: std::num::NonZeroI32,
        terminal: std::os::fd::OwnedFd,
    ) -> Result<(), EngineError> {
        self.backend.provide_member_terminal(guest_pid, terminal)
    }
    pub fn capture_checkpoint(&self) -> Result<(), EngineError> {
        self.backend.capture_checkpoint()
    }
    /// The freeze channel every session in this engine's process domain must join.
    #[cfg(unix)]
    #[must_use]
    pub fn checkpoint_channel(&self) -> Option<crate::composition::CheckpointChannel> {
        self.backend.checkpoint_channel()
    }
    /// Captures the running process tree before the caller's monotonic deadline.
    ///
    /// # Errors
    /// Returns lifecycle, storage, synchronization, or deadline failures.
    pub fn capture_checkpoint_until(&self, deadline: std::time::Instant) -> Result<(), EngineError> {
        self.backend.capture_checkpoint_until(deadline)
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
}

#[cfg(test)]
mod launch_error_tests {
    use super::{CompositionError, Engine, EngineError};

    #[test]
    fn non_terminal_composition_failures_keep_their_cause() {
        for error in [
            CompositionError::MissingCheckpointSink,
            CompositionError::MissingCheckpointSource,
            CompositionError::RuntimeConstruction,
            CompositionError::TransactionBusy,
            CompositionError::DeadlineExceeded,
            CompositionError::PublishedNotDurable,
        ] {
            assert_eq!(Engine::launch_error(error), EngineError::CompositionFailed(error));
        }
    }

    #[test]
    fn unsupported_terminal_stays_unsupported() {
        assert_eq!(
            Engine::launch_error(CompositionError::UnsupportedTerminal),
            EngineError::Unsupported
        );
    }
}
