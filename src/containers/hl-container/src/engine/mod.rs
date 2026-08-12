use crate::{
    Error, Result,
    service::{OverlayConfig, ProcessConfig, Running, Runtime},
};
use async_trait::async_trait;
use std::{
    collections::VecDeque,
    io::{Read, Write},
    sync::{Arc, Condvar, Mutex as StdMutex},
    time::Duration,
};

const CHECKPOINT_OBJECT: &str = "rust/image";
const CHECKPOINT_MANIFEST_MAGIC: &[u8; 8] = b"HLRUST01";

#[cfg(target_os = "linux")]
#[cfg_attr(not(test), allow(dead_code))]
mod executable;
mod process;
mod spec;
use process::Process;
use spec::Spec;

#[derive(Default)]
pub(crate) struct Engine;

struct CheckpointTransport {
    image: Arc<dyn crate::CheckpointImage>,
}

impl CheckpointTransport {
    fn new(image: Arc<dyn crate::CheckpointImage>) -> Self {
        Self { image }
    }
}

impl hl_engine::composition::CheckpointSink for CheckpointTransport {
    fn replace(&self, bytes: &[u8]) -> std::result::Result<(), hl_engine::composition::CompositionError> {
        self.image
            .put(CHECKPOINT_OBJECT, bytes)
            .map_err(|_| hl_engine::composition::CompositionError::RuntimeConstruction)?;
        let mut manifest = Vec::with_capacity(16);
        manifest.extend_from_slice(CHECKPOINT_MANIFEST_MAGIC);
        manifest.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        self.image
            .commit(&manifest)
            .map_err(|_| hl_engine::composition::CompositionError::RuntimeConstruction)
    }

    fn put(&self, name: &str, bytes: &[u8]) -> std::result::Result<(), hl_engine::composition::CompositionError> {
        self.image
            .put(name, bytes)
            .map_err(|_| hl_engine::composition::CompositionError::RuntimeConstruction)
    }

    fn commit(&self, manifest: &[u8]) -> std::result::Result<(), hl_engine::composition::CompositionError> {
        self.image
            .commit(manifest)
            .map_err(|_| hl_engine::composition::CompositionError::RuntimeConstruction)
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
}

struct ChannelInput {
    receiver: Option<tokio::sync::mpsc::Receiver<Vec<u8>>>,
    pending: VecDeque<u8>,
}

impl ChannelInput {
    fn new(receiver: Option<tokio::sync::mpsc::Receiver<Vec<u8>>>) -> Self {
        Self {
            receiver,
            pending: VecDeque::new(),
        }
    }
}

impl Read for ChannelInput {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        while self.pending.is_empty() {
            let Some(receiver) = self.receiver.as_mut() else {
                return Ok(0);
            };
            match receiver.blocking_recv() {
                Some(bytes) => self.pending.extend(bytes),
                None => self.receiver = None,
            }
        }
        let length = output.len().min(self.pending.len());
        for destination in &mut output[..length] {
            *destination = self.pending.pop_front().expect("bounded by pending length");
        }
        Ok(length)
    }
}

struct LogOutput {
    stream: crate::Stream,
    sender: crate::service::LogSender,
}

impl LogOutput {
    fn new(stream: crate::Stream, sender: crate::service::LogSender) -> Self {
        Self { stream, sender }
    }
}

impl Write for LogOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let length = bytes.len().min(crate::service::LOG_CHUNK_BYTES);
        self.sender
            .blocking_send(crate::LogChunk {
                stream: self.stream,
                bytes: bytes[..length].to_vec(),
            })
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "container log receiver closed"))?;
        Ok(length)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
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
                hl_engine::composition::StandardStreams::new(std::io::empty(), std::io::sink(), std::io::sink())
                    .with_terminal(terminal)
            }
            None => hl_engine::composition::StandardStreams::new(
                ChannelInput::new(config.input.take()),
                LogOutput::new(crate::Stream::Stdout, sender.clone()),
                LogOutput::new(crate::Stream::Stderr, sender),
            ),
        };
        let checkpoint = config
            .checkpoint
            .map(|checkpoint| Arc::new(CheckpointTransport::new(checkpoint.image)));
        let checkpointable = checkpoint.is_some();
        let engine = Arc::new(
            match checkpoint {
                Some(transport) => hl_engine::runtime::Engine::with_checkpoint(
                    spec.isa,
                    spec.plan,
                    streams,
                    transport.clone(),
                    transport,
                ),
                None => hl_engine::runtime::Engine::with_streams(spec.isa, spec.plan, streams),
            }
            .map_err(|error| Error::Runtime(format!("engine construction: {error:?}")))?,
        );
        engine
            .start()
            .map_err(|error| Error::Runtime(format!("engine start: {error:?}")))?;

        Ok(Arc::new(Process {
            id: Process::next_id(),
            child: StdMutex::new(Some(engine)),
            logs: StdMutex::new(Some(receiver)),
            domain: spec.domain,
            checkpointable,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::{ChannelInput, CheckpointTransport, LogOutput, Spec, TerminalChannel};
    use crate::CheckpointImage as _;
    use crate::service::{NetworkConfig, ProcessConfig};
    use hl_engine::composition::{CheckpointSink as _, CheckpointSource as _, TerminalPort as _};
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Image(Mutex<BTreeMap<String, Vec<u8>>>);

    impl crate::CheckpointImage for Image {
        fn put(&self, name: &str, bytes: &[u8]) -> Result<(), crate::CheckpointError> {
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
        image.put("rust/image", b"partial").unwrap();
        let transport = CheckpointTransport::new(image.clone());
        assert!(transport.read(64).is_err());
        image.put("MANIFEST", b"not-a-rust-manifest").unwrap();
        assert!(transport.read(64).is_err());
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
    fn native_execution_reaches_the_engine_launch_plan() {
        let mut launch = launch();
        launch.execution = crate::Execution::native(true);
        let spec = Spec::try_from(&launch).unwrap();
        assert_eq!(spec.plan.options.get("HL_NATIVE_EXECUTION"), Some("1"));
        assert_eq!(spec.plan.options.get("HL_NATIVE_DIAGNOSTICS"), Some("1"));
        assert_eq!(spec.plan.options.get("HL_C_EXECUTION_ATTESTATION"), Some("1"));
    }

    #[test]
    fn native_execution_without_diagnostics_omits_diagnostics_option() {
        let mut launch = launch();
        launch.execution = crate::Execution::native(false);
        let spec = Spec::try_from(&launch).unwrap();
        assert_eq!(spec.plan.options.get("HL_NATIVE_EXECUTION"), Some("1"));
        assert_eq!(spec.plan.options.get("HL_NATIVE_DIAGNOSTICS"), None);
        assert_eq!(spec.plan.options.get("HL_C_EXECUTION_ATTESTATION"), None);
    }

    #[test]
    fn explicit_retained_c_execution_reaches_the_engine_launch_plan() {
        let mut launch = launch();
        launch.execution = crate::Execution::retained_c();
        let spec = Spec::try_from(&launch).unwrap();
        assert_eq!(spec.plan.options.get("HL_EXECUTION_BACKEND"), Some("c"));
        assert_eq!(spec.plan.options.get("HL_NATIVE_EXECUTION"), Some("1"));
        assert_eq!(spec.plan.options.get("HL_NATIVE_DIAGNOSTICS"), None);
        assert_eq!(spec.plan.options.get("HL_C_DIAGNOSTICS"), None);
        assert_eq!(spec.plan.options.get("HL_C_EXECUTION_ATTESTATION"), None);
    }

    #[test]
    fn retained_c_diagnostics_enable_profile_and_completion_attestation() {
        let mut launch = launch();
        launch.execution = crate::Execution::retained_c_diagnostics();
        let spec = Spec::try_from(&launch).unwrap();
        assert_eq!(spec.plan.options.get("HL_EXECUTION_BACKEND"), Some("c"));
        assert_eq!(spec.plan.options.get("HL_C_DIAGNOSTICS"), Some("1"));
        assert_eq!(spec.plan.options.get("HL_C_EXECUTION_ATTESTATION"), Some("1"));
    }

    #[test]
    fn legacy_execution_modes_leave_product_backend_unselected() {
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
    fn stream_adapters_preserve_bytes_and_channels() {
        let (input_sender, input_receiver) = tokio::sync::mpsc::channel(1);
        input_sender.blocking_send(b"input".to_vec()).unwrap();
        drop(input_sender);
        let mut input = ChannelInput::new(Some(input_receiver));
        let mut bytes = [0_u8; 8];
        assert_eq!(input.read(&mut bytes).unwrap(), 5);
        assert_eq!(&bytes[..5], b"input");
        assert_eq!(input.read(&mut bytes).unwrap(), 0);

        let (sender, mut receiver) = crate::service::log_channel();
        let mut output = LogOutput::new(crate::Stream::Stdout, sender.clone());
        let mut error = LogOutput::new(crate::Stream::Stderr, sender);
        assert_eq!(output.write(b"out\0").unwrap(), 4);
        assert_eq!(error.write(b"err\xff").unwrap(), 4);
        drop(output);
        drop(error);
        assert_eq!(
            receiver.blocking_recv().unwrap(),
            crate::LogChunk {
                stream: crate::Stream::Stdout,
                bytes: b"out\0".to_vec(),
            }
        );
        assert_eq!(
            receiver.blocking_recv().unwrap(),
            crate::LogChunk {
                stream: crate::Stream::Stderr,
                bytes: b"err\xff".to_vec(),
            }
        );
        assert!(receiver.blocking_recv().is_none());
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

    #[test]
    fn log_output_bounds_queue_and_each_owned_copy() {
        let (sender, mut receiver) = crate::service::log_channel();
        let mut output = LogOutput::new(crate::Stream::Stdout, sender.clone());
        assert_eq!(sender.capacity(), crate::service::LOG_QUEUE_DEPTH);

        let oversized = vec![b'x'; crate::service::LOG_CHUNK_BYTES + 1];
        assert_eq!(output.write(&oversized).unwrap(), crate::service::LOG_CHUNK_BYTES);
        assert_eq!(
            receiver.blocking_recv().unwrap().bytes.len(),
            crate::service::LOG_CHUNK_BYTES
        );

        for _ in 0..crate::service::LOG_QUEUE_DEPTH {
            assert_eq!(output.write(&oversized).unwrap(), crate::service::LOG_CHUNK_BYTES);
        }
        assert_eq!(sender.capacity(), 0);
        assert_eq!(
            crate::service::LOG_QUEUE_DEPTH * crate::service::LOG_CHUNK_BYTES,
            1024 * 1024
        );
        let ready = std::sync::Arc::new(std::sync::Barrier::new(2));
        let writer_ready = ready.clone();
        let blocked = std::thread::spawn(move || {
            writer_ready.wait();
            output.write(b"blocked")
        });
        ready.wait();
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(!blocked.is_finished());
        drop(receiver);
        assert_eq!(
            blocked.join().unwrap().unwrap_err().kind(),
            std::io::ErrorKind::BrokenPipe
        );
    }
}
