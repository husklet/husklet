use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use hl_checkpoint::{CheckpointImage, Section};
use hl_network::{
    NETWORK_CHECKPOINT_VERSION, NetworkCatalog as HostNetworkCatalog, NetworkCatalogRestore, NetworkCheckpointImage,
    NetworkCheckpointRebind,
};

use crate::{CheckpointParticipant, CheckpointRole};

mod binding;
mod wire;
pub use binding::{CheckpointHost, ObjectBindings, ReconnectedSocket};
pub use wire::{NETWORK_CHECKPOINT_BYTES_MAXIMUM, PortableNetworkCodec};

const DEPENDENCIES: [CheckpointRole; 5] = [
    CheckpointRole::Task,
    CheckpointRole::Descriptors,
    CheckpointRole::Memory,
    CheckpointRole::Provider,
    CheckpointRole::Event,
];

pub trait NetworkCheckpointCodec: Send + Sync {
    fn encode(&self, image: &NetworkCheckpointImage) -> Result<Vec<u8>, ()>;
    fn decode(&self, bytes: &[u8]) -> Result<NetworkCheckpointImage, ()>;
}

struct CatalogState {
    generation: u64,
    catalog: Arc<HostNetworkCatalog>,
}

#[derive(Clone)]
struct CatalogLease {
    generation: u64,
    catalog: Arc<HostNetworkCatalog>,
}

pub struct NetworkCatalog {
    current: RwLock<CatalogState>,
}

impl NetworkCatalog {
    #[must_use]
    pub const fn new(catalog: Arc<HostNetworkCatalog>) -> Self {
        Self {
            current: RwLock::new(CatalogState { generation: 1, catalog }),
        }
    }

    #[must_use]
    pub fn current(&self) -> Arc<HostNetworkCatalog> {
        self.lease().catalog
    }

    fn lease(&self) -> CatalogLease {
        let current = self.current.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        CatalogLease {
            generation: current.generation,
            catalog: current.catalog.clone(),
        }
    }

    fn replace(&self, expected: u64, catalog: Arc<HostNetworkCatalog>) -> Result<(Arc<HostNetworkCatalog>, u64), ()> {
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

struct NetworkPublication {
    previous: CatalogLease,
    replacement: Arc<HostNetworkCatalog>,
    external: Box<dyn NetworkCatalogRestore>,
    committed: Option<u64>,
    resumed: bool,
}

pub struct NetworkCheckpointParticipant {
    catalog: Arc<NetworkCatalog>,
    rebind: Arc<dyn NetworkCheckpointRebind>,
    codec: Arc<dyn NetworkCheckpointCodec>,
    frozen: Mutex<Option<Arc<HostNetworkCatalog>>>,
    capture: Mutex<Option<NetworkCheckpointImage>>,
    staged: Mutex<BTreeMap<u64, NetworkPublication>>,
    next: AtomicU64,
}

impl NetworkCheckpointParticipant {
    #[must_use]
    pub fn new(
        catalog: Arc<NetworkCatalog>,
        rebind: Arc<dyn NetworkCheckpointRebind>,
        codec: Arc<dyn NetworkCheckpointCodec>,
    ) -> Self {
        Self {
            catalog,
            rebind,
            codec,
            frozen: Mutex::new(None),
            capture: Mutex::new(None),
            staged: Mutex::new(BTreeMap::new()),
            next: AtomicU64::new(1),
        }
    }
}

impl CheckpointParticipant for NetworkCheckpointParticipant {
    fn role(&self) -> CheckpointRole {
        CheckpointRole::Network
    }
    fn version(&self) -> u32 {
        NETWORK_CHECKPOINT_VERSION
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

    fn capture_prepare(&self) -> Result<(), ()> {
        if self.capture.lock().map_err(|_| ())?.is_some() {
            return Err(());
        }
        self.rebind.capture_prepare().map_err(|_| ())
    }

    fn snapshot(&self) -> Result<Vec<u8>, ()> {
        let frozen = self.frozen.lock().map_err(|_| ())?;
        let mut image = frozen.as_ref().ok_or(())?.checkpoint_image().map_err(|_| ())?;
        self.rebind.capture(&mut image).map_err(|_| ())?;
        *self.capture.lock().map_err(|_| ())? = Some(image.clone());
        self.codec.encode(&image)
    }

    fn capture_publish(&self, digest: [u8; 32]) -> Result<(), ()> {
        if self.capture.lock().map_err(|_| ())?.is_none() {
            return Err(());
        }
        self.rebind.capture_publish(digest).map_err(|_| ())
    }

    fn capture_abort(&self) {
        self.capture
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.rebind.capture_abort();
    }

    fn capture_finish(&self) {
        self.capture
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.rebind.capture_finish();
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
        self.stage_bound([0; 32], section)
    }

    fn stage_bound(&self, digest: [u8; 32], section: &Section) -> Result<u64, ()> {
        let previous = self.catalog.lease();
        previous.catalog.freeze_checkpoint();
        let result = (|| {
            let image = self.codec.decode(section.bytes())?;
            image.validate().map_err(|_| ())?;
            let mut external = self.rebind.stage_bound(digest, &image).map_err(|_| ())?;
            let replacement = match HostNetworkCatalog::restore_checkpoint(&image, external.as_mut()) {
                Ok(catalog) => Arc::new(catalog),
                Err(_) => {
                    external.rollback();
                    return Err(());
                }
            };
            if external.bind_catalog(replacement.clone()).is_err() {
                external.rollback();
                return Err(());
            }
            replacement.freeze_checkpoint();
            let reservation = self.next.fetch_add(1, Ordering::Relaxed);
            if reservation == 0 {
                replacement.thaw_checkpoint();
                external.rollback();
                return Err(());
            }
            self.staged.lock().map_err(|_| ())?.insert(
                reservation,
                NetworkPublication {
                    previous: previous.clone(),
                    replacement,
                    external,
                    committed: None,
                    resumed: false,
                },
            );
            Ok(reservation)
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
            return Err(());
        };
        if !Arc::ptr_eq(&previous, &state.previous.catalog) {
            let _ = self.catalog.replace(generation, previous);
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
            if !state.resumed {
                state.previous.catalog.thaw_checkpoint();
                state.replacement.thaw_checkpoint();
            }
            state.external.rollback();
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
