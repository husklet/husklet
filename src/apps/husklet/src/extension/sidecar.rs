//! The container one extension runs in, and the supervision that keeps it there.
//!
//! An extension is a program someone else wrote, so the container it gets is
//! described entirely here rather than taken from its image: one environment
//! variable, one mount, no network, and the resource ceiling the protocol
//! already clamps to. Anything the image asks for beyond that is ignored.
//!
//! Building the specification and computing its signature are pure functions
//! over a [`Manifest`], a [`Grant`], an [`Image`], and a socket path. Nothing in
//! that path reaches a daemon, so the reuse-versus-recreate decision — the only
//! part where a mistake silently leaves an extension running under a grant it no
//! longer has — is tested without a container runtime.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use hl_client::model::{CreateContainer, DockerMount, HostConfig, InspectImage};
use hl_extension::port::HostError;
use hl_extension::{Grant, Manifest, Resources};

use super::{failure, Bridge};

/// The only environment variable an extension's container is given.
///
/// Everything else an extension needs it asks for over the socket, so the
/// environment cannot become an unaudited second channel into the sandbox.
pub const SOCKET_VARIABLE: &str = "HUSKLET_EXTENSION_SOCKET";

/// Where the host socket appears inside the container.
pub const SOCKET_TARGET: &str = "/run/husklet/extension.sock";

/// Prefix of every extension container name, so a workspace's extension
/// containers are recognizable without consulting a label.
pub const NAME_PREFIX: &str = "extension-";

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

/// Permissions on the socket itself. An extension's socket is its credential:
/// anyone who can connect to it holds that extension's whole grant.
#[cfg(unix)]
const SOCKET_MODE: u32 = 0o600;

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
    granted: Vec<String>,
    resources: Resources,
    socket: PathBuf,
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
        Self {
            name: format!("{NAME_PREFIX}{}", manifest.name),
            image: image.clone(),
            entrypoint,
            granted: granted
                .iter()
                .map(|capability| capability.as_str().to_owned())
                .collect(),
            resources: manifest.resources.clamp(),
            socket: socket.into(),
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
        Self::field(&mut value, &self.generation.to_string());
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
            env: Some(vec![format!("{SOCKET_VARIABLE}={SOCKET_TARGET}")]),
            user: Some(self.image.user.clone()).filter(|user| !user.is_empty()),
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

    /// The host-side settings: the socket and nothing else, no network, and the
    /// clamped limits expressed in the daemon's units.
    fn host(&self) -> HostConfig {
        HostConfig {
            mounts: vec![DockerMount {
                kind: "bind".to_owned(),
                source: self.socket.to_string_lossy().into_owned(),
                target: SOCKET_TARGET.to_owned(),
                ..DockerMount::default()
            }],
            memory: i64::from(self.resources.memory_mb) * 1024 * 1024,
            nano_cpus: i64::from(self.resources.cpus) * 1_000_000_000,
            pids_limit: Some(i64::from(self.resources.process_count)),
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
        match std::fs::symlink_metadata(&self.socket) {
            Ok(_) => confine(&self.socket, SOCKET_MODE),
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
        if container.config.labels.get(SIGNATURE_LABEL) != Some(&spec.signature()) {
            let target = replacement_target(spec, &container.config.labels, &container.details.metadata.id)?;
            self.remove(target)?;
            return Ok(None);
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
    if !id.is_empty() && labels.get(NAME_LABEL).map(String::as_str) == Some(owner) && existing < spec.generation {
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    use super::{
        ensure_transaction, removal_target, replacement_target, stop_target, Image, Sidecar, SidecarSpec,
        GENERATION_LABEL, NAME_LABEL, SIGNATURE_LABEL, SOCKET_TARGET, SOCKET_VARIABLE,
    };
    use hl_extension::{Capability, ExtensionName, Grant, Manifest, Resources};

    fn manifest(capabilities: &[Capability], resources: Resources) -> Manifest {
        Manifest {
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
            filesystem_roots: Vec::new(),
        }
    }

    fn image() -> Image {
        Image {
            reference: "extension:1".to_owned(),
            digest: "sha256:aaaa".to_owned(),
            entrypoint: vec!["/usr/bin/extension".to_owned()],
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
        format!(
            r#"{{"Id":"{id}","Image":"sha256:image","Mounts":[],"Path":"/extension","Args":[],"Name":"sidecar","Created":"","State":{{"Status":"running","Running":true,"Paused":false,"Restarting":false,"OOMKilled":false,"Dead":false,"Pid":1,"ExitCode":0,"Error":"","StartedAt":"","FinishedAt":""}},"RestartCount":0,"Config":{{"ExposedPorts":{{}},"Labels":{labels},"StopSignal":"SIGTERM","StopTimeout":10}},"HostConfig":{{"NetworkMode":"none","AutoRemove":false,"RestartPolicy":{{"Name":"no","MaximumRetryCount":0}}}},"NetworkSettings":{{"Ports":{{}},"Networks":{{}}}}}}"#
        )
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
            &[Capability::ContainerRead, Capability::ContainerControl],
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
            &[Capability::ContainerRead, Capability::ContainerControl],
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
    fn the_container_is_given_the_socket_and_nothing_else() {
        let request = spec().request();
        let host = request.host_config.expect("host settings");

        assert_eq!(request.env, Some(vec![format!("{SOCKET_VARIABLE}={SOCKET_TARGET}")]));
        assert_eq!(host.mounts.len(), 1);
        assert_eq!(host.mounts[0].source, "/run/sample/extension.sock");
        assert_eq!(host.mounts[0].target, SOCKET_TARGET);
        assert!(host.binds.is_empty());
        assert_eq!(host.network_mode, "none");
    }

    #[test]
    fn the_container_runs_as_the_image_says_and_carries_its_signature() {
        let request = spec().request();

        assert_eq!(request.user.as_deref(), Some("1000:1000"), "never forced to root");
        assert_eq!(request.entrypoint, Some(vec!["/usr/bin/extension".to_owned()]));
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
    fn the_socket_directory_is_owner_only_and_the_socket_is_private() {
        use std::os::unix::fs::PermissionsExt as _;

        let temporary = tempfile::tempdir().expect("temporary directory");
        let socket = temporary.path().join("run/extension.sock");
        let manifest = manifest(&[Capability::ContainerRead], Resources::default());
        let spec = SidecarSpec::new(&manifest, &manifest.capabilities, &image(), &socket);

        spec.prepare().expect("prepared");
        let directory = std::fs::metadata(socket.parent().expect("directory")).expect("directory metadata");
        assert_eq!(directory.permissions().mode() & 0o777, 0o700);

        std::fs::write(&socket, b"").expect("socket placeholder");
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o666)).expect("widened");
        spec.prepare().expect("prepared again");
        let confined = std::fs::metadata(&socket).expect("socket metadata");
        assert_eq!(confined.permissions().mode() & 0o777, 0o600);
    }
}
