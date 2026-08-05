use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use hl_checkpoint::{CheckpointImage, Section};
use hl_memory::{
    MEMORY_CHECKPOINT_VERSION, MappingCoordinator, MappingHost, MemoryCheckpointHost, MemoryCheckpointImage,
    MemoryHostRestore, SharedObjectStore,
};

use crate::{CheckpointParticipant, CheckpointRole};

const DEPENDENCIES: [CheckpointRole; 1] = [CheckpointRole::Task];

pub trait MemoryCheckpointCodec: Send + Sync {
    fn encode(&self, image: &MemoryCheckpointImage) -> Result<Vec<u8>, ()>;
    fn decode(&self, bytes: &[u8]) -> Result<MemoryCheckpointImage, ()>;
}

pub trait MemoryResourceRestore: Send + Sync {
    fn stage(&self, shared: Arc<SharedObjectStore>) -> Result<Box<dyn MemoryResourceTransaction>, ()>;
}

pub trait MemoryResourceTransaction: Send {
    fn commit(&mut self) -> Result<(), ()>;
    fn rollback(&mut self);
    fn resume(&mut self) -> Result<(), ()>;
    fn finish(&mut self) {}
}

#[derive(Default)]
pub struct PortableMemoryCodec;

impl MemoryCheckpointCodec for PortableMemoryCodec {
    fn encode(&self, image: &MemoryCheckpointImage) -> Result<Vec<u8>, ()> {
        super::memory_wire::MemoryWire::encode(image)
    }

    fn decode(&self, bytes: &[u8]) -> Result<MemoryCheckpointImage, ()> {
        super::memory_wire::MemoryWire::decode(bytes)
    }
}

pub struct Memory<H> {
    pub coordinator: Arc<MappingCoordinator<H>>,
    pub shared: Arc<SharedObjectStore>,
}

impl<H> Memory<H> {
    #[must_use]
    pub const fn new(coordinator: Arc<MappingCoordinator<H>>, shared: Arc<SharedObjectStore>) -> Self {
        Self { coordinator, shared }
    }
}

impl<H: MappingHost> Memory<H> {
    fn freeze_checkpoint(&self) {
        self.coordinator.freeze_checkpoint();
        self.shared.freeze_checkpoint();
    }

    fn thaw_checkpoint(&self) {
        self.shared.thaw_checkpoint();
        self.coordinator.thaw_checkpoint();
    }
}

pub struct MemoryState<H> {
    current: RwLock<Arc<Memory<H>>>,
    staged: RwLock<Vec<Arc<Memory<H>>>>,
}

impl<H> MemoryState<H> {
    #[must_use]
    pub const fn new(memory: Arc<Memory<H>>) -> Self {
        Self {
            current: RwLock::new(memory),
            staged: RwLock::new(Vec::new()),
        }
    }

    #[must_use]
    pub fn current(&self) -> Arc<Memory<H>> {
        self.current
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn staged(&self) -> Option<Arc<Memory<H>>> {
        let staged = self.staged.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        (staged.len() == 1).then(|| staged[0].clone())
    }

    fn stage(&self, memory: Arc<Memory<H>>) -> Result<(), ()> {
        let mut staged = self.staged.write().map_err(|_| ())?;
        if staged.iter().any(|value| Arc::ptr_eq(value, &memory)) {
            return Err(());
        }
        staged.push(memory);
        Ok(())
    }

    fn clear_stage(&self, memory: &Arc<Memory<H>>) {
        let mut staged = self.staged.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        staged.retain(|value| !Arc::ptr_eq(value, memory));
    }

    fn replace(&self, memory: Arc<Memory<H>>) -> Arc<Memory<H>> {
        let mut current = self.current.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        std::mem::replace(&mut *current, memory)
    }
}

struct RestoreState<H: MappingHost> {
    previous: Arc<Memory<H>>,
    replacement: Arc<Memory<H>>,
    host: Box<dyn MemoryHostRestore<H>>,
    resources: Option<Box<dyn MemoryResourceTransaction>>,
    committed: bool,
    resumed: bool,
}

pub struct MemoryCheckpointParticipant<H: MappingHost> {
    memory: Arc<MemoryState<H>>,
    host: Arc<dyn MemoryCheckpointHost<H>>,
    codec: Arc<dyn MemoryCheckpointCodec>,
    frozen: Mutex<Option<Arc<Memory<H>>>>,
    staged: Mutex<BTreeMap<u64, RestoreState<H>>>,
    next: AtomicU64,
    futex: Option<Arc<dyn crate::RuntimeFutexPort>>,
    resources: Option<Arc<dyn MemoryResourceRestore>>,
}

impl<H: MappingHost> MemoryCheckpointParticipant<H> {
    #[must_use]
    pub fn new(
        memory: Arc<MemoryState<H>>,
        host: Arc<dyn MemoryCheckpointHost<H>>,
        codec: Arc<dyn MemoryCheckpointCodec>,
    ) -> Self {
        Self {
            memory,
            host,
            codec,
            frozen: Mutex::new(None),
            staged: Mutex::new(BTreeMap::new()),
            next: AtomicU64::new(1),
            futex: None,
            resources: None,
        }
    }

    #[must_use]
    pub fn with_futex_quiescence(mut self, futex: Arc<dyn crate::RuntimeFutexPort>) -> Self {
        self.futex = Some(futex);
        self
    }

    #[must_use]
    pub fn with_resources(mut self, resources: Arc<dyn MemoryResourceRestore>) -> Self {
        self.resources = Some(resources);
        self
    }
}

impl<H: MappingHost + 'static> CheckpointParticipant for MemoryCheckpointParticipant<H> {
    fn role(&self) -> CheckpointRole {
        CheckpointRole::Memory
    }

    fn version(&self) -> u32 {
        MEMORY_CHECKPOINT_VERSION
    }

    fn dependencies(&self) -> &[CheckpointRole] {
        &DEPENDENCIES
    }

    fn freeze(&self) -> Result<(), ()> {
        if self.futex.as_ref().is_some_and(|futex| !futex.checkpoint_quiescent()) {
            return Err(());
        }
        if self.frozen.lock().map_err(|_| ())?.is_some() {
            return Err(());
        }
        let memory = self.memory.current();
        memory.freeze_checkpoint();
        let mut frozen = self.frozen.lock().map_err(|_| ())?;
        if frozen.is_some() {
            memory.thaw_checkpoint();
            return Err(());
        }
        *frozen = Some(memory);
        Ok(())
    }

    fn snapshot(&self) -> Result<Vec<u8>, ()> {
        let frozen = self.frozen.lock().map_err(|_| ())?;
        let memory = frozen.as_ref().ok_or(())?;
        let image = memory
            .coordinator
            .checkpoint_image(self.host.as_ref())
            .map_err(|_| ())?;
        self.codec.encode(&image)
    }

    fn thaw(&self) -> Result<(), ()> {
        let memory = self.frozen.lock().map_err(|_| ())?.take().ok_or(())?;
        memory.thaw_checkpoint();
        Ok(())
    }

    fn validate(&self, _: &CheckpointImage, section: &Section) -> Result<(), ()> {
        self.codec.decode(section.bytes())?.validate().map_err(|_| ())
    }

    fn stage(&self, section: &Section) -> Result<u64, ()> {
        let previous = self.memory.current();
        previous.freeze_checkpoint();
        let result = (|| {
            let image = self.codec.decode(section.bytes())?;
            image.validate().map_err(|_| ())?;
            let mut staged = self.host.stage(&image).map_err(|_| ())?;
            let coordinator =
                match MappingCoordinator::restore(staged.mapping, staged.shared.clone(), image.ledger.clone()) {
                    Ok(coordinator) => Arc::new(coordinator),
                    Err(_) => {
                        staged.restore.rollback();
                        return Err(());
                    }
                };
            let replacement = Arc::new(Memory::new(coordinator, staged.shared));
            let mut host = staged.restore;
            if host.bind(Arc::clone(&replacement.coordinator)).is_err() {
                host.rollback();
                return Err(());
            }
            let mut resources = match &self.resources {
                Some(resources) => match resources.stage(Arc::clone(&replacement.shared)) {
                    Ok(transaction) => Some(transaction),
                    Err(()) => {
                        host.rollback();
                        return Err(());
                    }
                },
                None => None,
            };
            replacement.freeze_checkpoint();
            if self.memory.stage(replacement.clone()).is_err() {
                if let Some(resources) = &mut resources {
                    resources.rollback();
                }
                host.rollback();
                replacement.thaw_checkpoint();
                return Err(());
            }
            let reservation = self.next.fetch_add(1, Ordering::Relaxed);
            if reservation == 0 {
                self.memory.clear_stage(&replacement);
                if let Some(resources) = &mut resources {
                    resources.rollback();
                }
                host.rollback();
                replacement.thaw_checkpoint();
                return Err(());
            }
            let mut states = match self.staged.lock() {
                Ok(states) => states,
                Err(_) => {
                    self.memory.clear_stage(&replacement);
                    if let Some(resources) = &mut resources {
                        resources.rollback();
                    }
                    host.rollback();
                    replacement.thaw_checkpoint();
                    return Err(());
                }
            };
            states.insert(
                reservation,
                RestoreState {
                    previous: previous.clone(),
                    replacement,
                    host,
                    resources,
                    committed: false,
                    resumed: false,
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
        state.host.commit().map_err(|_| ())?;
        if let Some(resources) = &mut state.resources {
            if resources.commit().is_err() {
                state.host.rollback();
                return Err(());
            }
        }
        let previous = self.memory.replace(state.replacement.clone());
        if !Arc::ptr_eq(&previous, &state.previous) {
            self.memory.replace(previous);
            if let Some(resources) = &mut state.resources {
                resources.rollback();
            }
            state.host.rollback();
            return Err(());
        }
        state.committed = true;
        Ok(())
    }

    fn rollback(&self, reservation: u64) {
        let state = self
            .staged
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&reservation);
        if let Some(mut state) = state {
            self.memory.clear_stage(&state.replacement);
            if state.committed {
                self.memory.replace(state.previous.clone());
            }
            if let Some(resources) = &mut state.resources {
                resources.rollback();
            }
            state.host.rollback();
            if !state.resumed {
                state.previous.thaw_checkpoint();
                state.replacement.thaw_checkpoint();
            }
        }
    }

    fn resume(&self, reservation: u64) -> Result<(), ()> {
        let mut staged = self.staged.lock().map_err(|_| ())?;
        let state = staged.get_mut(&reservation).ok_or(())?;
        if !state.committed {
            return Err(());
        }
        state.host.resume().map_err(|_| ())?;
        if let Some(resources) = &mut state.resources {
            resources.resume()?;
        }
        self.memory.clear_stage(&state.replacement);
        state.replacement.thaw_checkpoint();
        state.previous.thaw_checkpoint();
        state.resumed = true;
        Ok(())
    }

    fn finish(&self, reservation: u64) {
        let state = self
            .staged
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&reservation);
        if let Some(mut state) = state {
            if let Some(resources) = &mut state.resources {
                resources.finish();
            }
        }
    }
}
