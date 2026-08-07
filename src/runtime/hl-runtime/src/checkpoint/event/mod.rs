use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use hl_checkpoint::{CheckpointImage, Section};
use hl_event::{
    EVENT_CHECKPOINT_VERSION, EventCatalog as HostEventCatalog, EventCatalogRestore, EventCheckpointImage,
    EventCheckpointRebind,
};

use crate::{CheckpointParticipant, CheckpointRole};

mod publication;
mod rebind;
mod registry;
#[cfg(test)]
mod test;
mod wire;

pub use publication::{ObjectBindings, ResourceRestore};
pub use rebind::{BindingRestore, DescriptorRebind, DescriptorReference};
pub use registry::ResourceRegistry;
pub use wire::WireCodec;

const DEPENDENCIES: [CheckpointRole; 4] = [
    CheckpointRole::Task,
    CheckpointRole::Descriptors,
    CheckpointRole::Memory,
    CheckpointRole::Provider,
];

pub trait CheckpointCodec: Send + Sync {
    fn encode(&self, image: &EventCheckpointImage) -> Result<Vec<u8>, ()>;
    fn decode(&self, bytes: &[u8]) -> Result<EventCheckpointImage, ()>;
}

struct CatalogState {
    generation: u64,
    catalog: Arc<HostEventCatalog>,
}

#[derive(Clone)]
struct CatalogLease {
    generation: u64,
    catalog: Arc<HostEventCatalog>,
}

pub struct Catalog {
    current: RwLock<CatalogState>,
}

impl Catalog {
    #[must_use]
    pub const fn new(catalog: Arc<HostEventCatalog>) -> Self {
        Self {
            current: RwLock::new(CatalogState { generation: 1, catalog }),
        }
    }

    #[must_use]
    pub fn current(&self) -> Arc<HostEventCatalog> {
        self.lease().catalog
    }

    fn lease(&self) -> CatalogLease {
        let current = self.current.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        CatalogLease {
            generation: current.generation,
            catalog: current.catalog.clone(),
        }
    }

    fn replace(&self, expected: u64, catalog: Arc<HostEventCatalog>) -> Result<(Arc<HostEventCatalog>, u64), ()> {
        let mut current = self.current.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        if current.generation != expected {
            return Err(());
        }
        let generation = current.generation.checked_add(1).ok_or(())?;
        let previous = std::mem::replace(&mut current.catalog, catalog);
        current.generation = generation;
        Ok((previous, generation))
    }
}

struct RestoreState {
    previous: CatalogLease,
    replacement: Arc<HostEventCatalog>,
    external: Box<dyn EventCatalogRestore>,
    committed: Option<u64>,
    resumed: bool,
}

pub struct Participant {
    catalog: Arc<Catalog>,
    rebind: Arc<dyn EventCheckpointRebind>,
    codec: Arc<dyn CheckpointCodec>,
    frozen: Mutex<Option<Arc<HostEventCatalog>>>,
    staged: Mutex<BTreeMap<u64, RestoreState>>,
    next: AtomicU64,
}

impl Participant {
    #[must_use]
    pub fn new(catalog: Arc<Catalog>, rebind: Arc<dyn EventCheckpointRebind>, codec: Arc<dyn CheckpointCodec>) -> Self {
        Self {
            catalog,
            rebind,
            codec,
            frozen: Mutex::new(None),
            staged: Mutex::new(BTreeMap::new()),
            next: AtomicU64::new(1),
        }
    }

    fn stage_image(&self, previous: &CatalogLease, image: &EventCheckpointImage) -> Result<u64, ()> {
        let mut external = self.rebind.stage(image).map_err(|_| ())?;
        let replacement = if let Ok(catalog) = HostEventCatalog::restore_checkpoint(image, external.as_mut()) {
            Arc::new(catalog)
        } else {
            external.rollback();
            return Err(());
        };
        replacement.freeze_checkpoint();
        let reservation = self.next.fetch_add(1, Ordering::Relaxed);
        if reservation == 0 {
            replacement.thaw_checkpoint();
            external.rollback();
            return Err(());
        }
        self.staged.lock().map_err(|_| ())?.insert(
            reservation,
            RestoreState {
                previous: previous.clone(),
                replacement,
                external,
                committed: None,
                resumed: false,
            },
        );
        Ok(reservation)
    }
}

impl CheckpointParticipant for Participant {
    fn role(&self) -> CheckpointRole {
        CheckpointRole::Event
    }
    fn version(&self) -> u32 {
        EVENT_CHECKPOINT_VERSION
    }
    fn dependencies(&self) -> &[CheckpointRole] {
        &DEPENDENCIES
    }

    fn freeze(&self) -> Result<(), ()> {
        if self.frozen.lock().map_err(|_| ())?.is_some() {
            return Err(());
        }
        let catalog = self.catalog.current();
        catalog.freeze_checkpoint();
        *self.frozen.lock().map_err(|_| ())? = Some(catalog);
        Ok(())
    }

    fn snapshot(&self) -> Result<Vec<u8>, ()> {
        let frozen = self.frozen.lock().map_err(|_| ())?;
        let image = frozen.as_ref().ok_or(())?.checkpoint_image().map_err(|_| ())?;
        self.codec.encode(&image)
    }

    fn thaw(&self) -> Result<(), ()> {
        let catalog = self.frozen.lock().map_err(|_| ())?.take().ok_or(())?;
        catalog.thaw_checkpoint();
        Ok(())
    }

    fn validate(&self, _: &CheckpointImage, section: &Section) -> Result<(), ()> {
        self.codec.decode(section.bytes())?.validate().map_err(|_| ())
    }

    fn stage(&self, section: &Section) -> Result<u64, ()> {
        let previous = self.catalog.lease();
        previous.catalog.freeze_checkpoint();
        let result = (|| {
            let image = self.codec.decode(section.bytes())?;
            image.validate().map_err(|_| ())?;
            self.stage_image(&previous, &image)
        })();
        if result.is_err() {
            previous.catalog.thaw_checkpoint();
        }
        result
    }

    fn commit(&self, reservation: u64) -> Result<(), ()> {
        let mut staged = self.staged.lock().map_err(|_| ())?;
        let state = staged.get_mut(&reservation).ok_or(())?;
        state.external.commit().map_err(|_| ())?;
        let Ok((previous, generation)) = self
            .catalog
            .replace(state.previous.generation, state.replacement.clone())
        else {
            state.external.rollback();
            return Err(());
        };
        if !Arc::ptr_eq(&previous, &state.previous.catalog) {
            let _ = self.catalog.replace(generation, previous);
            state.external.rollback();
            return Err(());
        }
        state.committed = Some(generation);
        Ok(())
    }

    fn rollback(&self, reservation: u64) {
        let state = self
            .staged
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&reservation);
        if let Some(mut state) = state {
            if let Some(generation) = state.committed {
                let _ = self.catalog.replace(generation, state.previous.catalog.clone());
            }
            state.external.rollback();
            if !state.resumed {
                state.previous.catalog.thaw_checkpoint();
                state.replacement.thaw_checkpoint();
            }
        }
    }

    fn resume(&self, reservation: u64) -> Result<(), ()> {
        let mut staged = self.staged.lock().map_err(|_| ())?;
        let state = staged.get_mut(&reservation).ok_or(())?;
        if state.committed.is_none() {
            return Err(());
        }
        state.external.resume().map_err(|_| ())?;
        state.replacement.thaw_checkpoint();
        state.previous.catalog.thaw_checkpoint();
        state.resumed = true;
        Ok(())
    }

    fn finish(&self, reservation: u64) {
        self.staged
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&reservation);
    }
}
