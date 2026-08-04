use crate::{
    Error, Result,
    service::{OverlayConfig, ProcessConfig, Running, Runtime},
};
use async_trait::async_trait;
use std::{
    collections::VecDeque,
    io::{Read, Write},
    sync::{Arc, Mutex as StdMutex},
};

mod process;
mod spec;
use process::Process;
use spec::Spec;

#[derive(Default)]
pub(crate) struct Engine;

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
    sender: tokio::sync::mpsc::UnboundedSender<crate::LogChunk>,
}

impl LogOutput {
    fn new(stream: crate::Stream, sender: tokio::sync::mpsc::UnboundedSender<crate::LogChunk>) -> Self {
        Self { stream, sender }
    }
}

impl Write for LogOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        self.sender
            .send(crate::LogChunk {
                stream: self.stream,
                bytes: bytes.to_vec(),
            })
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "container log receiver closed"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
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
        if config.checkpoint.as_ref().is_some_and(|checkpoint| checkpoint.restore) {
            return Err(Error::Runtime(
                "Rust engine checkpoint restore is not connected to container storage".into(),
            ));
        }
        let checkpointable = false;
        let spec = Spec::try_from(&config)?;
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let streams = hl_engine::composition::StandardStreams::new(
            ChannelInput::new(config.input.take()),
            LogOutput::new(crate::Stream::Stdout, sender.clone()),
            LogOutput::new(crate::Stream::Stderr, sender),
        );
        let engine = Arc::new(
            hl_engine::runtime::Engine::from_plan_with_streams(spec.isa, spec.plan, streams)
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
    use super::{ChannelInput, LogOutput, Spec};
    use crate::service::ProcessConfig;
    use std::io::{Read, Write};

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

    #[test]
    fn resolved_container_plan_uses_the_rust_engine() {
        let launch = launch();
        let spec = Spec::try_from(&launch).unwrap();
        assert_eq!(spec.plan.rootfs.as_deref(), Some(b"/rootfs".as_slice()));
        assert_eq!(spec.plan.arguments[0], b"/bin/true");
        assert_eq!(spec.plan.options.get("HL_NETNS"), Some("container-test"));
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

        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
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
}
