use super::{Containers, Execs, Logs, NetworkStore, VolumeStore};
use crate::{
    Container, ContainerId, Entry, Error, Exec, ExecId, JournalId, Network, Result, Stream, Volume, model::now_ms,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

const VERSION: u32 = 1;
const JOURNAL_HEADER: usize = 25;
const RECORD_LIMIT: u64 = 16 * 1024 * 1024;
const JOURNAL_STRIPES: usize = 64;

use journal::initialize;

#[derive(Clone)]
pub(crate) struct Disk {
    directory: PathBuf,
    execs: PathBuf,
    volumes: PathBuf,
    networks: PathBuf,
    transaction: Arc<Mutex<()>>,
    journal_stripes: Arc<[Mutex<()>; JOURNAL_STRIPES]>,
    indexes: Arc<Mutex<std::collections::BTreeMap<JournalId, Vec<u64>>>>,
}

impl Disk {
    pub(crate) async fn open(root: PathBuf) -> Result<Self> {
        let directory = root.join("state/containers");
        let execs = root.join("state/execs");
        let volumes = root.join("state/volumes");
        let networks = root.join("state/networks");
        let indexes = Self::blocking({
            let directory = directory.clone();
            let execs = execs.clone();
            let volumes = volumes.clone();
            let networks = networks.clone();
            move || initialize(&directory, &execs, &volumes, &networks)
        })
        .await?;
        Ok(Self {
            directory,
            execs,
            volumes,
            networks,
            transaction: Arc::new(Mutex::new(())),
            journal_stripes: Arc::new(std::array::from_fn(|_| Mutex::new(()))),
            indexes: Arc::new(Mutex::new(indexes)),
        })
    }
}

enum Require {
    Absent,
    Present,
}

#[async_trait]
impl Containers for Disk {
    async fn list(&self) -> Result<Vec<Container>> {
        let repository = self.clone();
        Self::blocking(move || repository.list_sync()).await
    }
    async fn get(&self, id: &ContainerId) -> Result<Option<Container>> {
        let repository = self.clone();
        let id = id.clone();
        Self::blocking(move || repository.get_sync(&id)).await
    }
    async fn insert(&self, container: &Container) -> Result<()> {
        let repository = self.clone();
        let container = container.clone();
        Self::blocking(move || repository.write(&container, Require::Absent)).await
    }
    async fn replace(&self, container: &Container) -> Result<()> {
        let repository = self.clone();
        let container = container.clone();
        Self::blocking(move || repository.write(&container, Require::Present)).await
    }
    async fn remove(&self, id: &ContainerId) -> Result<()> {
        let repository = self.clone();
        let id = id.clone();
        Self::blocking(move || repository.remove_sync(&id)).await
    }
}

#[async_trait]
impl Execs for Disk {
    async fn list(&self) -> Result<Vec<Exec>> {
        let repository = self.clone();
        Self::blocking(move || repository.list_execs_sync()).await
    }

    async fn get(&self, id: &ExecId) -> Result<Option<Exec>> {
        let repository = self.clone();
        let id = id.clone();
        Self::blocking(move || repository.get_exec_sync(&id)).await
    }

    async fn insert(&self, exec: &Exec) -> Result<()> {
        let repository = self.clone();
        let exec = exec.clone();
        Self::blocking(move || repository.write_exec(&exec, Require::Absent)).await
    }

    async fn replace(&self, exec: &Exec) -> Result<()> {
        let repository = self.clone();
        let exec = exec.clone();
        Self::blocking(move || repository.write_exec(&exec, Require::Present)).await
    }

    async fn remove(&self, id: &ExecId) -> Result<()> {
        let repository = self.clone();
        let id = id.clone();
        Self::blocking(move || repository.remove_exec_sync(&id)).await
    }

    async fn remove_parent(&self, id: &ContainerId) -> Result<()> {
        let repository = self.clone();
        let id = id.clone();
        Self::blocking(move || repository.remove_parent_sync(&id)).await
    }
}

#[async_trait]
impl VolumeStore for Disk {
    async fn list(&self) -> Result<Vec<Volume>> {
        let repository = self.clone();
        Self::blocking(move || repository.list_volumes_sync()).await
    }

    async fn get(&self, name: &str) -> Result<Option<Volume>> {
        let repository = self.clone();
        let name = name.to_owned();
        Self::blocking(move || repository.get_volume_sync(&name)).await
    }

    async fn insert(&self, volume: &Volume) -> Result<()> {
        let repository = self.clone();
        let volume = volume.clone();
        Self::blocking(move || repository.insert_volume_sync(&volume)).await
    }

    async fn remove(&self, name: &str) -> Result<()> {
        let repository = self.clone();
        let name = name.to_owned();
        Self::blocking(move || repository.remove_volume_sync(&name)).await
    }
}

#[async_trait]
impl NetworkStore for Disk {
    async fn list(&self) -> Result<Vec<Network>> {
        let repository = self.clone();
        Self::blocking(move || repository.list_networks_sync()).await
    }
    async fn get(&self, name: &str) -> Result<Option<Network>> {
        let repository = self.clone();
        let name = name.to_owned();
        Self::blocking(move || repository.get_network_sync(&name)).await
    }
    async fn insert(&self, network: &Network) -> Result<()> {
        let repository = self.clone();
        let network = network.clone();
        Self::blocking(move || repository.write_network_sync(&network, Require::Absent)).await
    }
    async fn replace(&self, network: &Network) -> Result<()> {
        let repository = self.clone();
        let network = network.clone();
        Self::blocking(move || repository.write_network_sync(&network, Require::Present)).await
    }
    async fn remove(&self, name: &str) -> Result<()> {
        let repository = self.clone();
        let name = name.to_owned();
        Self::blocking(move || repository.remove_network_sync(&name)).await
    }
}

#[async_trait]
impl Logs for Disk {
    async fn append(&self, id: &JournalId, stream: Stream, bytes: &[u8]) -> Result<Entry> {
        let repository = self.clone();
        let id = id.clone();
        let bytes = bytes.to_vec();
        Self::blocking(move || repository.append_sync(&id, stream, bytes)).await
    }
    async fn read(&self, id: &JournalId) -> Result<crate::Logs> {
        let repository = self.clone();
        let id = id.clone();
        let entries = Self::blocking(move || repository.entries_sync(&id)).await?;
        let mut logs = crate::Logs::default();
        for entry in entries {
            match entry.stream {
                Stream::Stdout => logs.stdout.extend_from_slice(&entry.bytes),
                Stream::Stderr => logs.stderr.extend_from_slice(&entry.bytes),
            }
        }
        Ok(logs)
    }
    async fn cursor(&self, id: &JournalId) -> Result<u64> {
        let repository = self.clone();
        let id = id.clone();
        Self::blocking(move || repository.cursor_sync(&id)).await
    }
    async fn after(&self, id: &JournalId, sequence: u64, limit: usize) -> Result<Vec<Entry>> {
        let repository = self.clone();
        let id = id.clone();
        Self::blocking(move || repository.after_sync(&id, sequence, limit)).await
    }
    async fn remove(&self, id: &JournalId) -> Result<()> {
        let repository = self.clone();
        let id = id.clone();
        Self::blocking(move || repository.remove_journal_sync(&id)).await
    }
}

mod journal;
mod record;

#[cfg(test)]
mod tests;
