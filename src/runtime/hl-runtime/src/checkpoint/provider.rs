use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use hl_checkpoint::{CheckpointImage, Section};
use hl_provider::{
    HandleNamespace, PROVIDER_CHECKPOINT_VERSION, ProviderCheckpointCapture, ProviderCheckpointImage,
    ProviderCheckpointReconnect, ProviderRemoteRestore,
};

use crate::{CheckpointParticipant, CheckpointRole};

mod registry;
mod transaction;
mod wire;

pub use registry::{Error as ProviderRegistryError, Lease as ProviderLease, Registry as ProviderRegistry};
pub use wire::{PROVIDER_CHECKPOINT_BYTES_MAXIMUM, PortableProviderCodec};

const DEPENDENCIES: [CheckpointRole; 3] = [
    CheckpointRole::Task,
    CheckpointRole::Descriptors,
    CheckpointRole::Memory,
];

pub trait ProviderCheckpointCodec: Send + Sync {
    fn encode(&self, image: &ProviderCheckpointImage) -> Result<Vec<u8>, ()>;
    fn decode(&self, bytes: &[u8]) -> Result<ProviderCheckpointImage, ()>;
}

struct NamespaceState {
    generation: u64,
    namespace: Arc<HandleNamespace>,
}

#[derive(Clone)]
struct NamespaceLease {
    generation: u64,
    namespace: Arc<HandleNamespace>,
}

pub struct ProviderNamespace {
    current: RwLock<NamespaceState>,
}

impl ProviderNamespace {
    #[must_use]
    pub const fn new(namespace: Arc<HandleNamespace>) -> Self {
        Self {
            current: RwLock::new(NamespaceState {
                generation: 1,
                namespace,
            }),
        }
    }

    #[must_use]
    pub fn current(&self) -> Arc<HandleNamespace> {
        self.lease().namespace
    }

    fn lease(&self) -> NamespaceLease {
        let current = self.current.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        NamespaceLease {
            generation: current.generation,
            namespace: current.namespace.clone(),
        }
    }

    fn replace(&self, expected: u64, namespace: Arc<HandleNamespace>) -> Result<(Arc<HandleNamespace>, u64), ()> {
        let mut current = self.current.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        if current.generation != expected {
            return Err(());
        }
        let generation = current.generation.checked_add(1).ok_or(())?;
        let previous = std::mem::replace(&mut current.namespace, namespace);
        current.generation = generation;
        Ok((previous, current.generation))
    }
}

struct RestoreState {
    previous: NamespaceLease,
    replacement: Arc<HandleNamespace>,
    remote: Box<dyn ProviderRemoteRestore>,
    committed: Option<u64>,
    resumed: bool,
}

pub struct ProviderCheckpointParticipant {
    namespace: Arc<ProviderNamespace>,
    capture: Arc<dyn ProviderCheckpointCapture>,
    reconnect: Arc<dyn ProviderCheckpointReconnect>,
    codec: Arc<dyn ProviderCheckpointCodec>,
    frozen: Mutex<Option<Arc<HandleNamespace>>>,
    staged: Mutex<BTreeMap<u64, RestoreState>>,
    next: AtomicU64,
}

impl ProviderCheckpointParticipant {
    #[must_use]
    pub fn new(
        namespace: Arc<ProviderNamespace>,
        capture: Arc<dyn ProviderCheckpointCapture>,
        reconnect: Arc<dyn ProviderCheckpointReconnect>,
        codec: Arc<dyn ProviderCheckpointCodec>,
    ) -> Self {
        Self {
            namespace,
            capture,
            reconnect,
            codec,
            frozen: Mutex::new(None),
            staged: Mutex::new(BTreeMap::new()),
            next: AtomicU64::new(1),
        }
    }

    fn stage_image(&self, previous: &NamespaceLease, image: &ProviderCheckpointImage) -> Result<u64, ()> {
        let mut remote = self.reconnect.stage(image).map_err(|_| ())?;
        let remotes = image
            .resources
            .iter()
            .map(|resource| remote.remote(resource.key).map(|value| (resource.slot, value)))
            .collect::<Result<Vec<_>, _>>();
        let Ok(remotes) = remotes else {
            remote.rollback();
            return Err(());
        };
        let replacement = match HandleNamespace::restore_checkpoint(&image.namespace, &remotes) {
            Ok(namespace) => Arc::new(namespace),
            Err(_) => {
                remote.rollback();
                return Err(());
            }
        };
        replacement.freeze_checkpoint();
        let reservation = self.next.fetch_add(1, Ordering::Relaxed);
        if reservation == 0 {
            replacement.thaw_checkpoint();
            remote.rollback();
            return Err(());
        }
        self.staged.lock().map_err(|_| ())?.insert(
            reservation,
            RestoreState {
                previous: previous.clone(),
                replacement,
                remote,
                committed: None,
                resumed: false,
            },
        );
        Ok(reservation)
    }
}

impl CheckpointParticipant for ProviderCheckpointParticipant {
    fn role(&self) -> CheckpointRole {
        CheckpointRole::Provider
    }

    fn version(&self) -> u32 {
        PROVIDER_CHECKPOINT_VERSION
    }

    fn dependencies(&self) -> &[CheckpointRole] {
        &DEPENDENCIES
    }

    fn freeze(&self) -> Result<(), ()> {
        if self.frozen.lock().map_err(|_| ())?.is_some() {
            return Err(());
        }
        self.capture.freeze().map_err(|_| ())?;
        let namespace = self.namespace.current();
        namespace.freeze_checkpoint();
        *self.frozen.lock().map_err(|_| ())? = Some(namespace);
        Ok(())
    }

    fn snapshot(&self) -> Result<Vec<u8>, ()> {
        let frozen = self.frozen.lock().map_err(|_| ())?;
        let namespace = frozen.as_ref().ok_or(())?;
        let image = ProviderCheckpointImage::capture(namespace, self.capture.as_ref()).map_err(|_| ())?;
        self.codec.encode(&image)
    }

    fn thaw(&self) -> Result<(), ()> {
        let namespace = self.frozen.lock().map_err(|_| ())?.take().ok_or(())?;
        namespace.thaw_checkpoint();
        self.capture.thaw();
        Ok(())
    }

    fn validate(&self, _: &CheckpointImage, section: &Section) -> Result<(), ()> {
        self.codec.decode(section.bytes())?.validate().map_err(|_| ())
    }

    fn stage(&self, section: &Section) -> Result<u64, ()> {
        let previous = self.namespace.lease();
        previous.namespace.freeze_checkpoint();
        let result = (|| {
            let image = self.codec.decode(section.bytes())?;
            image.validate().map_err(|_| ())?;
            self.stage_image(&previous, &image)
        })();
        if result.is_err() {
            previous.namespace.thaw_checkpoint();
        }
        result
    }

    fn commit(&self, reservation: u64) -> Result<(), ()> {
        let mut staged = self.staged.lock().map_err(|_| ())?;
        let state = staged.get_mut(&reservation).ok_or(())?;
        state.remote.commit().map_err(|_| ())?;
        let swapped = self
            .namespace
            .replace(state.previous.generation, state.replacement.clone());
        let Ok((previous, generation)) = swapped else {
            return Err(());
        };
        if !Arc::ptr_eq(&previous, &state.previous.namespace) {
            let _ = self.namespace.replace(generation, previous);
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
                let _ = self.namespace.replace(generation, state.previous.namespace.clone());
            }
            state.remote.rollback();
            if !state.resumed {
                state.previous.namespace.thaw_checkpoint();
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
        state.remote.resume().map_err(|_| ())?;
        state.replacement.thaw_checkpoint();
        state.previous.namespace.thaw_checkpoint();
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
