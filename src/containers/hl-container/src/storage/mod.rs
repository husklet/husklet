mod file;
mod memory;

use crate::{Container, ContainerId, Entry, Exec, ExecId, JournalId, Network, Result, Stream, Volume};
use async_trait::async_trait;

pub(crate) use file::Disk;
pub(crate) use memory::Memory;

#[async_trait]
pub(crate) trait Containers: Send + Sync {
    async fn list(&self) -> Result<Vec<Container>>;
    async fn get(&self, id: &ContainerId) -> Result<Option<Container>>;
    async fn insert(&self, container: &Container) -> Result<()>;
    async fn replace(&self, container: &Container) -> Result<()>;
    async fn remove(&self, id: &ContainerId) -> Result<()>;
}

#[async_trait]
pub(crate) trait Execs: Send + Sync {
    async fn list(&self) -> Result<Vec<Exec>>;
    async fn get(&self, id: &ExecId) -> Result<Option<Exec>>;
    async fn insert(&self, exec: &Exec) -> Result<()>;
    async fn replace(&self, exec: &Exec) -> Result<()>;
    async fn remove(&self, id: &ExecId) -> Result<()>;
    async fn remove_parent(&self, id: &ContainerId) -> Result<()>;
}

#[async_trait]
pub(crate) trait Logs: Send + Sync {
    async fn read(&self, id: &JournalId) -> Result<crate::Logs>;
    async fn append(&self, id: &JournalId, stream: Stream, bytes: &[u8]) -> Result<Entry>;
    async fn cursor(&self, id: &JournalId) -> Result<u64>;
    async fn after(&self, id: &JournalId, sequence: u64, limit: usize) -> Result<Vec<Entry>>;
    async fn remove(&self, id: &JournalId) -> Result<()>;
}

#[async_trait]
pub(crate) trait VolumeStore: Send + Sync {
    async fn list(&self) -> Result<Vec<Volume>>;
    async fn get(&self, name: &str) -> Result<Option<Volume>>;
    async fn insert(&self, volume: &Volume) -> Result<()>;
    async fn remove(&self, name: &str) -> Result<()>;
}

#[async_trait]
pub(crate) trait NetworkStore: Send + Sync {
    async fn list(&self) -> Result<Vec<Network>>;
    async fn get(&self, name: &str) -> Result<Option<Network>>;
    async fn insert(&self, network: &Network) -> Result<()>;
    async fn replace(&self, network: &Network) -> Result<()>;
    async fn remove(&self, name: &str) -> Result<()>;
}

pub(crate) trait Storage: Containers + Execs + Logs + VolumeStore + NetworkStore {}
impl<T: Containers + Execs + Logs + VolumeStore + NetworkStore> Storage for T {}
