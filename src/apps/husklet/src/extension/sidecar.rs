//! The container one extension runs in, and the supervision that keeps it there.
//!
//! An extension is a program someone else wrote, so the container it gets is
//! described entirely here rather than taken from its image: the private socket
//! and private durable data mounts, no network, and the resource ceiling the
//! protocol already clamps to. Anything the image asks for beyond that is ignored.
//!
//! Building the specification and computing its signature are pure functions
//! over a [`Manifest`], a [`Grant`], an [`Image`], and a socket path. Nothing in
//! that path reaches a daemon, so the reuse-versus-recreate decision — the only
//! part where a mistake silently leaves an extension running under a grant it no
//! longer has — is tested without a container runtime.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use hl_client::model::{CreateContainer, CreateExecution, DockerMount, HostConfig, InspectImage};
use hl_extension::port::HostError;
use hl_extension::{Grant, Manifest, Resources};

use super::{Bridge, failure};

/// The only environment variable an extension's container is given.
///
/// Everything else an extension needs it asks for over the socket, so the
/// environment cannot become an unaudited second channel into the sandbox.
pub const SOCKET_VARIABLE: &str = "HUSKLET_EXTENSION_SOCKET";

/// Where the host socket appears inside the container.
pub const SOCKET_TARGET: &str = "/run/husklet/extension.sock";

/// Stable private storage owned by this extension inside its workspace.
pub const DATA_TARGET: &str = "/var/lib/husklet-extension";

/// The path is also advertised so extensions do not have to duplicate it.
pub const DATA_VARIABLE: &str = "HUSKLET_EXTENSION_DATA";

/// Prefix of every extension container name, so a workspace's extension
/// containers are recognizable without consulting a label.
pub const NAME_PREFIX: &str = "extension-";

// Bump whenever a host-side confinement invariant changes. Otherwise a sidecar
// created under an older, weaker request would retain the same signature and be
// reused instead of replaced.
const SANDBOX_REVISION: &str = "socket-projection-v3";
const NODE_OPTIONS: &str = "--jitless";

/// Label carrying the specification signature.
pub const SIGNATURE_LABEL: &str = "husklet.extension.signature";
pub const GENERATION_LABEL: &str = "husklet.extension.generation";

/// Creation is a read/decide/write transaction over a reusable daemon name.
static ENSURE: Mutex<()> = Mutex::new(());

/// Label carrying the extension name, so a stray container can be traced back
/// to the extension that owns it.
pub const NAME_LABEL: &str = "husklet.extension.name";

/// How long a stop waits for the extension's process before it is forced.
const STOP_SECONDS: u64 = 5;

/// Permissions on the socket directory: the owner and nobody else.
#[cfg(unix)]
const DIRECTORY_MODE: u32 = 0o700;

/// The private parent supplies isolation. The mounted child must be writable by
/// the image's unprivileged uid, which need not equal the desktop user's uid.
#[cfg(unix)]
const DATA_MODE: u32 = 0o777;

/// Permissions on the socket itself. An extension's socket is its credential:
/// anyone who can connect to it holds that extension's whole grant.
#[cfg(unix)]
// The socket is mounted as a single file into a container that runs as the
// image's unprivileged user. Its 0700 host directory remains the credential;
// the mounted inode itself must permit that different uid to connect.
const SOCKET_MODE: u32 = 0o666;

/// What the sidecar takes from the image it runs.
///
/// The entrypoint and the user are read from the image rather than forced,
/// because an extension image that declares an unprivileged user has already
/// made the safer choice and overriding it to root would undo that.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Image {
    /// The reference the container is created from.
    pub reference: String,
    /// The digest the grant was given for.
    pub digest: String,
    /// The image's own entrypoint, used when the manifest declares none.
    pub entrypoint: Vec<String>,
    /// Arguments supplied by the image to its entrypoint.
    pub command: Vec<String>,
    /// The image's user, empty when the image names none.
    pub user: String,
}

impl Image {
    /// Reads what the sidecar needs out of a daemon image inspection.
    #[must_use]
    pub fn from_inspection(reference: impl Into<String>, inspection: &InspectImage) -> Self {
        Self {
            reference: reference.into(),
            digest: inspection.id.clone(),
            entrypoint: inspection.config.entrypoint.clone(),
            command: inspection.config.cmd.clone(),
            user: inspection.config.user.clone(),
        }
    }
}

/// Everything about one extension's container, resolved and clamped.
///
/// Built once and then used both to decide whether an existing container may be
/// reused and to create a new one, so the two can never disagree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SidecarSpec {
    name: String,
    image: Image,
    entrypoint: Vec<String>,
    command: Vec<String>,
    granted: Vec<String>,
    resources: Resources,
    socket: PathBuf,
    data: PathBuf,
    generation: i64,
}

impl SidecarSpec {
    /// Resolves a manifest, the grant a person actually gave, and an image into
    /// the container that will be created.
    ///
    /// The grant passed in is the recorded one, never `manifest.capabilities`:
    /// the manifest states what was asked for, and an image update must not be
    /// able to widen what is running by restating its request.
    #[must_use]
    pub fn new(manifest: &Manifest, granted: &Grant, image: &Image, socket: impl Into<PathBuf>) -> Self {
        let entrypoint = manifest.entrypoint.clone().unwrap_or_else(|| image.entrypoint.clone());
        let name = format!("{NAME_PREFIX}{}", manifest.name);
        let socket = socket.into();
        let data = socket
            .parent()
            .map_or_else(|| PathBuf::from("data"), |directory| directory.join("data"));
        Self {
            data,
            name,
            image: image.clone(),
            entrypoint,
            command: image.command.clone(),
            granted: granted
                .iter()
                .map(|capability| capability.as_str().to_owned())
                .collect(),
            resources: manifest.resources.clamp(),
            socket,
            generation: 0,
        }
    }

    #[must_use]
    pub const fn generation(mut self, generation: i64) -> Self {
        self.generation = generation;
        self
    }

    /// The container name this specification claims.
    #[must_use]
    pub fn container(&self) -> &str {
        &self.name
    }

    /// The resources the host will actually grant, already clamped.
    #[must_use]
    pub const fn resources(&self) -> Resources {
        self.resources
    }

    /// The host socket the extension speaks to.
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// The host-owned directory retained across sidecar replacement.
    #[must_use]
    pub fn data(&self) -> &Path {
        &self.data
    }

    /// A digest over everything that makes an existing container unusable when
    /// it changes: the image digest, the granted capabilities, the clamped
    /// resource limits, and the socket path.
    ///
    /// Each field is length-prefixed before hashing so that no two different
    /// specifications can concatenate to the same bytes — the same encoding the
    /// workspace runtime container uses for its own signature label.
    #[must_use]
    pub fn signature(&self) -> String {
        use sha2::Digest as _;

        let digest = sha2::Sha256::digest(self.identity().as_bytes());
        let mut signature = String::with_capacity(digest.len() * 2);
        for byte in digest {
            use std::fmt::Write as _;
            let _ = write!(signature, "{byte:02x}");
        }
        signature
    }

    /// The signed bytes, kept separate from hashing so the encoding is readable.
    fn identity(&self) -> String {
        let mut value = String::new();
        Self::field(&mut value, &self.image.digest);
        for argument in &self.entrypoint {
            Self::field(&mut value, argument);
        }
        for argument in &self.command {
            Self::field(&mut value, argument);
        }
        Self::field(&mut value, &self.image.user);
        for capability in &self.granted {
            Self::field(&mut value, capability);
        }
        for limit in [
            self.resources.memory_mb,
            self.resources.cpus,
            self.resources.process_count,
        ] {
            Self::field(&mut value, &limit.to_string());
        }
        Self::field(&mut value, &self.socket.to_string_lossy());
        Self::field(&mut value, &self.data.to_string_lossy());
        Self::field(&mut value, &self.generation.to_string());
        Self::field(&mut value, "interpreted");
        Self::field(&mut value, NODE_OPTIONS);
        Self::field(&mut value, SANDBOX_REVISION);
        value
    }

    fn field(output: &mut String, value: &str) {
        use std::fmt::Write as _;
        let _ = write!(output, "{}:{value}", value.len());
    }

    /// The create request this specification stands for.
    ///
    /// The signature travels as a label so the next run can compare against
    /// what was actually created rather than re-deriving it from state that may
    /// since have moved.
    #[must_use]
    pub fn request(&self) -> CreateContainer {
        CreateContainer {
            image: self.image.reference.clone(),
            labels: self.labels(),
            entrypoint: Some(self.entrypoint.clone()).filter(|values| !values.is_empty()),
            cmd: Some(self.command.clone()).filter(|values| !values.is_empty()),
            env: Some(vec![
                format!("{SOCKET_VARIABLE}={SOCKET_TARGET}"),
                format!("{DATA_VARIABLE}={DATA_TARGET}"),
                // V8 executable pages are not compatible with the translated backend yet. Keep
                // extension resource accounting and sandboxing while running JavaScript without JIT.
                format!("NODE_OPTIONS={NODE_OPTIONS}"),
            ]),
            user: Some(self.image.user.clone()).filter(|user| !user.is_empty()),
            execution: CreateExecution::Interpreted,
            host_config: Some(self.host()),
            ..CreateContainer::default()
        }
    }

    /// The labels the container carries.
    fn labels(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            (SIGNATURE_LABEL.to_owned(), self.signature()),
            (GENERATION_LABEL.to_owned(), self.generation.to_string()),
            (
                NAME_LABEL.to_owned(),
                self.name.trim_start_matches(NAME_PREFIX).to_owned(),
            ),
        ])
    }

    /// The host-side settings: the private socket, private durable data, no
    /// network, and the clamped limits expressed in daemon units.
    fn host(&self) -> HostConfig {
        HostConfig {
            mounts: vec![
                DockerMount {
                    kind: "bind".to_owned(),
                    source: self.socket.to_string_lossy().into_owned(),
                    target: SOCKET_TARGET.to_owned(),
                    ..DockerMount::default()
                },
                DockerMount {
                    kind: "bind".to_owned(),
                    source: self.data.to_string_lossy().into_owned(),
                    target: DATA_TARGET.to_owned(),
                    ..DockerMount::default()
                },
            ],
            memory: i64::from(self.resources.memory_mb) * 1024 * 1024,
            nano_cpus: i64::from(self.resources.cpus) * 1_000_000_000,
            pids_limit: Some(i64::from(self.resources.process_count)),
            // The native engine cannot currently layer the private socket over
            // a read-only image root. The container remains ephemeral and is
            // still isolated by user, sentry, network, resource, and socket
            // capability boundaries.
            readonly_rootfs: false,
            // An extension reaches the world through its socket, where every
            // request is checked against its grant. A network interface would
            // be a way around that check.
            network_mode: "none".to_owned(),
            ..HostConfig::default()
        }
    }

    /// Creates the socket's directory owner-only and tightens the socket itself
    /// if it is already there.
    ///
    /// Called before the listener binds, because a socket that exists for even
    /// a moment at a wider mode is a window in which any local process can hold
    /// this extension's grant.
    ///
    /// # Errors
    /// Returns the failure to create the directory or to read or change either
    /// mode.
    pub fn prepare(&self) -> std::io::Result<()> {
        let Some(directory) = self.socket.parent() else {
            return Err(std::io::Error::other("extension socket path has no directory"));
        };
        std::fs::create_dir_all(directory)?;
        confine(directory, DIRECTORY_MODE)?;
        match std::fs::symlink_metadata(&self.data) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => return Err(std::io::Error::other("extension data path is not a real directory")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir(&self.data)?,
            Err(error) => return Err(error),
        }
        confine(&self.data, DATA_MODE)?;
        match std::fs::symlink_metadata(&self.socket) {
            Ok(_) => confine(&self.socket, SOCKET_MODE),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Deletes private data only after uninstall has stopped the exact sidecar.
    pub fn purge_data(&self) -> std::io::Result<()> {
        match std::fs::symlink_metadata(&self.data) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                std::fs::remove_dir_all(&self.data)
            }
            Ok(_) => Err(std::io::Error::other("extension data path is not a real directory")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

/// Sets one path's permissions to exactly `mode`.
#[cfg(unix)]
fn confine(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

/// Non-Unix hosts have no socket to confine, so there is nothing to tighten.
#[cfg(not(unix))]
fn confine(_path: &Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

/// What [`Sidecar::ensure`] did, so a caller can report a restart honestly
/// instead of describing every call as a start.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    /// The container was already there with a matching signature and running.
    Reuse,
    /// The container was already there with a matching signature and stopped.
    Resumption,
    /// The container was created, either because none existed or because the
    /// one that did no longer matched its signature.
    Creation,
}

/// One extension's container, supervised over the workspace's container daemon.
pub struct Sidecar {
    bridge: Arc<Bridge>,
}

impl Sidecar {
    /// Binds the sidecar to the workspace's container daemon.
    #[must_use]
    pub fn new(bridge: Arc<Bridge>) -> Self {
        Self { bridge }
    }

    /// Brings the extension's container to the state `spec` describes.
    ///
    /// Idempotent: a matching container is reused, a stopped one is started, a
    /// container whose signature differs is removed and recreated, and an
    /// absent one is created. The signature comparison is what stops an
    /// extension from carrying on inside a container built for a wider grant.
    ///
    /// # Errors
    /// Returns a host failure from the container daemon, including the failure
    /// to remove the container that no longer matches.
    pub fn ensure(&self, spec: &SidecarSpec) -> Result<Outcome, HostError> {
        ensure_transaction(|| {
            if let Some(outcome) = self.reuse(spec)? {
                return Ok(outcome);
            }
            let client = self.bridge.client();
            self.bridge
                .wait(client.containers().create(&spec.request(), Some(spec.container())))
                .map_err(|error| failure(&error))?;
            self.start(spec.container())?;
            Ok(Outcome::Creation)
        })
    }

    /// Reuses the existing container when its signature still matches, and
    /// removes it when it does not.
    ///
    /// Returns `None` when the caller has to create one.
    fn reuse(&self, spec: &SidecarSpec) -> Result<Option<Outcome>, HostError> {
        let client = self.bridge.client();
        let existing = self.bridge.wait(client.containers().inspect(spec.container()));
        let container = match existing {
            Ok(container) => container,
            Err(error) => return absence(&error),
        };
        if container.config.labels.get(SIGNATURE_LABEL) != Some(&spec.signature())
            || !image_matches(spec, &container.details.metadata.image)
            || !sandbox_matches(spec, &container.host_config)
            || !mounts_match(spec, &container.details.metadata.mounts)
            || !process_matches(spec, &container.path, &container.args)
            || !environment_matches(spec, &container.config.env)
            || container.config.user != spec.image.user
            || !lifecycle_matches(&container.config, &container.network_settings)
            || container.execution != CreateExecution::Interpreted
        {
            let target = replacement_target(spec, &container.config.labels, &container.details.metadata.id)?;
            self.remove(target)?;
            return Ok(None);
        }
        if container.state.activity.restarting {
            return Err(HostError::Conflict(
                "the extension container is restarting; retry after its lifecycle transition finishes".to_owned(),
            ));
        }
        if container.state.activity.paused {
            self.unpause(spec.container())?;
            return Ok(Some(Outcome::Resumption));
        }
        if container.state.activity.running {
            return Ok(Some(Outcome::Reuse));
        }
        self.start(spec.container())?;
        Ok(Some(Outcome::Resumption))
    }

    /// Removes only the container created for this exact recorded
    /// specification. A container occupying Husklet's canonical name with any
    /// other signature is foreign state and is left untouched.
    pub fn remove_owned(&self, spec: &SidecarSpec) -> Result<(), HostError> {
        let client = self.bridge.client();
        let inspected = self.bridge.wait(client.containers().inspect(spec.container()));
        let container = match inspected {
            Ok(container) => container,
            Err(error) => return absence(&error).map(|_| ()),
        };
        let actual = container.config.labels.get(SIGNATURE_LABEL).map(String::as_str);
        let target = removal_target(&spec.signature(), actual, &container.details.metadata.id)?;
        if container.state.activity.running {
            self.stop(target)?;
        }
        self.remove(target)
    }

    /// Starts an extension container by name.
    ///
    /// # Errors
    /// Returns `HostError::Absent` when no such container exists.
    pub fn start(&self, container: &str) -> Result<(), HostError> {
        let client = self.bridge.client();
        self.bridge
            .wait(client.containers().start(container))
            .map_err(|error| failure(&error))
    }

    /// Resumes a paused extension container before reporting it available.
    fn unpause(&self, container: &str) -> Result<(), HostError> {
        let client = self.bridge.client();
        self.bridge
            .wait(client.containers().unpause(container))
            .map_err(|error| failure(&error))
    }

    /// Stops an extension container, giving its process a moment first.
    ///
    /// A container that is already gone is not an error: the caller wanted it
    /// down, and it is.
    ///
    /// # Errors
    /// Returns a host failure from the container daemon.
    pub fn stop(&self, container: &str) -> Result<(), HostError> {
        let client = self.bridge.client();
        let stopped = self
            .bridge
            .wait(client.containers().stop(container, Some(STOP_SECONDS)));
        match stopped {
            Ok(()) => Ok(()),
            Err(error) => absence(&error).map(|_| ()),
        }
    }

    /// Stops only the container generation created from `spec`.
    ///
    /// Shutdown can finish after an updated extension has recreated the stable
    /// name. The signature selects ownership and the immutable inspected id is
    /// the stop target, so delayed cleanup cannot stop that replacement.
    pub fn stop_owned(&self, spec: &SidecarSpec) -> Result<(), HostError> {
        let client = self.bridge.client();
        let container = match self.bridge.wait(client.containers().inspect(spec.container())) {
            Ok(container) => container,
            Err(error) => return absence(&error).map(|_| ()),
        };
        let actual = container.config.labels.get(SIGNATURE_LABEL).map(String::as_str);
        let Some(target) = stop_target(&spec.signature(), actual, &container.details.metadata.id) else {
            return Ok(());
        };
        self.stop(target)
    }

    /// Removes an extension container, forcing it down if it is still running.
    ///
    /// Forcing is right here and wrong in the extension-facing control port:
    /// this is the host retiring a container it owns, not an extension killing
    /// something a person is using.
    ///
    /// # Errors
    /// Returns a host failure from the container daemon.
    pub fn remove(&self, container: &str) -> Result<(), HostError> {
        let client = self.bridge.client();
        let removed = self.bridge.wait(client.containers().remove(container, true, false));
        match removed {
            Ok(()) => Ok(()),
            Err(error) => absence(&error).map(|_| ()),
        }
    }
}

fn ensure_transaction<T>(work: impl FnOnce() -> T) -> T {
    let _transaction = ENSURE.lock().unwrap_or_else(PoisonError::into_inner);
    work()
}

fn image_matches(spec: &SidecarSpec, image: &str) -> bool {
    image == spec.image.digest
}

fn sandbox_matches(spec: &SidecarSpec, actual: &hl_daemon::api::HostInspection) -> bool {
    let expected = spec.host();
    actual.memory == expected.memory
        && actual.nano_cpus == expected.nano_cpus
        && actual.pids_limit == expected.pids_limit
        && actual.network_mode == expected.network_mode
        && actual.extra_hosts == expected.extra_hosts
        && actual.dns == expected.dns
        && actual.dns_options == expected.dns_options
        && actual.dns_search == expected.dns_search
        && actual.auto_remove == expected.auto_remove
        && actual.readonly_rootfs == expected.readonly_rootfs
        && matches!(actual.restart_policy.name.as_str(), "" | "no")
        && actual.restart_policy.maximum_retry_count == 0
}

fn mounts_match(spec: &SidecarSpec, actual: &[hl_daemon::api::MountPoint]) -> bool {
    let expected = [(spec.socket(), SOCKET_TARGET), (spec.data(), DATA_TARGET)];
    actual.len() == expected.len()
        && expected.iter().all(|(source, destination)| {
            actual.iter().any(|mount| {
                mount.kind == "bind"
                    && mount.source == source.to_string_lossy()
                    && mount.destination == *destination
                    && mount.read_write
            })
        })
}

fn process_matches(spec: &SidecarSpec, program: &str, arguments: &[String]) -> bool {
    let (expected_program, entrypoint_arguments) = spec.entrypoint.split_first().map_or_else(
        || {
            spec.command
                .split_first()
                .map_or(("", &[][..]), |(program, args)| (program.as_str(), args))
        },
        |(program, args)| (program.as_str(), args),
    );
    let expected_arguments = entrypoint_arguments.iter().chain(
        (!spec.entrypoint.is_empty())
            .then_some(spec.command.as_slice())
            .into_iter()
            .flatten(),
    );
    program == expected_program
        && arguments
            .iter()
            .map(String::as_str)
            .eq(expected_arguments.map(String::as_str))
}

fn lifecycle_matches(config: &hl_daemon::api::ContainerConfig, network: &hl_daemon::api::NetworkSettings) -> bool {
    config.exposed_ports.is_empty()
        && config.stop_signal == "SIGTERM"
        && config.stop_timeout == 10
        && network.ports.is_empty()
        && network.networks.is_empty()
}

fn environment_matches(spec: &SidecarSpec, actual: &[String]) -> bool {
    let expected = spec.request().env.unwrap_or_default();
    actual.len() == expected.len() && expected.iter().all(|value| actual.contains(value))
}

fn replacement_target<'a>(
    spec: &SidecarSpec,
    labels: &BTreeMap<String, String>,
    id: &'a str,
) -> Result<&'a str, HostError> {
    let owner = spec.name.trim_start_matches(NAME_PREFIX);
    let existing = labels
        .get(GENERATION_LABEL)
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    if !id.is_empty() && labels.get(NAME_LABEL).map(String::as_str) == Some(owner) && existing <= spec.generation {
        return Ok(id);
    }
    Err(HostError::Conflict(
        "a newer or foreign extension generation occupies the sidecar name".to_owned(),
    ))
}

fn removal_target<'a>(expected: &str, actual: Option<&str>, id: &'a str) -> Result<&'a str, HostError> {
    if !id.is_empty() && actual == Some(expected) {
        return Ok(id);
    }
    Err(HostError::Conflict(
        "the extension container name is occupied by a container Husklet does not own".to_owned(),
    ))
}

fn stop_target<'a>(expected: &str, actual: Option<&str>, id: &'a str) -> Option<&'a str> {
    (!id.is_empty() && actual == Some(expected)).then_some(id)
}

/// Turns a "no such container" into an absence and anything else into a failure.
fn absence(error: &hl_client::Error) -> Result<Option<Outcome>, HostError> {
    match failure(error) {
        HostError::Absent(_) => Ok(None),
        other => Err(other),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::os::unix::net::UnixListener;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    use super::{
        DATA_TARGET, DATA_VARIABLE, GENERATION_LABEL, Image, NAME_LABEL, NODE_OPTIONS, SIGNATURE_LABEL, SOCKET_TARGET,
        SOCKET_VARIABLE, Sidecar, SidecarSpec, ensure_transaction, image_matches, removal_target, replacement_target,
        stop_target,
    };
    use hl_extension::{Capability, ExtensionName, Grant, Manifest, Resources};

    fn manifest(capabilities: &[Capability], resources: Resources) -> Manifest {
        Manifest {
            containers: hl_extension::ContainerGrant::default(),
            images: hl_extension::ImageGrant::default(),
            networks: hl_extension::NetworkGrant::default(),
            volumes: hl_extension::VolumeGrant::default(),
            name: ExtensionName::new("sample").expect("name"),
            display_name: "Sample".to_owned(),
            version: "1.0.0".to_owned(),
            protocol: hl_extension::PROTOCOL,
            capabilities: Grant::new(capabilities.iter().copied()),
            entrypoint: None,
            activation: hl_extension::Activation::default(),
            interface: None,
            pane_providers: Vec::new(),
            resources,
            filesystem: hl_extension::FilesystemGrant::default(),
            workspace_environment: hl_extension::WorkspaceEnvironmentGrant::default(),
        }
    }

    fn image() -> Image {
        Image {
            reference: "extension:1".to_owned(),
            digest: "sha256:aaaa".to_owned(),
            entrypoint: vec!["/usr/bin/extension".to_owned()],
            command: vec!["--serve".to_owned()],
            user: "1000:1000".to_owned(),
        }
    }

    fn spec() -> SidecarSpec {
        let manifest = manifest(&[Capability::ContainerRead], Resources::default());
        SidecarSpec::new(
            &manifest,
            &manifest.capabilities,
            &image(),
            "/run/sample/extension.sock",
        )
    }

    #[test]
    fn an_unchanged_specification_signs_the_same_every_time() {
        assert_eq!(spec().signature(), spec().signature());
    }

    #[test]
    fn a_matching_public_signature_cannot_reuse_a_different_image() {
        let wanted = spec();
        assert!(image_matches(&wanted, "sha256:aaaa"));
        assert!(!image_matches(&wanted, "sha256:forged"));
    }

    #[test]
    fn removal_targets_the_inspected_id_only_for_the_exact_signature() {
        assert_eq!(removal_target("ours", Some("ours"), "immutable-id"), Ok("immutable-id"));
        assert!(removal_target("ours", Some("foreign"), "foreign-id").is_err());
        assert!(removal_target("ours", None, "unlabelled-id").is_err());
        assert!(removal_target("ours", Some("ours"), "").is_err());
    }

    #[test]
    fn delayed_stop_targets_only_the_inspected_owned_generation() {
        assert_eq!(stop_target("ours", Some("ours"), "old-id"), Some("old-id"));
        assert_eq!(stop_target("ours", Some("replacement"), "replacement-id"), None);
        assert_eq!(stop_target("ours", None, "unrelated-id"), None);
        assert_eq!(stop_target("ours", Some("ours"), ""), None);
    }

    #[test]
    fn concurrent_ensure_transactions_never_overlap() {
        let entered = Arc::new(Barrier::new(3));
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let entered = Arc::clone(&entered);
            let active = Arc::clone(&active);
            let maximum = Arc::clone(&maximum);
            workers.push(std::thread::spawn(move || {
                entered.wait();
                ensure_transaction(|| {
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    maximum.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(20));
                    active.fetch_sub(1, Ordering::SeqCst);
                });
            }));
        }
        entered.wait();
        for worker in workers {
            worker.join().expect("ensure worker");
        }
        assert_eq!(maximum.load(Ordering::SeqCst), 1, "two ensure transactions overlapped");
    }

    #[test]
    fn stale_ensure_cannot_replace_a_newer_or_foreign_generation() {
        let old = spec().generation(10);
        let newer = spec().generation(20);
        let mut newer_labels = newer.labels();
        assert!(replacement_target(&old, &newer_labels, "new-id").is_err());
        assert_eq!(replacement_target(&newer, &old.labels(), "old-id"), Ok("old-id"));
        let mut same_generation_old_sandbox = newer.labels();
        same_generation_old_sandbox.insert(SIGNATURE_LABEL.to_owned(), "old-sandbox-signature".to_owned());
        assert_eq!(
            replacement_target(&newer, &same_generation_old_sandbox, "same-installation-id"),
            Ok("same-installation-id"),
            "a host sandbox revision must replace its own installation generation"
        );
        newer_labels.remove(NAME_LABEL);
        assert!(replacement_target(&newer, &newer_labels, "foreign-id").is_err());
        assert!(replacement_target(&newer, &old.labels(), "").is_err());
        assert_eq!(newer.labels().get(GENERATION_LABEL).map(String::as_str), Some("20"));
        assert_ne!(
            old.signature(),
            newer.signature(),
            "generation belongs to the signed identity"
        );
    }

    #[test]
    fn simultaneous_ensures_coalesce_over_the_real_unix_transport() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let socket = temporary.path().join("docker.sock");
        let listener = UnixListener::bind(&socket).expect("mock Docker socket");
        listener.set_nonblocking(true).expect("bounded mock listener");
        let wanted = spec().generation(42);
        let signature = wanted.signature();
        let served = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for step in 0..4 {
                let mut stream = accept_bounded(&listener);
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .expect("read timeout");
                requests.push(read_request(&mut stream).expect("Docker request"));
                match step {
                    0 => respond_close(&mut stream, "404 Not Found", br#"{"message":"absent"}"#),
                    1 => respond_close(&mut stream, "201 Created", br#"{"Id":"winning-id","Warnings":[]}"#),
                    2 => respond_close(&mut stream, "204 No Content", &[]),
                    3 => {
                        let labels = format!(
                            r#"{{"{SIGNATURE_LABEL}":"{signature}","{NAME_LABEL}":"sample","{GENERATION_LABEL}":"42"}}"#
                        );
                        respond_close(&mut stream, "200 OK", inspection("winning-id", &labels).as_bytes());
                    }
                    _ => unreachable!(),
                }
            }
            requests
        });
        let gate = Arc::new(Barrier::new(3));
        let mut callers = Vec::new();
        for _ in 0..2 {
            let gate = Arc::clone(&gate);
            let socket = socket.clone();
            let wanted = wanted.clone();
            callers.push(std::thread::spawn(move || {
                let bridge = Arc::new(super::super::Bridge::new(socket).expect("bridge"));
                gate.wait();
                Sidecar::new(bridge).ensure(&wanted)
            }));
        }
        gate.wait();
        let mut outcomes = callers
            .into_iter()
            .map(|caller| caller.join().expect("caller").expect("ensure"))
            .collect::<Vec<_>>();
        outcomes.sort_by_key(|outcome| format!("{outcome:?}"));

        assert_eq!(outcomes, vec![super::Outcome::Creation, super::Outcome::Reuse]);
        assert_eq!(
            served.join().expect("daemon joined"),
            vec![
                "GET /v1.43/containers/extension%2Dsample/json?size=false",
                "POST /v1.43/containers/create?name=extension%2Dsample",
                "POST /v1.43/containers/extension%2Dsample/start",
                "GET /v1.43/containers/extension%2Dsample/json?size=false",
            ],
            "one create/start wins and the other caller only reuses it"
        );
    }

    #[test]
    fn ensure_unpauses_a_matching_container_before_reporting_resumption() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let socket = temporary.path().join("docker.sock");
        let listener = UnixListener::bind(&socket).expect("mock Docker socket");
        let wanted = spec().generation(42);
        let signature = wanted.signature();
        let served = std::thread::spawn(move || {
            let labels =
                format!(r#"{{"{SIGNATURE_LABEL}":"{signature}","{NAME_LABEL}":"sample","{GENERATION_LABEL}":"42"}}"#);
            let paused = inspection("paused-id", &labels).replace("\"Paused\":false", "\"Paused\":true");
            let mut requests = Vec::new();
            for (status, body) in [("200 OK", paused.as_bytes()), ("204 No Content", &[])] {
                let mut stream = accept_bounded(&listener);
                requests.push(read_request(&mut stream).expect("Docker request"));
                respond_close(&mut stream, status, body);
            }
            requests
        });
        let bridge = Arc::new(super::super::Bridge::new(socket).expect("bridge"));

        assert_eq!(Sidecar::new(bridge).ensure(&wanted), Ok(super::Outcome::Resumption));
        assert_eq!(
            served.join().expect("daemon joined"),
            vec![
                "GET /v1.43/containers/extension%2Dsample/json?size=false",
                "POST /v1.43/containers/extension%2Dsample/unpause",
            ],
            "availability is reported only after the paused process is resumed"
        );
    }

    #[test]
    fn ensure_reports_restarting_without_sending_an_invalid_start() {
        for running in [false, true] {
            let temporary = tempfile::tempdir().expect("temporary directory");
            let socket = temporary.path().join("docker.sock");
            let listener = UnixListener::bind(&socket).expect("mock Docker socket");
            let wanted = spec().generation(42);
            let signature = wanted.signature();
            let served = std::thread::spawn(move || {
                let labels = format!(
                    r#"{{"{SIGNATURE_LABEL}":"{signature}","{NAME_LABEL}":"sample","{GENERATION_LABEL}":"42"}}"#
                );
                let restarting = inspection("restarting-id", &labels)
                    .replace("\"Restarting\":false", "\"Restarting\":true")
                    .replace("\"Running\":true", &format!("\"Running\":{running}"));
                let mut stream = accept_bounded(&listener);
                stream
                    .set_read_timeout(Some(Duration::from_millis(250)))
                    .expect("read timeout");
                let request = read_request(&mut stream).expect("inspect request");
                respond(&mut stream, "200 OK", restarting.as_bytes());
                assert!(
                    read_request(&mut stream).is_none(),
                    "a restarting container received another lifecycle request"
                );
                request
            });
            let bridge = Arc::new(super::super::Bridge::new(socket).expect("bridge"));

            let failure = Sidecar::new(bridge)
                .ensure(&wanted)
                .expect_err("restart is not readiness");

            assert!(failure.to_string().contains("restarting"));
            assert!(failure.to_string().contains("retry"));
            assert_eq!(
                served.join().expect("daemon joined"),
                "GET /v1.43/containers/extension%2Dsample/json?size=false"
            );
        }
    }

    #[test]
    fn ensure_replaces_a_matching_signature_backed_by_a_different_image() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let socket = temporary.path().join("docker.sock");
        let listener = UnixListener::bind(&socket).expect("mock Docker socket");
        let wanted = spec().generation(42);
        let signature = wanted.signature();
        let served = std::thread::spawn(move || {
            let labels =
                format!(r#"{{"{SIGNATURE_LABEL}":"{signature}","{NAME_LABEL}":"sample","{GENERATION_LABEL}":"42"}}"#);
            let forged = inspection("stale-id", &labels).replace("sha256:aaaa", "sha256:forged");
            let responses = [
                ("200 OK", forged.into_bytes()),
                ("204 No Content", Vec::new()),
                ("201 Created", br#"{"Id":"fresh-id","Warnings":[]}"#.to_vec()),
                ("204 No Content", Vec::new()),
            ];
            responses
                .into_iter()
                .map(|(status, body)| {
                    let mut stream = accept_bounded(&listener);
                    let request = read_request(&mut stream).expect("Docker request");
                    respond_close(&mut stream, status, &body);
                    request
                })
                .collect::<Vec<_>>()
        });
        let bridge = Arc::new(super::super::Bridge::new(socket).expect("bridge"));

        assert_eq!(Sidecar::new(bridge).ensure(&wanted), Ok(super::Outcome::Creation));
        assert_eq!(
            served.join().expect("daemon joined"),
            vec![
                "GET /v1.43/containers/extension%2Dsample/json?size=false",
                "DELETE /v1.43/containers/stale%2Did?force=true&v=false",
                "POST /v1.43/containers/create?name=extension%2Dsample",
                "POST /v1.43/containers/extension%2Dsample/start",
            ]
        );
    }

    #[test]
    fn ensure_replaces_a_matching_signature_with_mutated_resource_authority() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let socket = temporary.path().join("docker.sock");
        let listener = UnixListener::bind(&socket).expect("mock Docker socket");
        let wanted = spec().generation(42);
        let signature = wanted.signature();
        let served = std::thread::spawn(move || {
            let labels =
                format!(r#"{{"{SIGNATURE_LABEL}":"{signature}","{NAME_LABEL}":"sample","{GENERATION_LABEL}":"42"}}"#);
            let widened = inspection("widened-id", &labels).replace("\"Memory\":268435456", "\"Memory\":0");
            let responses = [
                ("200 OK", widened.into_bytes()),
                ("204 No Content", Vec::new()),
                ("201 Created", br#"{"Id":"fresh-id","Warnings":[]}"#.to_vec()),
                ("204 No Content", Vec::new()),
            ];
            responses
                .into_iter()
                .map(|(status, body)| {
                    let mut stream = accept_bounded(&listener);
                    let request = read_request(&mut stream).expect("Docker request");
                    respond_close(&mut stream, status, &body);
                    request
                })
                .collect::<Vec<_>>()
        });
        let bridge = Arc::new(super::super::Bridge::new(socket).expect("bridge"));

        assert_eq!(Sidecar::new(bridge).ensure(&wanted), Ok(super::Outcome::Creation));
        assert_eq!(
            served.join().expect("daemon joined"),
            vec![
                "GET /v1.43/containers/extension%2Dsample/json?size=false",
                "DELETE /v1.43/containers/widened%2Did?force=true&v=false",
                "POST /v1.43/containers/create?name=extension%2Dsample",
                "POST /v1.43/containers/extension%2Dsample/start",
            ]
        );
    }

    #[test]
    fn ensure_replaces_a_matching_signature_with_a_redirected_socket_mount() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let socket = temporary.path().join("docker.sock");
        let listener = UnixListener::bind(&socket).expect("mock Docker socket");
        let wanted = spec().generation(42);
        let signature = wanted.signature();
        let served = std::thread::spawn(move || {
            let labels =
                format!(r#"{{"{SIGNATURE_LABEL}":"{signature}","{NAME_LABEL}":"sample","{GENERATION_LABEL}":"42"}}"#);
            let redirected =
                inspection("redirected-id", &labels).replace("/run/sample/extension.sock", "/tmp/foreign.sock");
            let responses = [
                ("200 OK", redirected.into_bytes()),
                ("204 No Content", Vec::new()),
                ("201 Created", br#"{"Id":"fresh-id","Warnings":[]}"#.to_vec()),
                ("204 No Content", Vec::new()),
            ];
            responses
                .into_iter()
                .map(|(status, body)| {
                    let mut stream = accept_bounded(&listener);
                    let request = read_request(&mut stream).expect("Docker request");
                    respond_close(&mut stream, status, &body);
                    request
                })
                .collect::<Vec<_>>()
        });
        let bridge = Arc::new(super::super::Bridge::new(socket).expect("bridge"));

        assert_eq!(Sidecar::new(bridge).ensure(&wanted), Ok(super::Outcome::Creation));
        assert_eq!(
            served.join().expect("daemon joined"),
            vec![
                "GET /v1.43/containers/extension%2Dsample/json?size=false",
                "DELETE /v1.43/containers/redirected%2Did?force=true&v=false",
                "POST /v1.43/containers/create?name=extension%2Dsample",
                "POST /v1.43/containers/extension%2Dsample/start",
            ]
        );
    }

    #[test]
    fn ensure_replaces_a_matching_signature_with_redirected_process_environment() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let socket = temporary.path().join("docker.sock");
        let listener = UnixListener::bind(&socket).expect("mock Docker socket");
        let wanted = spec().generation(42);
        let signature = wanted.signature();
        let served = std::thread::spawn(move || {
            let labels =
                format!(r#"{{"{SIGNATURE_LABEL}":"{signature}","{NAME_LABEL}":"sample","{GENERATION_LABEL}":"42"}}"#);
            let redirected = inspection("redirected-env-id", &labels).replace(
                "HUSKLET_EXTENSION_SOCKET=/run/husklet/extension.sock",
                "HUSKLET_EXTENSION_SOCKET=/tmp/foreign.sock",
            );
            let responses = [
                ("200 OK", redirected.into_bytes()),
                ("204 No Content", Vec::new()),
                ("201 Created", br#"{"Id":"fresh-id","Warnings":[]}"#.to_vec()),
                ("204 No Content", Vec::new()),
            ];
            responses
                .into_iter()
                .map(|(status, body)| {
                    let mut stream = accept_bounded(&listener);
                    let request = read_request(&mut stream).expect("Docker request");
                    respond_close(&mut stream, status, &body);
                    request
                })
                .collect::<Vec<_>>()
        });
        let bridge = Arc::new(super::super::Bridge::new(socket).expect("bridge"));

        assert_eq!(Sidecar::new(bridge).ensure(&wanted), Ok(super::Outcome::Creation));
        assert_eq!(
            served.join().expect("daemon joined"),
            vec![
                "GET /v1.43/containers/extension%2Dsample/json?size=false",
                "DELETE /v1.43/containers/redirected%2Denv%2Did?force=true&v=false",
                "POST /v1.43/containers/create?name=extension%2Dsample",
                "POST /v1.43/containers/extension%2Dsample/start",
            ]
        );
    }

    #[test]
    fn ensure_replaces_a_matching_signature_running_as_a_different_user() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let socket = temporary.path().join("docker.sock");
        let listener = UnixListener::bind(&socket).expect("mock Docker socket");
        let wanted = spec().generation(42);
        let signature = wanted.signature();
        let served = std::thread::spawn(move || {
            let labels =
                format!(r#"{{"{SIGNATURE_LABEL}":"{signature}","{NAME_LABEL}":"sample","{GENERATION_LABEL}":"42"}}"#);
            let root = inspection("root-user-id", &labels).replace("\"User\":\"1000:1000\"", "\"User\":\"root\"");
            let responses = [
                ("200 OK", root.into_bytes()),
                ("204 No Content", Vec::new()),
                ("201 Created", br#"{"Id":"fresh-id","Warnings":[]}"#.to_vec()),
                ("204 No Content", Vec::new()),
            ];
            responses
                .into_iter()
                .map(|(status, body)| {
                    let mut stream = accept_bounded(&listener);
                    let request = read_request(&mut stream).expect("Docker request");
                    respond_close(&mut stream, status, &body);
                    request
                })
                .collect::<Vec<_>>()
        });
        let bridge = Arc::new(super::super::Bridge::new(socket).expect("bridge"));

        assert_eq!(Sidecar::new(bridge).ensure(&wanted), Ok(super::Outcome::Creation));
        assert_eq!(
            served.join().expect("daemon joined"),
            vec![
                "GET /v1.43/containers/extension%2Dsample/json?size=false",
                "DELETE /v1.43/containers/root%2Duser%2Did?force=true&v=false",
                "POST /v1.43/containers/create?name=extension%2Dsample",
                "POST /v1.43/containers/extension%2Dsample/start",
            ]
        );
    }

    #[test]
    fn ensure_replaces_a_matching_signature_running_a_different_program() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let socket = temporary.path().join("docker.sock");
        let listener = UnixListener::bind(&socket).expect("mock Docker socket");
        let wanted = spec().generation(42);
        let signature = wanted.signature();
        let served = std::thread::spawn(move || {
            let labels =
                format!(r#"{{"{SIGNATURE_LABEL}":"{signature}","{NAME_LABEL}":"sample","{GENERATION_LABEL}":"42"}}"#);
            let redirected = inspection("redirected-id", &labels).replace("/usr/bin/extension", "/bin/sh");
            let responses = [
                ("200 OK", redirected.into_bytes()),
                ("204 No Content", Vec::new()),
                ("201 Created", br#"{"Id":"fresh-id","Warnings":[]}"#.to_vec()),
                ("204 No Content", Vec::new()),
            ];
            responses
                .into_iter()
                .map(|(status, body)| {
                    let mut stream = accept_bounded(&listener);
                    let request = read_request(&mut stream).expect("Docker request");
                    respond_close(&mut stream, status, &body);
                    request
                })
                .collect::<Vec<_>>()
        });
        let bridge = Arc::new(super::super::Bridge::new(socket).expect("bridge"));

        assert_eq!(Sidecar::new(bridge).ensure(&wanted), Ok(super::Outcome::Creation));
        assert_eq!(
            served.join().expect("daemon joined"),
            vec![
                "GET /v1.43/containers/extension%2Dsample/json?size=false",
                "DELETE /v1.43/containers/redirected%2Did?force=true&v=false",
                "POST /v1.43/containers/create?name=extension%2Dsample",
                "POST /v1.43/containers/extension%2Dsample/start",
            ]
        );
    }

    #[test]
    fn ensure_replaces_a_matching_signature_with_a_destructive_stop_signal() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let socket = temporary.path().join("docker.sock");
        let listener = UnixListener::bind(&socket).expect("mock Docker socket");
        let wanted = spec().generation(42);
        let signature = wanted.signature();
        let served = std::thread::spawn(move || {
            let labels =
                format!(r#"{{"{SIGNATURE_LABEL}":"{signature}","{NAME_LABEL}":"sample","{GENERATION_LABEL}":"42"}}"#);
            let destructive = inspection("destructive-id", &labels).replace("SIGTERM", "SIGKILL");
            let responses = [
                ("200 OK", destructive.into_bytes()),
                ("204 No Content", Vec::new()),
                ("201 Created", br#"{"Id":"fresh-id","Warnings":[]}"#.to_vec()),
                ("204 No Content", Vec::new()),
            ];
            responses
                .into_iter()
                .map(|(status, body)| {
                    let mut stream = accept_bounded(&listener);
                    let request = read_request(&mut stream).expect("Docker request");
                    respond_close(&mut stream, status, &body);
                    request
                })
                .collect::<Vec<_>>()
        });
        let bridge = Arc::new(super::super::Bridge::new(socket).expect("bridge"));

        assert_eq!(Sidecar::new(bridge).ensure(&wanted), Ok(super::Outcome::Creation));
        assert_eq!(
            served.join().expect("daemon joined"),
            vec![
                "GET /v1.43/containers/extension%2Dsample/json?size=false",
                "DELETE /v1.43/containers/destructive%2Did?force=true&v=false",
                "POST /v1.43/containers/create?name=extension%2Dsample",
                "POST /v1.43/containers/extension%2Dsample/start",
            ]
        );
    }

    #[test]
    fn ensure_replaces_a_matching_signature_using_the_wrong_execution_backend() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let socket = temporary.path().join("docker.sock");
        let listener = UnixListener::bind(&socket).expect("mock Docker socket");
        let wanted = spec().generation(42);
        let signature = wanted.signature();
        let served = std::thread::spawn(move || {
            let labels =
                format!(r#"{{"{SIGNATURE_LABEL}":"{signature}","{NAME_LABEL}":"sample","{GENERATION_LABEL}":"42"}}"#);
            let native = inspection("native-id", &labels).replace(
                "\"HuskletExecution\":\"interpreted\"",
                "\"HuskletExecution\":\"native\"",
            );
            let responses = [
                ("200 OK", native.into_bytes()),
                ("204 No Content", Vec::new()),
                ("201 Created", br#"{"Id":"fresh-id","Warnings":[]}"#.to_vec()),
                ("204 No Content", Vec::new()),
            ];
            responses
                .into_iter()
                .map(|(status, body)| {
                    let mut stream = accept_bounded(&listener);
                    let request = read_request(&mut stream).expect("Docker request");
                    respond_close(&mut stream, status, &body);
                    request
                })
                .collect::<Vec<_>>()
        });
        let bridge = Arc::new(super::super::Bridge::new(socket).expect("bridge"));

        assert_eq!(Sidecar::new(bridge).ensure(&wanted), Ok(super::Outcome::Creation));
        assert_eq!(
            served.join().expect("daemon joined"),
            vec![
                "GET /v1.43/containers/extension%2Dsample/json?size=false",
                "DELETE /v1.43/containers/native%2Did?force=true&v=false",
                "POST /v1.43/containers/create?name=extension%2Dsample",
                "POST /v1.43/containers/extension%2Dsample/start",
            ]
        );
    }

    #[test]
    fn owned_stop_uses_real_transport_and_never_addresses_a_replacement_name() {
        for (actual, id, expected_requests) in [
            (Some(spec().signature()), "immutable-old-id", 2),
            (Some("replacement-signature".to_owned()), "replacement-id", 1),
            (None, "unrelated-id", 1),
            (Some(spec().signature()), "", 1),
        ] {
            let temporary = tempfile::tempdir().expect("temporary directory");
            let socket = temporary.path().join("docker.sock");
            let listener = UnixListener::bind(&socket).expect("mock Docker socket");
            let signature = actual.clone();
            let id = id.to_owned();
            let served = std::thread::spawn(move || serve_stop(listener, signature.as_deref(), &id, expected_requests));
            let bridge = Arc::new(super::super::Bridge::new(socket).expect("bridge"));

            Sidecar::new(bridge).stop_owned(&spec()).expect("bounded stop");

            let requests = served.join().expect("mock joined");
            assert_eq!(requests[0], "GET /v1.43/containers/extension%2Dsample/json?size=false");
            if expected_requests == 2 {
                assert_eq!(requests[1], "POST /v1.43/containers/immutable%2Dold%2Did/stop?t=5");
            } else {
                assert_eq!(requests.len(), 1, "foreign/replacement generation received a stop");
            }
        }
    }

    #[test]
    fn owned_stop_bounds_an_inspection_failure_without_sending_stop() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let socket = temporary.path().join("docker.sock");
        let listener = UnixListener::bind(&socket).expect("mock Docker socket");
        let served = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("client connected");
            stream
                .set_read_timeout(Some(Duration::from_millis(250)))
                .expect("read timeout");
            let request = read_request(&mut stream).expect("inspect request");
            respond(
                &mut stream,
                "500 Internal Server Error",
                br#"{"message":"inspection failed"}"#,
            );
            assert!(
                read_request(&mut stream).is_none(),
                "failure must not fall through to stop"
            );
            request
        });
        let bridge = Arc::new(super::super::Bridge::new(socket).expect("bridge"));

        let failure = Sidecar::new(bridge)
            .stop_owned(&spec())
            .expect_err("inspection failure is reported");

        assert!(failure.to_string().contains("inspection failed"));
        assert_eq!(
            served.join().expect("mock joined"),
            "GET /v1.43/containers/extension%2Dsample/json?size=false"
        );
    }

    #[test]
    fn owned_removal_stops_then_removes_only_the_immutable_generation() {
        for (actual, id, expected_requests) in [
            (Some(spec().signature()), "immutable-old-id", 3),
            (Some("replacement-signature".to_owned()), "replacement-id", 1),
            (None, "unrelated-id", 1),
            (Some(spec().signature()), "", 1),
        ] {
            let temporary = tempfile::tempdir().expect("temporary directory");
            let socket = temporary.path().join("docker.sock");
            let listener = UnixListener::bind(&socket).expect("mock Docker socket");
            let signature = actual.clone();
            let id = id.to_owned();
            let served = std::thread::spawn(move || serve_stop(listener, signature.as_deref(), &id, expected_requests));
            let bridge = Arc::new(super::super::Bridge::new(socket).expect("bridge"));

            let result = Sidecar::new(bridge).remove_owned(&spec());

            let requests = served.join().expect("mock joined");
            assert_eq!(requests[0], "GET /v1.43/containers/extension%2Dsample/json?size=false");
            if expected_requests == 3 {
                result.expect("owned removal");
                assert_eq!(requests[1], "POST /v1.43/containers/immutable%2Dold%2Did/stop?t=5");
                assert_eq!(
                    requests[2],
                    "DELETE /v1.43/containers/immutable%2Dold%2Did?force=true&v=false"
                );
            } else {
                assert!(result.is_err(), "foreign identity must be visible as a refusal");
                assert_eq!(requests.len(), 1, "foreign/replacement generation was mutated");
            }
        }
    }

    #[test]
    fn owned_removal_reports_stop_failure_and_never_falls_through_to_remove() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let socket = temporary.path().join("docker.sock");
        let listener = UnixListener::bind(&socket).expect("mock Docker socket");
        let signature = spec().signature();
        let served = std::thread::spawn(move || serve_stop(listener, Some(&signature), "immutable-old-id", 4));
        let bridge = Arc::new(super::super::Bridge::new(socket).expect("bridge"));

        let failure = Sidecar::new(bridge)
            .remove_owned(&spec())
            .expect_err("stop failure is visible");

        assert!(failure.to_string().contains("stop failed"));
        let requests = served.join().expect("mock joined");
        assert_eq!(requests.len(), 2, "a failed stop cannot fall through to remove");
    }

    fn serve_stop(listener: UnixListener, signature: Option<&str>, id: &str, expected: usize) -> Vec<String> {
        let (mut stream, _) = listener.accept().expect("client connected");
        stream
            .set_read_timeout(Some(Duration::from_millis(250)))
            .expect("read timeout");
        let mut requests = vec![read_request(&mut stream).expect("inspect request")];
        let labels = signature.map_or_else(
            || "{}".to_owned(),
            |value| format!(r#"{{"{SIGNATURE_LABEL}":"{value}"}}"#),
        );
        let body = inspection(id, &labels);
        respond(&mut stream, "200 OK", body.as_bytes());
        if expected >= 2 {
            requests.push(read_request(&mut stream).expect("stop request"));
            if expected == 4 {
                respond(
                    &mut stream,
                    "500 Internal Server Error",
                    br#"{"message":"stop failed"}"#,
                );
                assert!(
                    read_request(&mut stream).is_none(),
                    "failed stop fell through to removal"
                );
                return requests;
            }
            respond(&mut stream, "204 No Content", &[]);
            if expected == 3 {
                requests.push(read_request(&mut stream).expect("remove request"));
                respond(&mut stream, "204 No Content", &[]);
            }
        } else {
            assert!(
                read_request(&mut stream).is_none(),
                "an unowned generation was addressed"
            );
        }
        requests
    }

    fn read_request(stream: &mut std::os::unix::net::UnixStream) -> Option<String> {
        let mut bytes = Vec::new();
        let mut byte = [0_u8; 1];
        while !bytes.ends_with(b"\r\n\r\n") {
            if stream.read(&mut byte).ok()? == 0 {
                return None;
            }
            bytes.push(byte[0]);
        }
        let headers = String::from_utf8(bytes).ok()?;
        let length = headers
            .lines()
            .find_map(|line| {
                line.strip_prefix("content-length: ")
                    .or_else(|| line.strip_prefix("Content-Length: "))
            })
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = vec![0_u8; length];
        stream.read_exact(&mut body).ok()?;
        let line = headers.lines().next()?.to_owned();
        line.strip_suffix(" HTTP/1.1").map(str::to_owned)
    }

    fn accept_bounded(listener: &UnixListener) -> std::os::unix::net::UnixStream {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            match listener.accept() {
                Ok((stream, _)) => return stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("client did not connect inside the bound: {error}"),
            }
        }
    }

    fn inspection(id: &str, labels: &str) -> String {
        let mut inspection: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"Id":"{id}","Image":"sha256:aaaa","Mounts":[{{"Type":"bind","Name":"","Source":"/run/sample/extension.sock","Destination":"/run/husklet/extension.sock","Driver":"","Mode":"","RW":true,"Propagation":""}},{{"Type":"bind","Name":"","Source":"/run/sample/data","Destination":"/var/lib/husklet-extension","Driver":"","Mode":"","RW":true,"Propagation":""}}],"Path":"/usr/bin/extension","Args":["--serve"],"Name":"sidecar","Created":"","HuskletExecution":"interpreted","State":{{"Status":"running","Running":true,"Paused":false,"Restarting":false,"OOMKilled":false,"Dead":false,"Pid":1,"ExitCode":0,"Error":"","StartedAt":"","FinishedAt":""}},"RestartCount":0,"Config":{{"ExposedPorts":{{}},"Labels":{labels},"StopSignal":"SIGTERM","StopTimeout":10}},"HostConfig":{{"Memory":268435456,"NanoCpus":1000000000,"PidsLimit":128,"NetworkMode":"none","AutoRemove":false,"RestartPolicy":{{"Name":"no","MaximumRetryCount":0}}}},"NetworkSettings":{{"Ports":{{}},"Networks":{{}}}}}}"#
        ))
        .expect("valid inspection fixture");
        inspection["Config"]["Env"] = serde_json::json!([
            "HUSKLET_EXTENSION_DATA=/var/lib/husklet-extension",
            "HUSKLET_EXTENSION_SOCKET=/run/husklet/extension.sock",
            "NODE_OPTIONS=--jitless",
        ]);
        inspection["Config"]["User"] = serde_json::json!("1000:1000");
        inspection.to_string()
    }

    fn respond_close(stream: &mut std::os::unix::net::UnixStream, status: &str, body: &[u8]) {
        write!(
            stream,
            "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .expect("response headers");
        stream.write_all(body).expect("response body");
        stream.flush().expect("response flush");
    }

    fn respond(stream: &mut std::os::unix::net::UnixStream, status: &str, body: &[u8]) {
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .expect("response headers");
        stream.write_all(body).expect("response body");
        stream.flush().expect("response flush");
    }

    #[test]
    fn a_new_image_digest_forces_a_recreate() {
        let manifest = manifest(&[Capability::ContainerRead], Resources::default());
        let mut updated = image();
        updated.digest = "sha256:bbbb".to_owned();
        let other = SidecarSpec::new(
            &manifest,
            &manifest.capabilities,
            &updated,
            "/run/sample/extension.sock",
        );

        assert_ne!(spec().signature(), other.signature());
    }

    #[test]
    fn an_added_capability_forces_a_recreate() {
        let wider = manifest(
            &[Capability::ContainerRead, Capability::ContainerLifecycle],
            Resources::default(),
        );
        let other = SidecarSpec::new(&wider, &wider.capabilities, &image(), "/run/sample/extension.sock");

        assert_ne!(spec().signature(), other.signature());
    }

    #[test]
    fn a_changed_resource_limit_forces_a_recreate() {
        let heavier = manifest(
            &[Capability::ContainerRead],
            Resources {
                memory_mb: 512,
                ..Resources::default()
            },
        );
        let other = SidecarSpec::new(&heavier, &heavier.capabilities, &image(), "/run/sample/extension.sock");

        assert_ne!(spec().signature(), other.signature());
    }

    #[test]
    fn a_moved_socket_forces_a_recreate() {
        let manifest = manifest(&[Capability::ContainerRead], Resources::default());
        let other = SidecarSpec::new(&manifest, &manifest.capabilities, &image(), "/run/other/extension.sock");

        assert_ne!(spec().signature(), other.signature());
    }

    #[test]
    fn a_container_the_person_never_widened_keeps_its_signature() {
        // The manifest asks for more than the record granted; the container is
        // built from the grant, so restating the request changes nothing.
        let wider = manifest(
            &[Capability::ContainerRead, Capability::ContainerLifecycle],
            Resources::default(),
        );
        let narrow = SidecarSpec::new(
            &wider,
            &Grant::new([Capability::ContainerRead]),
            &image(),
            "/run/sample/extension.sock",
        );

        assert_eq!(narrow.signature(), spec().signature());
    }

    #[test]
    fn the_container_gets_only_its_socket_and_stable_private_data() {
        let request = spec().request();
        let host = request.host_config.expect("host settings");

        assert_eq!(
            request.env,
            Some(vec![
                format!("{SOCKET_VARIABLE}={SOCKET_TARGET}"),
                format!("{DATA_VARIABLE}={DATA_TARGET}"),
                format!("NODE_OPTIONS={NODE_OPTIONS}"),
            ])
        );
        assert_eq!(host.mounts.len(), 2);
        assert_eq!(host.mounts[0].source, "/run/sample/extension.sock");
        assert_eq!(host.mounts[0].target, SOCKET_TARGET);
        assert_eq!(host.mounts[1].kind, "bind");
        assert_eq!(host.mounts[1].source, "/run/sample/data");
        assert_eq!(host.mounts[1].target, DATA_TARGET);
        assert!(!host.mounts[1].read_only);
        assert_eq!(spec().data(), Path::new("/run/sample/data"));
        assert!(host.binds.is_empty());
        assert_eq!(host.network_mode, "none");
        assert!(
            !host.readonly_rootfs,
            "the native socket projection needs a writable image root"
        );
        assert!(host.tmpfs.is_empty(), "no unbounded writable filesystem is granted");
    }

    #[test]
    fn replacement_changes_the_sandbox_but_keeps_the_same_private_data_path() {
        let original = spec();
        let manifest = manifest(
            &[Capability::ContainerRead, Capability::ContainerLifecycle],
            Resources::default(),
        );
        let updated = SidecarSpec::new(
            &manifest,
            &manifest.capabilities,
            &Image {
                digest: "sha256:bbbb".into(),
                ..image()
            },
            "/run/sample/extension.sock",
        );

        assert_ne!(original.signature(), updated.signature());
        assert_eq!(original.data(), updated.data());
    }

    #[test]
    fn different_extensions_cannot_share_a_private_data_path() {
        let first = spec();
        let mut other_manifest = manifest(&[Capability::ContainerRead], Resources::default());
        other_manifest.name = ExtensionName::new("other").expect("name");
        let other = SidecarSpec::new(
            &other_manifest,
            &other_manifest.capabilities,
            &image(),
            "/run/other/extension.sock",
        );

        assert_ne!(first.data(), other.data());
    }

    #[test]
    fn the_container_runs_as_the_image_says_and_carries_its_signature() {
        let request = spec().request();

        assert_eq!(request.user.as_deref(), Some("1000:1000"), "never forced to root");
        assert_eq!(request.entrypoint, Some(vec!["/usr/bin/extension".to_owned()]));
        assert_eq!(request.cmd, Some(vec!["--serve".to_owned()]));
        assert_eq!(request.labels.get(SIGNATURE_LABEL), Some(&spec().signature()));
        assert_eq!(request.labels.get(NAME_LABEL).map(String::as_str), Some("sample"));
    }

    #[test]
    fn the_manifest_entrypoint_wins_over_the_image() {
        let mut declared = manifest(&[Capability::ContainerRead], Resources::default());
        declared.entrypoint = Some(vec!["/bin/own".to_owned()]);
        let spec = SidecarSpec::new(
            &declared,
            &declared.capabilities,
            &image(),
            "/run/sample/extension.sock",
        );

        assert_eq!(spec.request().entrypoint, Some(vec!["/bin/own".to_owned()]));
    }

    #[test]
    fn a_request_above_the_ceiling_still_gets_the_ceiling() {
        let greedy = manifest(
            &[Capability::ContainerRead],
            Resources {
                memory_mb: Resources::CEILING_MEMORY_MB * 8,
                cpus: Resources::CEILING_CPUS * 8,
                process_count: Resources::CEILING_PROCESS_COUNT * 8,
            },
        );
        let spec = SidecarSpec::new(&greedy, &greedy.capabilities, &image(), "/run/sample/extension.sock");
        let host = spec.request().host_config.expect("host settings");

        assert_eq!(spec.resources().memory_mb, Resources::CEILING_MEMORY_MB);
        assert_eq!(spec.resources().cpus, Resources::CEILING_CPUS);
        assert_eq!(spec.resources().process_count, Resources::CEILING_PROCESS_COUNT);
        assert_eq!(host.memory, i64::from(Resources::CEILING_MEMORY_MB) * 1024 * 1024);
        assert_eq!(host.pids_limit, Some(i64::from(Resources::CEILING_PROCESS_COUNT)));
    }

    #[cfg(unix)]
    #[test]
    fn the_socket_directory_is_private_and_the_mounted_inode_is_connectable() {
        use std::os::unix::fs::PermissionsExt as _;

        let temporary = tempfile::tempdir().expect("temporary directory");
        let socket = temporary.path().join("run/extension.sock");
        let manifest = manifest(&[Capability::ContainerRead], Resources::default());
        let spec = SidecarSpec::new(&manifest, &manifest.capabilities, &image(), &socket);

        spec.prepare().expect("prepared");
        let directory = std::fs::metadata(socket.parent().expect("directory")).expect("directory metadata");
        assert_eq!(directory.permissions().mode() & 0o777, 0o700);
        let data = socket.parent().expect("directory").join("data");
        assert!(data.is_dir());
        assert_eq!(
            std::fs::metadata(&data).expect("data metadata").permissions().mode() & 0o777,
            0o777
        );

        std::fs::write(&socket, b"").expect("socket placeholder");
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o666)).expect("widened");
        spec.prepare().expect("prepared again");
        let confined = std::fs::metadata(&socket).expect("socket metadata");
        assert_eq!(confined.permissions().mode() & 0o777, 0o666);
    }

    #[cfg(unix)]
    #[test]
    fn private_data_survives_replacement_and_uninstall_purges_only_its_directory() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let socket = temporary.path().join("extensions/sample/extension.sock");
        let original = SidecarSpec::new(
            &manifest(&[Capability::ContainerRead], Resources::default()),
            &Grant::new([Capability::ContainerRead]),
            &image(),
            &socket,
        );
        original.prepare().expect("first generation prepared");
        std::fs::write(original.data().join("index.db"), b"embeddings").expect("private data");

        let replacement = SidecarSpec::new(
            &manifest(
                &[Capability::ContainerRead, Capability::ContainerLifecycle],
                Resources::default(),
            ),
            &Grant::new([Capability::ContainerRead, Capability::ContainerLifecycle]),
            &Image {
                digest: "sha256:bbbb".into(),
                ..image()
            },
            &socket,
        );
        replacement.prepare().expect("replacement prepared");
        assert_eq!(
            std::fs::read(replacement.data().join("index.db")).expect("retained index"),
            b"embeddings"
        );

        let neighbour = temporary.path().join("extensions/other/data");
        std::fs::create_dir_all(&neighbour).expect("other extension data");
        std::fs::write(neighbour.join("database"), b"other").expect("other value");
        replacement.purge_data().expect("uninstall purge");
        assert!(!replacement.data().exists());
        assert_eq!(
            std::fs::read(neighbour.join("database")).expect("other retained"),
            b"other"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_link_cannot_redirect_private_data_preparation_or_uninstall() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let socket = temporary.path().join("extensions/sample/extension.sock");
        std::fs::create_dir_all(socket.parent().expect("parent")).expect("extension directory");
        let outside = temporary.path().join("outside");
        std::fs::create_dir(&outside).expect("outside");
        std::fs::write(outside.join("keep"), b"safe").expect("sentinel");
        std::os::unix::fs::symlink(&outside, socket.parent().expect("parent").join("data")).expect("link");
        let spec = SidecarSpec::new(
            &manifest(&[Capability::ContainerRead], Resources::default()),
            &Grant::new([Capability::ContainerRead]),
            &image(),
            &socket,
        );

        assert!(spec.prepare().is_err());
        assert!(spec.purge_data().is_err());
        assert_eq!(std::fs::read(outside.join("keep")).expect("sentinel retained"), b"safe");
    }
}
