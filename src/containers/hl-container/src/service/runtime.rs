use crate::{ExitStatus, Guest, Isolation, LogChunk, Process, Resources, Result, Signal, Size, model::ResolvedMount};
use async_trait::async_trait;
use std::{path::PathBuf, sync::Arc};

pub(crate) const LOG_QUEUE_DEPTH: usize = 64;
pub(crate) const LOG_CHUNK_BYTES: usize = 16 * 1024;

pub(crate) type LogReceiver = tokio::sync::mpsc::Receiver<LogChunk>;
pub(crate) type LogSender = tokio::sync::mpsc::Sender<LogChunk>;

pub(crate) fn log_channel() -> (LogSender, LogReceiver) {
    tokio::sync::mpsc::channel(LOG_QUEUE_DEPTH)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NetworkConfig {
    pub(crate) namespace: String,
    pub(crate) bridge: Option<String>,
    pub(crate) address: Option<std::net::Ipv4Addr>,
    pub(crate) prefix: Option<u8>,
    pub(crate) name: String,
    pub(crate) driver: crate::NetworkDriver,
    pub(crate) endpoints: Vec<crate::Endpoint>,
}

impl NetworkConfig {
    pub(crate) fn from_network(network: &crate::Network, id: &crate::ContainerId) -> Self {
        Self {
            namespace: id.namespace(),
            bridge: (network.driver == crate::NetworkDriver::Bridge).then(|| network.id.to_string()),
            address: network.endpoints.get(id).and_then(|endpoint| endpoint.address),
            prefix: network.subnet.map(|subnet| subnet.prefix),
            name: network.name.clone(),
            driver: network.driver,
            endpoints: network.endpoints.values().cloned().collect(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OverlayConfig {
    pub(crate) lower: PathBuf,
    pub(crate) upper: PathBuf,
    pub(crate) work: PathBuf,
}

/// Runtime-neutral, fully resolved launch request.
#[derive(Debug)]
pub(crate) struct ProcessConfig {
    /// Stable opaque identity used to join every process launched for one container.
    pub(crate) network_namespace: String,
    pub(crate) rootfs: PathBuf,
    pub(crate) overlay: Option<OverlayConfig>,
    pub(crate) owners: Vec<(PathBuf, u32, u32)>,
    pub(crate) filesystem_generation: PathBuf,
    pub(crate) translation_cache: Option<PathBuf>,
    pub(crate) checkpoint: Option<CheckpointRole>,
    pub(crate) guest: Guest,
    pub(crate) execution: crate::Execution,
    pub(crate) process: Process,
    pub(crate) hostname: Option<String>,
    pub(crate) mounts: Vec<ResolvedMount>,
    pub(crate) resources: Resources,
    pub(crate) isolation: Isolation,
    pub(crate) network_mode: crate::NetworkMode,
    pub(crate) networks: Vec<NetworkConfig>,
    pub(crate) publish: Vec<crate::Publication>,
    pub(crate) input: Option<tokio::sync::mpsc::Receiver<Vec<u8>>>,
    // Runtime ports receive these launch semantics even though the built-in engine adapter does not
    // consume them yet; substitute runtimes verify their propagation.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) terminal: Option<Size>,
    pub(crate) domain: Option<hl_engine::Domain>,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) domain_owner: bool,
}

/// How a launch participates in its process domain's checkpoint.
///
/// A domain has exactly one coordinator, which owns the image, the broker socket
/// and the trigger word. Every other session in the domain is a member: it joins
/// that broker and trigger and holds no image, so it is captured as part of the
/// coordinator's freeze and can never be the subject of a capture of its own.
#[derive(Debug)]
pub(crate) enum CheckpointRole {
    Coordinator(CheckpointConfig),
    DomainMember,
}

#[derive(Clone)]
pub(crate) struct CheckpointConfig {
    pub(crate) image: Arc<dyn crate::CheckpointImage>,
    pub(crate) restore: bool,
}

impl std::fmt::Debug for CheckpointConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CheckpointConfig")
            .field("restore", &self.restore)
            .finish_non_exhaustive()
    }
}

#[async_trait]
pub(crate) trait Running: Send + Sync {
    fn id(&self) -> u64;
    fn domain(&self) -> hl_engine::Domain;
    fn checkpointable(&self) -> bool;
    /// The container-namespace pid of the guest process this launch is running, once the guest has
    /// published one.
    ///
    /// A whole-image capture names every sealed member by exactly this number and a restore re-forks
    /// it under the same one, so it is the only identity of a launched guest that survives the
    /// capture. It is `None` before the guest has entered its container identity and again once the
    /// process has been reaped, so a caller that needs it durably reads it while the guest runs.
    fn guest_pid(&self) -> Option<std::num::NonZeroI32>;
    async fn wait(self: Arc<Self>) -> Result<ExitStatus>;
    async fn signal(&self, signal: Signal) -> Result<()>;
    async fn pause(&self) -> Result<()>;
    async fn resume(&self) -> Result<()>;
    async fn checkpoint(&self, timeout: std::time::Duration) -> Result<()>;
    async fn resize(&self, size: Size) -> Result<()>;
    fn take_logs(&self) -> Option<LogReceiver>;
}

#[async_trait]
pub(crate) trait Runtime: Send + Sync {
    fn validate_overlay(&self, _overlay: &OverlayConfig) -> bool {
        false
    }
    async fn start(&self, config: ProcessConfig) -> Result<Arc<dyn Running>>;
}
