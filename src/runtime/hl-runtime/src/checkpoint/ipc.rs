use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use hl_checkpoint::{CheckpointImage, Section};
use hl_ipc::{
    IPC_CHECKPOINT_VERSION, IpcCatalog as HostIpcCatalog, IpcCatalogRestore, IpcCheckpointImage, IpcCheckpointRebind,
};

use crate::{CheckpointParticipant, CheckpointRole};

mod binding;
mod rebind;
mod registry;
mod wire;

pub use binding::PipeBindings;
pub use rebind::ResourceRebind;
pub use registry::{OpenPipe, PipeRegistry, Publication as PipePublication, RegistryError};

pub const IPC_CHECKPOINT_BYTES_MAXIMUM: usize = 4 * 1024 * 1024;

const DEPENDENCIES: [CheckpointRole; 6] = [
    CheckpointRole::Task,
    CheckpointRole::Descriptors,
    CheckpointRole::Memory,
    CheckpointRole::Provider,
    CheckpointRole::Event,
    CheckpointRole::Network,
];

pub trait IpcCheckpointCodec: Send + Sync {
    fn encode(&self, image: &IpcCheckpointImage) -> Result<Vec<u8>, ()>;
    fn decode(&self, bytes: &[u8]) -> Result<IpcCheckpointImage, ()>;
}

#[derive(Default)]
pub struct PortableIpcCodec;

impl IpcCheckpointCodec for PortableIpcCodec {
    fn encode(&self, image: &IpcCheckpointImage) -> Result<Vec<u8>, ()> {
        wire::Codec::encode(image)
    }

    fn decode(&self, bytes: &[u8]) -> Result<IpcCheckpointImage, ()> {
        wire::Codec::decode(bytes)
    }
}

pub struct IpcCatalog {
    current: RwLock<Arc<HostIpcCatalog>>,
}

impl IpcCatalog {
    #[must_use]
    pub const fn new(catalog: Arc<HostIpcCatalog>) -> Self {
        Self {
            current: RwLock::new(catalog),
        }
    }

    #[must_use]
    pub fn current(&self) -> Arc<HostIpcCatalog> {
        self.current
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn replace(&self, catalog: Arc<HostIpcCatalog>) -> Arc<HostIpcCatalog> {
        let mut current = self.current.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        std::mem::replace(&mut *current, catalog)
    }
}

struct RestoreState {
    previous: Arc<HostIpcCatalog>,
    replacement: Arc<HostIpcCatalog>,
    external: Box<dyn IpcCatalogRestore>,
    committed: bool,
    resumed: bool,
}

pub struct IpcCheckpointParticipant {
    catalog: Arc<IpcCatalog>,
    rebind: Arc<dyn IpcCheckpointRebind>,
    codec: Arc<dyn IpcCheckpointCodec>,
    frozen: Mutex<Option<Arc<HostIpcCatalog>>>,
    staged: Mutex<BTreeMap<u64, RestoreState>>,
    next: AtomicU64,
}

impl IpcCheckpointParticipant {
    #[must_use]
    pub fn new(
        catalog: Arc<IpcCatalog>,
        rebind: Arc<dyn IpcCheckpointRebind>,
        codec: Arc<dyn IpcCheckpointCodec>,
    ) -> Self {
        Self {
            catalog,
            rebind,
            codec,
            frozen: Mutex::new(None),
            staged: Mutex::new(BTreeMap::new()),
            next: AtomicU64::new(1),
        }
    }

    fn stage_image(&self, previous: &Arc<HostIpcCatalog>, image: &IpcCheckpointImage) -> Result<u64, ()> {
        let mut external = self.rebind.stage(image).map_err(|_| ())?;
        let Ok(replacement) = HostIpcCatalog::restore_checkpoint(image, external.as_mut()) else {
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
                committed: false,
                resumed: false,
            },
        );
        Ok(reservation)
    }
}

impl CheckpointParticipant for IpcCheckpointParticipant {
    fn role(&self) -> CheckpointRole {
        CheckpointRole::Ipc
    }
    fn version(&self) -> u32 {
        IPC_CHECKPOINT_VERSION
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
        let previous = self.catalog.current();
        previous.freeze_checkpoint();
        let result = (|| {
            let image = self.codec.decode(section.bytes())?;
            image.validate().map_err(|_| ())?;
            self.stage_image(&previous, &image)
        })();
        if result.is_err() {
            previous.thaw_checkpoint();
        }
        result
    }

    fn commit(&self, reservation: u64) -> Result<(), ()> {
        let mut staged = self.staged.lock().map_err(|_| ())?;
        let state = staged.get_mut(&reservation).ok_or(())?;
        state.external.commit().map_err(|_| ())?;
        let previous = self.catalog.replace(state.replacement.clone());
        if !Arc::ptr_eq(&previous, &state.previous) {
            self.catalog.replace(previous);
            state.external.rollback();
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
            if state.committed {
                self.catalog.replace(state.previous.clone());
            }
            state.external.rollback();
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
        state.external.resume().map_err(|_| ())?;
        state.replacement.thaw_checkpoint();
        state.previous.thaw_checkpoint();
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
