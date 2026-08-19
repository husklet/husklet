use crate::{
    Error, Result,
    service::{OverlayConfig, ProcessConfig, Running, Runtime},
};
use async_trait::async_trait;
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Condvar, Mutex as StdMutex},
    time::Duration,
};

const CHECKPOINT_OBJECT: &str = "rust/image";
const CHECKPOINT_MANIFEST_MAGIC: &[u8; 8] = b"HLRUST01";

mod process;
mod spec;
use process::Process;
use spec::Spec;

/// Every domain freeze channel this runtime currently coordinates, keyed by the
/// process domain identity the guest processes are launched into.
type DomainChannels = StdMutex<HashMap<[u64; 2], hl_engine::composition::CheckpointChannel>>;

#[derive(Default)]
pub(crate) struct Engine {
    domains: Arc<DomainChannels>,
}

/// Keeps a coordinator's freeze channel published for its domain's exec sessions
/// and withdraws it when the coordinator process is released, so a session started
/// after the coordinator is gone cannot join a dead broker.
pub(super) struct DomainChannelEntry {
    domains: Arc<DomainChannels>,
    identity: [u64; 2],
}

impl Drop for DomainChannelEntry {
    fn drop(&mut self) {
        if let Ok(mut domains) = self.domains.lock() {
            domains.remove(&self.identity);
        }
    }
}

struct CheckpointTransport {
    image: Arc<dyn crate::CheckpointImage>,
}

impl CheckpointTransport {
    fn new(image: Arc<dyn crate::CheckpointImage>) -> Self {
        Self { image }
    }

    fn storage_error(error: &crate::CheckpointError) -> hl_engine::composition::CompositionError {
        if error.is_deadline() {
            hl_engine::composition::CompositionError::DeadlineExceeded
        } else if error.is_busy() {
            hl_engine::composition::CompositionError::TransactionBusy
        } else if error.publication_occurred() {
            hl_engine::composition::CompositionError::PublishedNotDurable
        } else {
            hl_engine::composition::CompositionError::RuntimeConstruction
        }
    }
}

impl hl_engine::composition::CheckpointSink for CheckpointTransport {
    fn replace(&self, bytes: &[u8]) -> std::result::Result<(), hl_engine::composition::CompositionError> {
        let deadline = std::time::Instant::now() + hl_engine::composition::DEFAULT_CHECKPOINT_TIMEOUT;
        let transaction = self
            .image
            .begin_until(deadline)
            .map_err(|error| Self::storage_error(&error))?;
        self.image
            .put_until(transaction, CHECKPOINT_OBJECT, bytes, deadline)
            .map_err(|_| hl_engine::composition::CompositionError::RuntimeConstruction)?;
        let mut manifest = Vec::with_capacity(16);
        manifest.extend_from_slice(CHECKPOINT_MANIFEST_MAGIC);
        manifest.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        self.image
            .commit_until(transaction, &manifest, deadline)
            .map_err(|_| hl_engine::composition::CompositionError::RuntimeConstruction)
    }

    fn begin_until(
        &self,
        deadline: std::time::Instant,
    ) -> std::result::Result<std::num::NonZeroU64, hl_engine::composition::CompositionError> {
        self.image
            .begin_until(deadline)
            .map_err(|error| Self::storage_error(&error))
    }

    fn put_until(
        &self,
        transaction: std::num::NonZeroU64,
        name: &str,
        bytes: &[u8],
        deadline: std::time::Instant,
    ) -> std::result::Result<(), hl_engine::composition::CompositionError> {
        self.image
            .put_until(transaction, name, bytes, deadline)
            .map_err(|error| Self::storage_error(&error))
    }

    fn abort_until(
        &self,
        transaction: std::num::NonZeroU64,
        deadline: std::time::Instant,
    ) -> std::result::Result<(), hl_engine::composition::CompositionError> {
        self.image
            .abort_until(transaction, deadline)
            .map_err(|error| Self::storage_error(&error))
    }

    fn commit_until(
        &self,
        transaction: std::num::NonZeroU64,
        manifest: &[u8],
        deadline: std::time::Instant,
    ) -> std::result::Result<(), hl_engine::composition::CompositionError> {
        self.image
            .commit_until(transaction, manifest, deadline)
            .map_err(|error| Self::storage_error(&error))
    }
}

impl hl_engine::composition::CheckpointSource for CheckpointTransport {
    fn read(&self, maximum: usize) -> std::result::Result<Vec<u8>, hl_engine::composition::CompositionError> {
        let manifest = self
            .image
            .get("MANIFEST")
            .map_err(|_| hl_engine::composition::CompositionError::RuntimeConstruction)?;
        if manifest.len() != 16 || &manifest[..8] != CHECKPOINT_MANIFEST_MAGIC {
            return Err(hl_engine::composition::CompositionError::RuntimeConstruction);
        }
        let length = u64::from_le_bytes(
            manifest[8..]
                .try_into()
                .map_err(|_| hl_engine::composition::CompositionError::RuntimeConstruction)?,
        );
        let length =
            usize::try_from(length).map_err(|_| hl_engine::composition::CompositionError::RuntimeConstruction)?;
        if length > maximum {
            return Err(hl_engine::composition::CompositionError::RuntimeConstruction);
        }
        let bytes = self
            .image
            .get(CHECKPOINT_OBJECT)
            .map_err(|_| hl_engine::composition::CompositionError::RuntimeConstruction)?;
        (bytes.len() == length)
            .then_some(bytes)
            .ok_or(hl_engine::composition::CompositionError::RuntimeConstruction)
    }

    fn get(&self, name: &str) -> std::result::Result<Vec<u8>, hl_engine::composition::CompositionError> {
        self.image
            .get(name)
            .map_err(|_| hl_engine::composition::CompositionError::RuntimeConstruction)
    }

    fn list(&self) -> std::result::Result<Vec<String>, hl_engine::composition::CompositionError> {
        self.image
            .list()
            .map_err(|_| hl_engine::composition::CompositionError::RuntimeConstruction)
    }

    fn get_until(
        &self,
        name: &str,
        deadline: std::time::Instant,
    ) -> std::result::Result<Vec<u8>, hl_engine::composition::CompositionError> {
        self.image
            .get_until(name, deadline)
            .map_err(|error| Self::storage_error(&error))
    }

    fn list_until(
        &self,
        deadline: std::time::Instant,
    ) -> std::result::Result<Vec<String>, hl_engine::composition::CompositionError> {
        self.image
            .list_until(deadline)
            .map_err(|error| Self::storage_error(&error))
    }
}

struct TerminalState {
    receiver: Option<tokio::sync::mpsc::Receiver<Vec<u8>>>,
    pending: VecDeque<u8>,
    closed: bool,
}

/// Container-owned adapter for the engine's host-terminal port.
///
/// The condition variable provides bounded cancellation independently of the
/// client input sender's lifetime. Tokio's bounded queues provide backpressure;
/// timed waits avoid holding a lock across a channel operation or busy-spinning.
struct TerminalChannel {
    state: StdMutex<TerminalState>,
    changed: Condvar,
    output: crate::service::LogSender,
}

struct OutputChannel {
    state: StdMutex<TerminalState>,
    changed: Condvar,
    output: crate::service::LogSender,
}

impl OutputChannel {
    fn new(receiver: Option<tokio::sync::mpsc::Receiver<Vec<u8>>>, output: crate::service::LogSender) -> Self {
        Self {
            state: StdMutex::new(TerminalState {
                receiver,
                pending: VecDeque::new(),
                closed: false,
            }),
            changed: Condvar::new(),
            output,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, TerminalState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl hl_engine::composition::StandardStreamPort for OutputChannel {
    fn read(&self, output: &mut [u8]) -> std::io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let mut state = self.lock();
        loop {
            if state.closed {
                return Ok(0);
            }
            if !state.pending.is_empty() {
                let length = output.len().min(state.pending.len());
                for destination in &mut output[..length] {
                    *destination = state.pending.pop_front().expect("bounded by pending length");
                }
                return Ok(length);
            }
            let received = match state.receiver.as_mut() {
                Some(receiver) => receiver.try_recv(),
                None => return Ok(0),
            };
            match received {
                Ok(bytes) => state.pending.extend(bytes),
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => state.receiver = None,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                    state = self
                        .changed
                        .wait_timeout(state, TerminalChannel::CANCELLATION_POLL)
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .0;
                }
            }
        }
    }

    fn write(&self, stream: hl_engine::composition::StandardStream, input: &[u8]) -> std::io::Result<usize> {
        if input.is_empty() {
            return Ok(0);
        }
        let length = input.len().min(crate::service::LOG_CHUNK_BYTES);
        let stream = match stream {
            hl_engine::composition::StandardStream::Stdout => crate::Stream::Stdout,
            hl_engine::composition::StandardStream::Stderr => crate::Stream::Stderr,
        };
        let mut chunk = crate::LogChunk {
            stream,
            bytes: input[..length].to_vec(),
        };
        loop {
            if self.lock().closed {
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
            match self.output.try_send(chunk) {
                Ok(()) => return Ok(length),
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    return Err(std::io::ErrorKind::BrokenPipe.into());
                }
                Err(tokio::sync::mpsc::error::TrySendError::Full(returned)) => {
                    chunk = returned;
                    std::thread::sleep(TerminalChannel::CANCELLATION_POLL);
                }
            }
        }
    }

    fn close(&self) {
        self.lock().closed = true;
        self.changed.notify_all();
    }
}

impl TerminalChannel {
    const CANCELLATION_POLL: Duration = Duration::from_millis(10);

    fn new(receiver: Option<tokio::sync::mpsc::Receiver<Vec<u8>>>, output: crate::service::LogSender) -> Self {
        Self {
            state: StdMutex::new(TerminalState {
                receiver,
                pending: VecDeque::new(),
                closed: false,
            }),
            changed: Condvar::new(),
            output,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, TerminalState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl hl_engine::composition::TerminalPort for TerminalChannel {
    fn read(&self, output: &mut [u8]) -> std::io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let mut state = self.lock();
        loop {
            if state.closed {
                return Ok(0);
            }
            if !state.pending.is_empty() {
                let length = output.len().min(state.pending.len());
                for destination in &mut output[..length] {
                    *destination = state.pending.pop_front().expect("bounded by pending length");
                }
                return Ok(length);
            }
            let received = match state.receiver.as_mut() {
                Some(receiver) => receiver.try_recv(),
                None => return Ok(0),
            };
            match received {
                Ok(bytes) => state.pending.extend(bytes),
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => state.receiver = None,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                    state = self
                        .changed
                        .wait_timeout(state, Self::CANCELLATION_POLL)
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .0;
                }
            }
        }
    }

    fn write(&self, input: &[u8]) -> std::io::Result<usize> {
        if input.is_empty() {
            return Ok(0);
        }
        let length = input.len().min(crate::service::LOG_CHUNK_BYTES);
        let mut chunk = crate::LogChunk {
            stream: crate::Stream::Stdout,
            bytes: input[..length].to_vec(),
        };
        loop {
            if self.lock().closed {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "terminal transport closed",
                ));
            }
            match self.output.try_send(chunk) {
                Ok(()) => return Ok(length),
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "container log receiver closed",
                    ));
                }
                Err(tokio::sync::mpsc::error::TrySendError::Full(returned)) => {
                    chunk = returned;
                    let state = self.lock();
                    if state.closed {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::BrokenPipe,
                            "terminal transport closed",
                        ));
                    }
                    drop(
                        self.changed
                            .wait_timeout(state, Self::CANCELLATION_POLL)
                            .unwrap_or_else(std::sync::PoisonError::into_inner),
                    );
                }
            }
        }
    }

    fn close(&self) {
        self.lock().closed = true;
        self.changed.notify_all();
    }
}

#[async_trait]
impl Runtime for Engine {
    fn validate_overlay(&self, overlay: &OverlayConfig) -> bool {
        overlay.lower.is_dir() && overlay.upper.is_dir() && overlay.work.is_dir()
    }

    async fn start(&self, mut config: ProcessConfig) -> Result<Arc<dyn Running>> {
        if !config.rootfs.is_dir() {
            return Err(Error::InvalidSpec(format!(
                "rootfs does not exist or is not a directory: {}",
                config.rootfs.display()
            )));
        }
        let spec = Spec::try_from(&config)?;
        let (sender, receiver) = crate::service::log_channel();
        let streams = match config.terminal {
            Some(size) => {
                let port = Arc::new(TerminalChannel::new(config.input.take(), sender));
                let terminal = hl_engine::composition::Terminal::new(port, size.rows(), size.columns())
                    .map_err(|_| Error::Runtime("terminal construction failed".into()))?;
                hl_engine::composition::StandardStreams::default().with_terminal(terminal)
            }
            None => hl_engine::composition::StandardStreams::default()
                .with_output(Arc::new(OutputChannel::new(config.input.take(), sender))),
        };
        let identity = spec.domain.identity();
        let role = config.checkpoint.take();
        // A member joins the coordinator's broker and trigger. Resolving the channel
        // before construction keeps the refusal on the launch boundary: a session that
        // cannot reach its domain's freeze must not start armed on a channel of its own.
        let member = match &role {
            Some(crate::service::CheckpointRole::DomainMember) => Some(
                self.domains
                    .lock()
                    .map_err(|_| Error::Runtime("checkpoint domain registry is poisoned".into()))?
                    .get(&identity)
                    .cloned()
                    .ok_or_else(|| {
                        Error::Runtime("process domain has no checkpoint coordinator to join".into())
                    })?,
            ),
            _ => None,
        };
        let checkpoint = match role {
            Some(crate::service::CheckpointRole::Coordinator(checkpoint)) => {
                Some(Arc::new(CheckpointTransport::new(checkpoint.image)))
            }
            Some(crate::service::CheckpointRole::DomainMember) | None => None,
        };
        let checkpointable = checkpoint.is_some();
        let engine = Arc::new(
            match (checkpoint, member) {
                (Some(transport), _) => hl_engine::runtime::Engine::with_checkpoint(
                    spec.isa,
                    spec.plan,
                    streams,
                    transport.clone(),
                    transport,
                ),
                (None, Some(channel)) => {
                    hl_engine::runtime::Engine::with_checkpoint_channel(spec.isa, spec.plan, streams, channel)
                }
                (None, None) => hl_engine::runtime::Engine::with_streams(spec.isa, spec.plan, streams),
            }
            .map_err(|error| Error::Runtime(format!("engine construction: {error:?}")))?,
        );
        let domain_channel = engine.checkpoint_channel().map(|channel| {
            if let Ok(mut domains) = self.domains.lock() {
                domains.insert(identity, channel);
            }
            DomainChannelEntry {
                domains: Arc::clone(&self.domains),
                identity,
            }
        });
        engine
            .start()
            .map_err(|error| Error::Runtime(format!("engine start: {error:?}")))?;

        Ok(Arc::new(Process {
            id: Process::next_id(),
            child: StdMutex::new(Some(engine)),
            logs: StdMutex::new(Some(receiver)),
            domain: spec.domain,
            checkpointable,
            domain_channel,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::{CheckpointTransport, Spec, TerminalChannel};
    use crate::CheckpointImage as _;
    use crate::service::{NetworkConfig, ProcessConfig};
    use hl_engine::composition::{CheckpointSink as _, CheckpointSource as _, TerminalPort as _};
    use std::collections::BTreeMap;
    use std::num::NonZeroU64;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Image(Mutex<BTreeMap<String, Vec<u8>>>);

    impl crate::CheckpointImage for Image {
        fn begin_until(&self, _: std::time::Instant) -> Result<NonZeroU64, crate::CheckpointError> {
            Ok(NonZeroU64::MIN)
        }

        fn put_until(
            &self,
            _: NonZeroU64,
            name: &str,
            bytes: &[u8],
            deadline: std::time::Instant,
        ) -> Result<(), crate::CheckpointError> {
            (std::time::Instant::now() < deadline)
                .then_some(())
                .ok_or_else(|| crate::CheckpointError::new("deadline exceeded"))?;
            self.0.lock().unwrap().insert(name.to_owned(), bytes.to_vec());
            Ok(())
        }

        fn get(&self, name: &str) -> Result<Vec<u8>, crate::CheckpointError> {
            self.0
                .lock()
                .unwrap()
                .get(name)
                .cloned()
                .ok_or_else(|| crate::CheckpointError::new("missing object"))
        }

        fn list(&self) -> Result<Vec<String>, crate::CheckpointError> {
            Ok(self.0.lock().unwrap().keys().cloned().collect())
        }

        fn abort_until(&self, _: NonZeroU64, _: std::time::Instant) -> Result<(), crate::CheckpointError> {
            Ok(())
        }

        fn get_until(&self, name: &str, deadline: std::time::Instant) -> Result<Vec<u8>, crate::CheckpointError> {
            (std::time::Instant::now() < deadline)
                .then_some(())
                .ok_or_else(|| crate::CheckpointError::new("deadline exceeded"))?;
            self.get(name)
        }

        fn list_until(&self, deadline: std::time::Instant) -> Result<Vec<String>, crate::CheckpointError> {
            (std::time::Instant::now() < deadline)
                .then_some(())
                .ok_or_else(|| crate::CheckpointError::new("deadline exceeded"))?;
            self.list()
        }

        fn commit_until(
            &self,
            transaction: NonZeroU64,
            manifest: &[u8],
            deadline: std::time::Instant,
        ) -> Result<(), crate::CheckpointError> {
            (std::time::Instant::now() < deadline)
                .then_some(())
                .ok_or_else(|| crate::CheckpointError::new("deadline exceeded"))?;
            self.put_until(transaction, "MANIFEST", manifest, deadline)
        }
    }

    #[test]
    fn checkpoint_transport_publishes_then_reads_exact_image() {
        let image = Arc::new(Image::default());
        let transport = CheckpointTransport::new(image.clone());
        transport.replace(b"rust-checkpoint").unwrap();
        assert_eq!(transport.read(64).unwrap(), b"rust-checkpoint");
        assert_eq!(image.list().unwrap(), ["MANIFEST", "rust/image"]);
        assert!(transport.read(4).is_err());
    }

    #[test]
    fn checkpoint_transport_rejects_uncommitted_or_torn_images() {
        let image = Arc::new(Image::default());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let transaction = image.begin_until(deadline).unwrap();
        image
            .put_until(transaction, "rust/image", b"partial", deadline)
            .unwrap();
        let transport = CheckpointTransport::new(image.clone());
        assert!(transport.read(64).is_err());
        image
            .put_until(transaction, "MANIFEST", b"not-a-rust-manifest", deadline)
            .unwrap();
        assert!(transport.read(64).is_err());
    }

    struct BusyImage;

    impl crate::CheckpointImage for BusyImage {
        fn begin_until(&self, _: std::time::Instant) -> Result<NonZeroU64, crate::CheckpointError> {
            Err(crate::CheckpointError::busy())
        }
        fn put_until(
            &self,
            _: NonZeroU64,
            _: &str,
            _: &[u8],
            _: std::time::Instant,
        ) -> Result<(), crate::CheckpointError> {
            unreachable!()
        }
        fn abort_until(&self, _: NonZeroU64, _: std::time::Instant) -> Result<(), crate::CheckpointError> {
            unreachable!()
        }
        fn get(&self, _: &str) -> Result<Vec<u8>, crate::CheckpointError> {
            unreachable!()
        }
        fn get_until(&self, _: &str, _: std::time::Instant) -> Result<Vec<u8>, crate::CheckpointError> {
            unreachable!()
        }
        fn list(&self) -> Result<Vec<String>, crate::CheckpointError> {
            unreachable!()
        }
        fn list_until(&self, _: std::time::Instant) -> Result<Vec<String>, crate::CheckpointError> {
            unreachable!()
        }
        fn commit_until(&self, _: NonZeroU64, _: &[u8], _: std::time::Instant) -> Result<(), crate::CheckpointError> {
            unreachable!()
        }
    }

    #[test]
    fn checkpoint_transport_preserves_transaction_busy() {
        let transport = CheckpointTransport::new(Arc::new(BusyImage));
        assert_eq!(
            transport.begin_until(std::time::Instant::now() + std::time::Duration::from_secs(1)),
            Err(hl_engine::composition::CompositionError::TransactionBusy)
        );
    }

    fn launch() -> ProcessConfig {
        ProcessConfig {
            network_namespace: "container-test".to_owned(),
            rootfs: "/rootfs".into(),
            overlay: None,
            owners: Vec::new(),
            filesystem_generation: "/generation".into(),
            translation_cache: None,
            checkpoint: None,
            guest: crate::Guest::Aarch64,
            execution: crate::Execution::default(),
            process: crate::Process::new("/bin/true"),
            hostname: None,
            mounts: Vec::new(),
            resources: crate::Resources::default(),
            isolation: crate::Isolation::default(),
            network_mode: crate::NetworkMode::Automatic,
            networks: Vec::new(),
            publish: Vec::new(),
            input: None,
            terminal: None,
            domain: None,
            domain_owner: true,
        }
    }

    fn bridge(name: &str, address: &str, prefix: u8) -> NetworkConfig {
        NetworkConfig {
            namespace: "container-test".to_owned(),
            bridge: Some(name.to_owned()),
            address: Some(address.parse().unwrap()),
            prefix: Some(prefix),
            name: name.to_owned(),
            driver: crate::NetworkDriver::Bridge,
            endpoints: Vec::new(),
        }
    }

    #[test]
    fn resolved_container_plan_uses_the_production_engine() {
        let launch = launch();
        let spec = Spec::try_from(&launch).unwrap();
        assert_eq!(spec.plan.rootfs.as_deref(), Some(b"/rootfs".as_slice()));
        assert_eq!(spec.plan.arguments[0], b"/bin/true");
        assert_eq!(spec.plan.options.get("HL_NETNS"), Some("container-test"));
    }

    #[test]
    fn checkpoint_transport_arms_capture_and_requested_restore() {
        for restore in [false, true] {
            let mut launch = launch();
            launch.checkpoint = Some(crate::service::CheckpointRole::Coordinator(
                crate::service::CheckpointConfig {
                    image: Arc::new(Image::default()),
                    restore,
                },
            ));
            let spec = Spec::try_from(&launch).unwrap();
            assert_eq!(spec.plan.options.get("HL_CHECKPOINT"), Some("1"));
            assert_eq!(spec.plan.options.get("HL_RESTORE"), restore.then_some("1"));
        }
    }

    #[test]
    fn file_mounts_use_the_volume_protocol() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("guest-program");
        std::fs::write(&source, b"program").unwrap();
        let mut launch = launch();
        launch.mounts.push(crate::model::ResolvedMount {
            source: source.clone(),
            target: "/bin/guest-program".into(),
            access: crate::Access::ReadOnly,
        });

        let spec = Spec::try_from(&launch).unwrap();

        assert_eq!(
            spec.plan.options.get("HL_VOLUMES"),
            Some(format!("ro:/bin/guest-program:{}", source.display()).as_str())
        );
        assert_eq!(spec.plan.options.get("HL_NAME_BINDS"), None);
    }

    #[test]
    fn native_execution_selects_only_the_retained_c_diagnostics_option() {
        // The engine has no native-execution switch: `src/runtime/hl-native/src/native/engine/options.c`
        // is the authoritative option registry and defines neither `HL_NATIVE_EXECUTION` nor
        // `HL_NATIVE_DIAGNOSTICS`. `Execution::Native` therefore carries exactly one launch effect,
        // the retained-C diagnostics request.
        let mut launch = launch();
        launch.execution = crate::Execution::native(true);
        let spec = Spec::try_from(&launch).unwrap();
        assert_eq!(spec.plan.options.get("HL_NATIVE_EXECUTION"), None);
        assert_eq!(spec.plan.options.get("HL_NATIVE_DIAGNOSTICS"), None);
        assert_eq!(spec.plan.options.get("HL_C_DIAGNOSTICS"), Some("1"));
    }

    #[test]
    fn native_execution_without_diagnostics_selects_no_launch_option() {
        let mut launch = launch();
        launch.execution = crate::Execution::native(false);
        let spec = Spec::try_from(&launch).unwrap();
        assert_eq!(spec.plan.options.get("HL_NATIVE_EXECUTION"), None);
        assert_eq!(spec.plan.options.get("HL_NATIVE_DIAGNOSTICS"), None);
        assert_eq!(spec.plan.options.get("HL_C_DIAGNOSTICS"), None);
    }

    #[test]
    fn every_execution_mode_leaves_product_backend_unselected() {
        for execution in [crate::Execution::default(), crate::Execution::native(false)] {
            let mut launch = launch();
            launch.execution = execution;
            let spec = Spec::try_from(&launch).unwrap();
            assert_eq!(spec.plan.options.get("HL_EXECUTION_BACKEND"), None);
        }
    }

    #[test]
    fn network_interfaces_preserve_attachment_order_and_prefixes() {
        let mut launch = launch();
        launch.networks = vec![bridge("front", "172.29.0.2", 24), bridge("back", "10.7.0.9", 19)];

        let spec = Spec::try_from(&launch).unwrap();

        assert_eq!(
            spec.plan.options.get("HL_NETIFS"),
            Some("front=172.29.0.2/24\nback=10.7.0.9/19")
        );
        assert_eq!(spec.plan.options.get("HL_NETBR"), None);
        assert_eq!(spec.plan.options.get("HL_IP"), None);
    }

    #[test]
    fn network_interfaces_are_bounded() {
        let mut launch = launch();
        launch.networks = (0..9)
            .map(|index| bridge(&format!("net{index}"), &format!("10.0.0.{}", index + 1), 24))
            .collect();

        let error = Spec::try_from(&launch).err().unwrap();
        assert!(error.to_string().contains("at most 8 virtual network interfaces"));
    }

    #[test]
    fn network_interface_fields_are_validated() {
        let mut launch = launch();
        launch.networks = vec![bridge("bad=bridge", "10.0.0.2", 24)];
        assert!(Spec::try_from(&launch).is_err());

        launch.networks = vec![bridge(&"x".repeat(41), "10.0.0.2", 24)];
        assert!(Spec::try_from(&launch).is_err());

        launch.networks = vec![bridge("valid", "10.0.0.2", 33)];
        assert!(Spec::try_from(&launch).is_err());

        launch.networks = vec![bridge("valid", "0.0.0.0", 24)];
        assert!(Spec::try_from(&launch).is_err());

        let mut incomplete = bridge("valid", "10.0.0.2", 24);
        incomplete.prefix = None;
        launch.networks = vec![incomplete];
        assert!(Spec::try_from(&launch).is_err());
    }

    #[test]
    fn terminal_channel_preserves_partial_input_and_eof() {
        let (input, receiver) = tokio::sync::mpsc::channel(1);
        input.blocking_send(b"abcdef".to_vec()).unwrap();
        drop(input);
        let (output, _logs) = crate::service::log_channel();
        let terminal = TerminalChannel::new(Some(receiver), output);
        let mut bytes = [0_u8; 4];

        assert_eq!(terminal.read(&mut bytes).unwrap(), 4);
        assert_eq!(&bytes, b"abcd");
        assert_eq!(terminal.read(&mut bytes).unwrap(), 2);
        assert_eq!(&bytes[..2], b"ef");
        assert_eq!(terminal.read(&mut bytes).unwrap(), 0);
    }

    #[test]
    fn terminal_channel_merges_and_bounds_output() {
        let (output, mut logs) = crate::service::log_channel();
        let terminal = TerminalChannel::new(None, output);
        let bytes = vec![b'x'; crate::service::LOG_CHUNK_BYTES + 7];

        assert_eq!(terminal.write(&bytes).unwrap(), crate::service::LOG_CHUNK_BYTES);
        assert_eq!(
            logs.blocking_recv().unwrap(),
            crate::LogChunk {
                stream: crate::Stream::Stdout,
                bytes: bytes[..crate::service::LOG_CHUNK_BYTES].to_vec(),
            }
        );
    }

    #[test]
    fn terminal_close_cancels_blocked_read() {
        let (_input, receiver) = tokio::sync::mpsc::channel(1);
        let (output, _logs) = crate::service::log_channel();
        let terminal = Arc::new(TerminalChannel::new(Some(receiver), output));
        let reader = Arc::clone(&terminal);
        let (finished, result) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            finished.send(reader.read(&mut [0_u8; 1])).unwrap();
        });

        terminal.close();

        assert_eq!(
            result
                .recv_timeout(std::time::Duration::from_millis(100))
                .unwrap()
                .unwrap(),
            0
        );
        worker.join().unwrap();
    }

    #[test]
    fn terminal_close_cancels_backpressured_write() {
        let (output, _logs) = crate::service::log_channel();
        for _ in 0..crate::service::LOG_QUEUE_DEPTH {
            output
                .blocking_send(crate::LogChunk {
                    stream: crate::Stream::Stdout,
                    bytes: vec![b'x'],
                })
                .unwrap();
        }
        let terminal = Arc::new(TerminalChannel::new(None, output));
        let writer = Arc::clone(&terminal);
        let (finished, result) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            finished.send(writer.write(b"blocked")).unwrap();
        });

        terminal.close();

        assert_eq!(
            result
                .recv_timeout(std::time::Duration::from_millis(100))
                .unwrap()
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::BrokenPipe
        );
        worker.join().unwrap();
    }
}
