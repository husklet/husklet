use super::{Containers, Execs, Logs, NetworkStore, VolumeStore};
use crate::{
    Container, ContainerId, Entry, Error, Exec, ExecId, JournalId, Network, Result, Stream, Volume, model::now_ms,
};
use async_trait::async_trait;
use std::collections::BTreeMap;
use tokio::sync::RwLock;

#[derive(Default)]
pub(crate) struct Memory {
    values: RwLock<BTreeMap<ContainerId, Container>>,
    execs: RwLock<BTreeMap<ExecId, Exec>>,
    logs: RwLock<BTreeMap<JournalId, Vec<Entry>>>,
    volumes: RwLock<BTreeMap<String, Volume>>,
    networks: RwLock<BTreeMap<String, Network>>,
    #[cfg(test)]
    fail_exec_replace: std::sync::atomic::AtomicU64,
}

#[cfg(test)]
impl Memory {
    pub(crate) fn fail_next_exec_replace(&self) {
        self.fail_exec_replace.store(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[async_trait]
impl NetworkStore for Memory {
    async fn list(&self) -> Result<Vec<Network>> {
        Ok(self.networks.read().await.values().cloned().collect())
    }
    async fn get(&self, name: &str) -> Result<Option<Network>> {
        Ok(self.networks.read().await.get(name).cloned())
    }
    async fn insert(&self, network: &Network) -> Result<()> {
        let mut values = self.networks.write().await;
        if values.contains_key(&network.name) {
            return Err(Error::NetworkConflict(network.name.clone()));
        }
        values.insert(network.name.clone(), network.clone());
        Ok(())
    }
    async fn replace(&self, network: &Network) -> Result<()> {
        let mut values = self.networks.write().await;
        if !values.contains_key(&network.name) {
            return Err(Error::NetworkNotFound(network.name.clone()));
        }
        values.insert(network.name.clone(), network.clone());
        Ok(())
    }
    async fn remove(&self, name: &str) -> Result<()> {
        self.networks
            .write()
            .await
            .remove(name)
            .ok_or_else(|| Error::NetworkNotFound(name.into()))?;
        Ok(())
    }
}

#[async_trait]
impl VolumeStore for Memory {
    async fn list(&self) -> Result<Vec<Volume>> {
        Ok(self.volumes.read().await.values().cloned().collect())
    }

    async fn get(&self, name: &str) -> Result<Option<Volume>> {
        Ok(self.volumes.read().await.get(name).cloned())
    }

    async fn insert(&self, volume: &Volume) -> Result<()> {
        let mut volumes = self.volumes.write().await;
        if volumes.contains_key(&volume.name) {
            return Err(Error::VolumeConflict(volume.name.clone()));
        }
        volumes.insert(volume.name.clone(), volume.clone());
        Ok(())
    }

    async fn remove(&self, name: &str) -> Result<()> {
        self.volumes
            .write()
            .await
            .remove(name)
            .ok_or_else(|| Error::VolumeNotFound(name.into()))?;
        Ok(())
    }
}

#[async_trait]
impl Execs for Memory {
    async fn list(&self) -> Result<Vec<Exec>> {
        Ok(self.execs.read().await.values().cloned().collect())
    }

    async fn get(&self, id: &ExecId) -> Result<Option<Exec>> {
        Ok(self.execs.read().await.get(id).cloned())
    }

    async fn insert(&self, exec: &Exec) -> Result<()> {
        let mut values = self.execs.write().await;
        if values.contains_key(&exec.id) {
            return Err(Error::Corrupt(format!("duplicate exec {}", exec.id)));
        }
        values.insert(exec.id.clone(), exec.clone());
        Ok(())
    }

    async fn replace(&self, exec: &Exec) -> Result<()> {
        #[cfg(test)]
        if self
            .fail_exec_replace
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |remaining| remaining.checked_sub(1),
            )
            .is_ok()
        {
            return Err(Error::Corrupt("injected exec replace failure".into()));
        }
        let mut values = self.execs.write().await;
        if !values.contains_key(&exec.id) {
            return Err(Error::NotFound(exec.id.to_string()));
        }
        values.insert(exec.id.clone(), exec.clone());
        Ok(())
    }

    async fn remove(&self, id: &ExecId) -> Result<()> {
        self.execs
            .write()
            .await
            .remove(id)
            .ok_or_else(|| Error::NotFound(id.to_string()))?;
        Ok(())
    }

    async fn remove_parent(&self, id: &ContainerId) -> Result<()> {
        self.execs.write().await.retain(|_, exec| &exec.container != id);
        Ok(())
    }
}

#[async_trait]
impl Containers for Memory {
    async fn list(&self) -> Result<Vec<Container>> {
        Ok(self.values.read().await.values().cloned().collect())
    }
    async fn get(&self, id: &ContainerId) -> Result<Option<Container>> {
        Ok(self.values.read().await.get(id).cloned())
    }
    async fn insert(&self, container: &Container) -> Result<()> {
        let mut values = self.values.write().await;
        if values.contains_key(&container.id) {
            return Err(Error::Corrupt(format!("duplicate container {}", container.id)));
        }
        values.insert(container.id.clone(), container.clone());
        Ok(())
    }
    async fn replace(&self, container: &Container) -> Result<()> {
        let mut values = self.values.write().await;
        if !values.contains_key(&container.id) {
            return Err(Error::NotFound(container.id.to_string()));
        }
        values.insert(container.id.clone(), container.clone());
        Ok(())
    }
    async fn remove(&self, id: &ContainerId) -> Result<()> {
        self.values
            .write()
            .await
            .remove(id)
            .ok_or_else(|| Error::NotFound(id.to_string()))?;
        Ok(())
    }
}

#[async_trait]
impl Logs for Memory {
    async fn append(&self, id: &JournalId, stream: Stream, bytes: &[u8]) -> Result<Entry> {
        let mut values = self.logs.write().await;
        let journal = values.entry(id.clone()).or_default();
        let sequence = journal
            .last()
            .map_or(Some(1), |entry| entry.sequence.checked_add(1))
            .ok_or_else(|| Error::Corrupt("log sequence exhausted".into()))?;
        let entry = Entry {
            sequence,
            timestamp_ms: now_ms(),
            stream,
            bytes: bytes.to_vec(),
        };
        journal.push(entry.clone());
        Ok(entry)
    }
    async fn read(&self, id: &JournalId) -> Result<crate::Logs> {
        let values = self.logs.read().await;
        let mut logs = crate::Logs::default();
        for entry in values.get(id).into_iter().flatten() {
            match entry.stream {
                Stream::Stdout => logs.stdout.extend_from_slice(&entry.bytes),
                Stream::Stderr => logs.stderr.extend_from_slice(&entry.bytes),
            }
        }
        Ok(logs)
    }
    async fn cursor(&self, id: &JournalId) -> Result<u64> {
        Ok(self
            .logs
            .read()
            .await
            .get(id)
            .and_then(|entries| entries.last())
            .map_or(0, |entry| entry.sequence))
    }
    async fn after(&self, id: &JournalId, sequence: u64, limit: usize) -> Result<Vec<Entry>> {
        Ok(self
            .logs
            .read()
            .await
            .get(id)
            .into_iter()
            .flatten()
            .filter(|entry| entry.sequence > sequence)
            .take(limit)
            .cloned()
            .collect())
    }
    async fn remove(&self, id: &JournalId) -> Result<()> {
        self.logs.write().await.remove(id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Execs as _, Memory};
    use crate::{ContainerId, Exec, ExecSpec, ExecState, Process};

    #[tokio::test]
    async fn execution_state_and_parent_cleanup_are_consistent() {
        let repository = Memory::default();
        let parent = ContainerId::new();
        let retained_parent = ContainerId::new();
        let mut removed = Exec::new(parent.clone(), ExecSpec::new(Process::new("/bin/one")));
        let retained = Exec::new(retained_parent, ExecSpec::new(Process::new("/bin/two")));
        repository.insert(&removed).await.unwrap();
        repository.insert(&retained).await.unwrap();

        removed.state = ExecState::Running {
            process_id: 41,
            started_at_ms: 12,
        };
        repository.replace(&removed).await.unwrap();
        assert_eq!(repository.get(&removed.id).await.unwrap(), Some(removed));

        repository.remove_parent(&parent).await.unwrap();
        assert_eq!(repository.list().await.unwrap(), vec![retained]);
    }
}
