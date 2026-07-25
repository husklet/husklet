use crate::{
    model::ResolvedMount, ExitStatus, Guest, Isolation, LogChunk, Process, Resources, Result,
    Signal, Size,
};
use async_trait::async_trait;
use std::{path::PathBuf, sync::Arc};

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
            bridge: (network.driver == crate::NetworkDriver::Bridge)
                .then(|| network.id.to_string()),
            address: network
                .endpoints
                .get(id)
                .and_then(|endpoint| endpoint.address),
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
    pub(crate) checkpoint: Option<CheckpointConfig>,
    pub(crate) guest: Guest,
    pub(crate) process: Process,
    pub(crate) hostname: Option<String>,
    pub(crate) mounts: Vec<ResolvedMount>,
    pub(crate) resources: Resources,
    pub(crate) isolation: Isolation,
    pub(crate) network_mode: crate::NetworkMode,
    pub(crate) networks: Vec<NetworkConfig>,
    pub(crate) publish: Vec<crate::Publication>,
    pub(crate) input: Option<tokio::sync::mpsc::Receiver<Vec<u8>>>,
    pub(crate) terminal: Option<Size>,
    pub(crate) domain: Option<hl_engine::Domain>,
    pub(crate) domain_owner: bool,
    pub(crate) extensions: Vec<hl_engine::extension::ExtensionSpec>,
    pub(crate) authorities: Vec<crate::Authority>,
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
    fn domain(&self) -> Option<hl_engine::Domain>;
    fn checkpointable(&self) -> bool;
    async fn wait(self: Arc<Self>) -> Result<ExitStatus>;
    async fn signal(&self, signal: Signal) -> Result<()>;
    async fn pause(&self) -> Result<()>;
    async fn resume(&self) -> Result<()>;
    async fn checkpoint(&self, timeout: std::time::Duration) -> Result<()>;
    async fn resize(&self, size: Size) -> Result<()>;
    fn take_logs(&self) -> Option<tokio::sync::mpsc::UnboundedReceiver<LogChunk>>;
}

#[async_trait]
pub(crate) trait Runtime: Send + Sync {
    fn validate_overlay(&self, _overlay: &OverlayConfig) -> bool {
        false
    }
    async fn start(&self, config: ProcessConfig) -> Result<Arc<dyn Running>>;
}
