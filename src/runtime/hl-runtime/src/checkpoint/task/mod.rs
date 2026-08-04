use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use hl_checkpoint::{CheckpointImage, Section};
use hl_task::{
    TASK_CHECKPOINT_VERSION, TaskExternalCheckpoint, TaskExternalRestore, TaskRegistry as HostRegistry,
    TaskRegistryImage,
};

use crate::{CheckpointParticipant, CheckpointRole};
use crate::{SeccompControl, SeccompRestoreTransaction};

mod wire;

#[cfg(test)]
pub(super) use wire::TASK_BYTES_MAXIMUM;

pub trait Codec: Send + Sync {
    fn encode(&self, image: &TaskRegistryImage) -> Result<Vec<u8>, ()>;
    fn decode(&self, bytes: &[u8]) -> Result<TaskRegistryImage, ()>;
}

#[derive(Default)]
pub struct PortableCodec;

impl Codec for PortableCodec {
    fn encode(&self, image: &TaskRegistryImage) -> Result<Vec<u8>, ()> {
        wire::TaskWire::encode(image)
    }

    fn decode(&self, bytes: &[u8]) -> Result<TaskRegistryImage, ()> {
        wire::TaskWire::decode(bytes)
    }
}

pub struct Registry {
    current: RwLock<State>,
    staged: RwLock<Option<StagedTask>>,
}

struct State {
    generation: u64,
    registry: Arc<HostRegistry>,
}

struct StagedTask {
    registry: Arc<HostRegistry>,
    image: Arc<TaskRegistryImage>,
}

impl Registry {
    #[must_use]
    pub const fn new(registry: Arc<HostRegistry>) -> Self {
        Self {
            current: RwLock::new(State {
                generation: 1,
                registry,
            }),
            staged: RwLock::new(None),
        }
    }

    #[must_use]
    pub fn current(&self) -> Arc<HostRegistry> {
        self.current
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .registry
            .clone()
    }

    fn snapshot(&self) -> (u64, Arc<HostRegistry>) {
        let current = self.current.read().unwrap_or_else(|error| error.into_inner());
        (current.generation, current.registry.clone())
    }

    pub(crate) fn staged(&self) -> Option<(Arc<HostRegistry>, Arc<TaskRegistryImage>)> {
        self.staged
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(|state| (state.registry.clone(), state.image.clone()))
    }

    fn stage(&self, registry: Arc<HostRegistry>, image: Arc<TaskRegistryImage>) -> Result<(), ()> {
        let mut staged = self.staged.write().map_err(|_| ())?;
        if staged.is_some() {
            return Err(());
        }
        *staged = Some(StagedTask { registry, image });
        Ok(())
    }

    fn clear_stage(&self, registry: &Arc<HostRegistry>) {
        let mut staged = self.staged.write().unwrap_or_else(|error| error.into_inner());
        if staged
            .as_ref()
            .is_some_and(|value| Arc::ptr_eq(&value.registry, registry))
        {
            *staged = None;
        }
    }

    fn replace(&self, expected: u64, registry: Arc<HostRegistry>) -> Result<(u64, Arc<HostRegistry>), ()> {
        let mut current = self.current.write().unwrap_or_else(|error| error.into_inner());
        if current.generation != expected {
            return Err(());
        }
        let previous = std::mem::replace(&mut current.registry, registry);
        current.generation = current.generation.wrapping_add(1).max(1);
        Ok((current.generation, previous))
    }

    fn rollback(&self, expected: u64, generation: u64, registry: Arc<HostRegistry>) -> Result<(), ()> {
        let mut current = self.current.write().unwrap_or_else(|error| error.into_inner());
        if current.generation != expected {
            return Err(());
        }
        current.registry = registry;
        current.generation = generation;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn test_publish(&self, registry: Arc<HostRegistry>) -> Result<(), ()> {
        let (generation, _) = self.snapshot();
        self.replace(generation, registry).map(|_| ())
    }
}

struct RestoreState {
    previous: Arc<HostRegistry>,
    replacement: Arc<HostRegistry>,
    external: Box<dyn TaskExternalRestore>,
    previous_generation: u64,
    committed_generation: Option<u64>,
    seccomp: Option<SeccompRestoreTransaction>,
}

pub struct Participant {
    registry: Arc<Registry>,
    external: Arc<dyn TaskExternalCheckpoint>,
    codec: Arc<dyn Codec>,
    frozen: Mutex<Option<Arc<HostRegistry>>>,
    staged: Mutex<BTreeMap<u64, RestoreState>>,
    next: AtomicU64,
    seccomp: Option<Arc<SeccompControl>>,
}

impl Participant {
    #[must_use]
    pub fn new(registry: Arc<Registry>, external: Arc<dyn TaskExternalCheckpoint>, codec: Arc<dyn Codec>) -> Self {
        Self {
            registry,
            external,
            codec,
            frozen: Mutex::new(None),
            staged: Mutex::new(BTreeMap::new()),
            next: AtomicU64::new(1),
            seccomp: None,
        }
    }

    #[must_use]
    pub fn with_seccomp(mut self, seccomp: Arc<SeccompControl>) -> Self {
        self.seccomp = Some(seccomp);
        self
    }

    fn encode_section(&self, image: &TaskRegistryImage) -> Result<Vec<u8>, ()> {
        let task = self.codec.encode(image)?;
        let Some(seccomp) = &self.seccomp else { return Ok(task) };
        let policy = super::seccomp::Wire::encode(&seccomp.snapshot())?;
        let task_length = u32::try_from(task.len()).map_err(|_| ())?;
        let mut bytes = Vec::with_capacity(8 + task.len() + policy.len());
        bytes.extend_from_slice(b"TSCP");
        bytes.extend_from_slice(&task_length.to_le_bytes());
        bytes.extend_from_slice(&task);
        bytes.extend_from_slice(&policy);
        Ok(bytes)
    }

    fn decode_section(&self, bytes: &[u8]) -> Result<(TaskRegistryImage, Option<crate::SeccompPolicySnapshot>), ()> {
        let Some(_) = &self.seccomp else {
            return Ok((self.codec.decode(bytes)?, None));
        };
        if bytes.len() < 8 || &bytes[..4] != b"TSCP" {
            return Err(());
        }
        let task_length = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        let split = 8_usize.checked_add(task_length).ok_or(())?;
        if split > bytes.len() {
            return Err(());
        }
        let task = self.codec.decode(&bytes[8..split])?;
        let policy = super::seccomp::Wire::decode(&bytes[split..])?;
        let threads = task
            .threads
            .iter()
            .map(|thread| thread.thread)
            .collect::<std::collections::BTreeSet<_>>();
        let policy_threads = policy
            .policies
            .iter()
            .map(|(thread, _)| *thread)
            .collect::<std::collections::BTreeSet<_>>();
        if threads != policy_threads || policy.policies.len() != policy_threads.len() {
            return Err(());
        }
        Ok((task, Some(policy)))
    }

    fn stage_seccomp(
        &self,
        snapshot: Option<crate::SeccompPolicySnapshot>,
    ) -> Result<Option<SeccompRestoreTransaction>, ()> {
        let Some(control) = &self.seccomp else {
            return if snapshot.is_none() { Ok(None) } else { Err(()) };
        };
        let snapshot = snapshot.ok_or(())?;
        control.freeze_checkpoint().map_err(|_| ())?;
        match control.stage_checkpoint(&snapshot) {
            Ok(transaction) => Ok(Some(transaction)),
            Err(_) => {
                control.thaw_checkpoint();
                Err(())
            }
        }
    }

    fn thaw_seccomp(&self) {
        if let Some(control) = &self.seccomp {
            control.thaw_checkpoint();
        }
    }
}

impl CheckpointParticipant for Participant {
    fn role(&self) -> CheckpointRole {
        CheckpointRole::Task
    }

    fn version(&self) -> u32 {
        TASK_CHECKPOINT_VERSION
    }

    fn dependencies(&self) -> &[CheckpointRole] {
        &[]
    }

    fn freeze(&self) -> Result<(), ()> {
        if self.frozen.lock().map_err(|_| ())?.is_some() {
            return Err(());
        }
        let registry = self.registry.current();
        registry.freeze_checkpoint();
        if let Some(seccomp) = &self.seccomp {
            if seccomp.freeze_checkpoint().is_err() {
                registry.thaw_checkpoint();
                return Err(());
            }
        }
        *self.frozen.lock().map_err(|_| ())? = Some(registry);
        Ok(())
    }

    fn snapshot(&self) -> Result<Vec<u8>, ()> {
        let frozen = self.frozen.lock().map_err(|_| ())?;
        let registry = frozen.as_ref().ok_or(())?;
        let image = registry.image(self.external.as_ref()).map_err(|_| ())?;
        self.encode_section(&image)
    }

    fn thaw(&self) -> Result<(), ()> {
        let registry = self.frozen.lock().map_err(|_| ())?.take().ok_or(())?;
        if let Some(seccomp) = &self.seccomp {
            seccomp.thaw_checkpoint();
        }
        registry.thaw_checkpoint();
        Ok(())
    }

    fn validate(&self, _: &CheckpointImage, section: &Section) -> Result<(), ()> {
        self.decode_section(section.bytes())?.0.validate().map_err(|_| ())
    }

    fn stage(&self, section: &Section) -> Result<u64, ()> {
        let (previous_generation, previous) = self.registry.snapshot();
        previous.freeze_checkpoint();
        let result = (|| {
            let (image, seccomp_image) = self.decode_section(section.bytes())?;
            let image = Arc::new(image);
            image.validate().map_err(|_| ())?;
            let replacement = Arc::new(HostRegistry::restore(&image.registry).map_err(|_| ())?);
            replacement.freeze_checkpoint();
            let reservation = self.next.fetch_add(1, Ordering::Relaxed);
            if reservation == 0 {
                replacement.thaw_checkpoint();
                return Err(());
            }
            let mut external = self.external.stage(&image).map_err(|_| ())?;
            let seccomp = self.stage_seccomp(seccomp_image)?;
            if self.registry.stage(replacement.clone(), image).is_err() {
                self.thaw_seccomp();
                external.rollback();
                replacement.thaw_checkpoint();
                return Err(());
            }
            let mut staged = match self.staged.lock() {
                Ok(staged) => staged,
                Err(_) => {
                    self.registry.clear_stage(&replacement);
                    self.thaw_seccomp();
                    external.rollback();
                    replacement.thaw_checkpoint();
                    return Err(());
                }
            };
            staged.insert(
                reservation,
                RestoreState {
                    previous: previous.clone(),
                    replacement,
                    external,
                    previous_generation,
                    committed_generation: None,
                    seccomp,
                },
            );
            Ok(reservation)
        })();
        if result.is_err() {
            previous.thaw_checkpoint();
        }
        result
    }

    fn commit(&self, reservation: u64) -> Result<(), ()> {
        let mut staged = self.staged.lock().map_err(|_| ())?;
        let state = staged.get_mut(&reservation).ok_or(())?;
        if let (Some(control), Some(transaction)) = (&self.seccomp, &mut state.seccomp) {
            control.commit_checkpoint(transaction).map_err(|_| ())?;
        }
        state.external.commit().map_err(|_| ())?;
        let (generation, previous) = self
            .registry
            .replace(state.previous_generation, state.replacement.clone())?;
        if !Arc::ptr_eq(&previous, &state.previous) {
            let _ = self.registry.rollback(generation, state.previous_generation, previous);
            state.external.rollback();
            if let (Some(control), Some(transaction)) = (&self.seccomp, &mut state.seccomp) {
                control.rollback_checkpoint(transaction);
            }
            return Err(());
        }
        state.committed_generation = Some(generation);
        Ok(())
    }

    fn rollback(&self, reservation: u64) {
        let state = self
            .staged
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&reservation);
        if let Some(mut state) = state {
            self.registry.clear_stage(&state.replacement);
            if let Some(generation) = state.committed_generation {
                let _ = self
                    .registry
                    .rollback(generation, state.previous_generation, state.previous.clone());
            }
            state.external.rollback();
            if let (Some(control), Some(transaction)) = (&self.seccomp, &mut state.seccomp) {
                control.rollback_checkpoint(transaction);
                control.thaw_checkpoint();
            }
            state.previous.thaw_checkpoint();
            state.replacement.thaw_checkpoint();
        }
    }

    fn resume(&self, reservation: u64) -> Result<(), ()> {
        let mut staged = self.staged.lock().map_err(|_| ())?;
        let state = staged.get_mut(&reservation).ok_or(())?;
        if state.committed_generation.is_none() {
            return Err(());
        }
        state.external.resume().map_err(|_| ())?;
        if let (Some(control), Some(transaction)) = (&self.seccomp, &state.seccomp) {
            control.resume_checkpoint(transaction).map_err(|_| ())?;
            control.thaw_checkpoint();
        }
        self.registry.clear_stage(&state.replacement);
        state.replacement.thaw_checkpoint();
        state.previous.thaw_checkpoint();
        staged.remove(&reservation);
        Ok(())
    }
}
