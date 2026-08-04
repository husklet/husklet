use std::collections::BTreeMap;
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::sync::{Arc, Mutex, Weak};

use hl_memory::{SharedBacking, SharedBackingFactory, SharedError, SharedObjectId};

use super::abi;

#[derive(Debug, Default)]
pub(super) struct Registry {
    objects: Mutex<BTreeMap<SharedObjectId, Weak<Backing>>>,
}

impl Registry {
    pub(super) fn file(&self, id: SharedObjectId) -> Result<File, SharedError> {
        self.objects
            .lock()
            .map_err(|_| SharedError::Io)?
            .get(&id)
            .and_then(Weak::upgrade)
            .ok_or(SharedError::NotFound)?
            .file
            .try_clone()
            .map_err(|_| SharedError::Io)
    }

    fn bind(&self, id: SharedObjectId, backing: &Arc<Backing>) -> Result<(), SharedError> {
        let mut objects = self.objects.lock().map_err(|_| SharedError::Io)?;
        objects.retain(|_, object| object.strong_count() != 0);
        if objects.len() >= 4096 || objects.contains_key(&id) {
            return Err(SharedError::ResourceLimit);
        }
        objects.insert(id, Arc::downgrade(backing));
        Ok(())
    }
}

#[derive(Debug)]
pub(super) struct Factory {
    registry: Arc<Registry>,
}

impl Factory {
    pub(super) fn new(registry: Arc<Registry>) -> Self {
        Self { registry }
    }

    pub(super) fn store(
        limits: hl_memory::SharedLimits,
    ) -> Result<(Arc<hl_memory::SharedObjectStore>, Arc<Registry>), SharedError> {
        let registry = Arc::new(Registry::default());
        let factory = Arc::new(Self::new(Arc::clone(&registry)));
        let store = Arc::new(hl_memory::SharedObjectStore::with_factory(limits, factory)?);
        Ok((store, registry))
    }
}

impl SharedBackingFactory for Factory {
    fn create(&self, id: SharedObjectId, size: usize) -> Result<Arc<dyn SharedBacking>, SharedError> {
        let file = abi::Memfd::create(c"hl-shared").map_err(|()| SharedError::Io)?;
        file.set_len(u64::try_from(size).map_err(|_| SharedError::ResourceLimit)?)
            .map_err(|_| SharedError::Io)?;
        let backing = Arc::new(Backing { file });
        self.registry.bind(id, &backing)?;
        Ok(backing)
    }
}

#[derive(Debug)]
struct Backing {
    file: File,
}

impl SharedBacking for Backing {
    fn len(&self) -> Result<usize, SharedError> {
        usize::try_from(self.file.metadata().map_err(|_| SharedError::Io)?.len())
            .map_err(|_| SharedError::ResourceLimit)
    }

    fn resize(&self, size: usize) -> Result<(), SharedError> {
        self.file
            .set_len(u64::try_from(size).map_err(|_| SharedError::ResourceLimit)?)
            .map_err(|_| SharedError::Io)
    }

    fn read(&self, offset: usize, output: &mut [u8]) -> Result<(), SharedError> {
        self.file
            .read_exact_at(output, u64::try_from(offset).map_err(|_| SharedError::Range)?)
            .map_err(|_| SharedError::Io)
    }

    fn write(&self, offset: usize, input: &[u8]) -> Result<(), SharedError> {
        self.file
            .write_all_at(input, u64::try_from(offset).map_err(|_| SharedError::Range)?)
            .map_err(|_| SharedError::Io)
    }
}
