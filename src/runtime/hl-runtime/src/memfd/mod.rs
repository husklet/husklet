use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use hl_descriptor::{DescriptionIdentity, ObjectError};
use hl_memory::{Backing, SharedBackingRef, SharedError, SharedObjectId, SharedObjectStore, SharedSeal};

mod checkpoint;
mod object;
pub use checkpoint::Bindings as MemfdBindings;

#[derive(Debug)]
pub struct Registry {
    state: Mutex<RegistryState>,
}

#[derive(Debug)]
struct RegistryState {
    epoch: u64,
    store: Option<(Arc<SharedObjectStore>, u64)>,
    objects: BTreeMap<DescriptionIdentity, Arc<RuntimeMemfd>>,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            state: Mutex::new(RegistryState {
                epoch: 1,
                store: None,
                objects: BTreeMap::new(),
            }),
        }
    }
}

impl Registry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn register(
        self: &Arc<Self>,
        identity: DescriptionIdentity,
        object: Arc<RuntimeMemfd>,
    ) -> Result<(), ()> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.objects.contains_key(&identity) {
            return Err(());
        }
        state.objects.insert(identity, object.clone());
        object.bind(Arc::downgrade(self), identity, state.epoch);
        Ok(())
    }

    pub(crate) fn configure(&self, store: Arc<SharedObjectStore>, owner: u64) -> Result<(), ()> {
        let mut state = self.state.lock().map_err(|_| ())?;
        match &state.store {
            Some((current, current_owner)) if !Arc::ptr_eq(current, &store) || *current_owner != owner => Err(()),
            Some(_) => Ok(()),
            None => {
                state.store = Some((store, owner));
                Ok(())
            }
        }
    }

    pub(crate) fn create(&self, allow_sealing: bool) -> Result<Arc<RuntimeMemfd>, SharedError> {
        let (store, owner) = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .store
            .as_ref()
            .map(|(store, owner)| (Arc::clone(store), *owner))
            .ok_or(SharedError::NotFound)?;
        RuntimeMemfd::create(store, owner, allow_sealing)
    }

    pub(crate) fn backing(
        &self,
        identity: DescriptionIdentity,
        offset: u64,
        length: u64,
        write_shared: bool,
    ) -> Result<Backing, SharedError> {
        let object = self.object(identity)?;
        let size = object.mapping_size()?;
        let end = offset.checked_add(length).ok_or(SharedError::Range)?;
        if end > size {
            return Err(SharedError::Range);
        }
        Ok(Backing::Shared(SharedBackingRef {
            object: object.id,
            offset: 0,
            length: size,
            write_shared,
        }))
    }

    pub(crate) fn resize(&self, identity: DescriptionIdentity, size: u64) -> Result<(), SharedError> {
        let object = self.object(identity)?;
        object.resize(size)
    }

    pub(crate) fn add_seals(&self, identity: DescriptionIdentity, bits: u8) -> Result<u8, SharedError> {
        let object = self.object(identity)?;
        if !object.allow_sealing {
            return Err(SharedError::Sealed);
        }
        object
            .store
            .add_seals(object.id, SharedSeal::from_bits(bits))
            .map(SharedSeal::bits)
    }

    pub(crate) fn seals(&self, identity: DescriptionIdentity) -> Result<u8, SharedError> {
        let object = self.object(identity)?;
        object
            .store
            .snapshot()
            .objects
            .into_iter()
            .find(|snapshot| snapshot.id == object.id)
            .map(|snapshot| snapshot.seals.bits())
            .ok_or(SharedError::NotFound)
    }

    fn object(&self, identity: DescriptionIdentity) -> Result<Arc<RuntimeMemfd>, SharedError> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .objects
            .get(&identity)
            .cloned()
            .ok_or(SharedError::NotFound)
    }

    fn retire(&self, identity: DescriptionIdentity, epoch: u64) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.epoch == epoch {
            state.objects.remove(&identity);
        }
    }
}

#[derive(Debug)]
pub(crate) struct RuntimeMemfd {
    store: Arc<SharedObjectStore>,
    id: SharedObjectId,
    allow_sealing: bool,
    size: AtomicU64,
    position: Arc<Mutex<Position>>,
    binding: Mutex<Option<(Weak<Registry>, DescriptionIdentity, u64)>>,
    owns_backing: AtomicBool,
    released: AtomicBool,
}

#[derive(Debug)]
struct Position {
    offset: u64,
    splice_reserved: bool,
}

struct PreparedMemfdRead {
    position: Arc<Mutex<Position>>,
    start: u64,
    bytes: Vec<u8>,
    reserved: bool,
}

impl RuntimeMemfd {
    pub(crate) fn create(
        store: Arc<SharedObjectStore>,
        owner: u64,
        allow_sealing: bool,
    ) -> Result<Arc<Self>, SharedError> {
        let id = store.create(owner, 0)?;
        if !allow_sealing {
            store.add_seals(id, SharedSeal::from_bits(SharedSeal::SEAL))?;
        }
        Ok(Arc::new(Self {
            store,
            id,
            allow_sealing,
            size: AtomicU64::new(0),
            position: Arc::new(Mutex::new(Position {
                offset: 0,
                splice_reserved: false,
            })),
            binding: Mutex::new(None),
            owns_backing: AtomicBool::new(true),
            released: AtomicBool::new(false),
        }))
    }

    fn bind(&self, registry: Weak<Registry>, identity: DescriptionIdentity, epoch: u64) {
        *self.binding.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((registry, identity, epoch));
    }

    fn activate_backing(&self) {
        self.owns_backing.store(true, Ordering::Release);
    }

    fn deactivate_backing(&self) {
        self.owns_backing.store(false, Ordering::Release);
    }

    fn size(&self) -> Result<u64, SharedError> {
        self.store
            .snapshot()
            .objects
            .into_iter()
            .find(|snapshot| snapshot.id == self.id)
            .ok_or(SharedError::NotFound)?;
        Ok(self.size.load(Ordering::Acquire))
    }

    fn mapping_size(&self) -> Result<u64, SharedError> {
        self.store
            .snapshot()
            .objects
            .into_iter()
            .find(|snapshot| snapshot.id == self.id)
            .map(|snapshot| snapshot.bytes.len() as u64)
            .ok_or(SharedError::NotFound)
    }

    fn resize(&self, size: u64) -> Result<(), SharedError> {
        let current = self.size()?;
        let seals = self
            .store
            .snapshot()
            .objects
            .into_iter()
            .find(|snapshot| snapshot.id == self.id)
            .map(|snapshot| snapshot.seals)
            .ok_or(SharedError::NotFound)?;
        if size < current && seals.contains(SharedSeal::SHRINK) || size > current && seals.contains(SharedSeal::GROW) {
            return Err(SharedError::Sealed);
        }
        let allocation = size.checked_add(4095).ok_or(SharedError::ResourceLimit)? & !4095;
        let allocation = usize::try_from(allocation).map_err(|_| SharedError::ResourceLimit)?;
        self.store.resize(self.id, allocation)?;
        self.size.store(size, Ordering::Release);
        Ok(())
    }

    fn read_at_offset(&self, offset: u64, output: &mut [u8]) -> Result<usize, ObjectError> {
        let pin = self.store.pin(self.id, false).map_err(Self::object_error)?;
        let offset = usize::try_from(offset).map_err(|_| ObjectError::InvalidArgument)?;
        let logical =
            usize::try_from(self.size().map_err(Self::object_error)?).map_err(|_| ObjectError::ResourceLimit)?;
        let count = output.len().min(logical.saturating_sub(offset));
        pin.read(offset, &mut output[..count]).map_err(Self::object_error)?;
        Ok(count)
    }

    fn write_at_offset(&self, offset: u64, input: &[u8]) -> Result<usize, ObjectError> {
        let offset = usize::try_from(offset).map_err(|_| ObjectError::InvalidArgument)?;
        let end = offset.checked_add(input.len()).ok_or(ObjectError::ResourceLimit)?;
        let allocation = end.checked_add(4095).ok_or(ObjectError::ResourceLimit)? & !4095;
        if allocation > self.mapping_size().map_err(Self::object_error)? as usize {
            self.store.resize(self.id, allocation).map_err(Self::object_error)?;
        }
        let count = self
            .store
            .write_growing(self.id, offset, input)
            .map_err(Self::object_error)?;
        self.size.fetch_max(end as u64, Ordering::AcqRel);
        Ok(count)
    }

    fn object_error(error: SharedError) -> ObjectError {
        match error {
            SharedError::ResourceLimit => ObjectError::ResourceLimit,
            SharedError::Sealed => ObjectError::PermissionDenied,
            _ => ObjectError::Io,
        }
    }

    fn release(&self) {
        if self.released.swap(true, Ordering::AcqRel) {
            return;
        }
        let binding = self
            .binding
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some((registry, identity, epoch)) = binding
            && let Some(registry) = registry.upgrade()
        {
            registry.retire(identity, epoch);
        }
        if self.owns_backing.swap(false, Ordering::AcqRel) {
            let _ = self.store.remove(self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hl_descriptor::{OpenFileDescription, OperationContext, SeekPosition};
    use hl_memory::SharedLimits;
    use std::io::IoSlice;

    fn memfd() -> Arc<RuntimeMemfd> {
        let store = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
        let object = RuntimeMemfd::create(Arc::clone(&store), 1, true).unwrap();
        object.resize(8).unwrap();
        object.write_at_offset(0, b"abcdefgh").unwrap();
        object
    }

    #[test]
    fn reservation_serialization() {
        let object = memfd();
        let prepared = object.prepare_splice_read(None, 4, false, None).unwrap().unwrap();
        assert_eq!(prepared.bytes(), b"abcd");
        assert_eq!(object.write(b"x"), Err(ObjectError::WouldBlock));
        assert_eq!(object.read(&mut [0_u8; 1]), Err(ObjectError::WouldBlock));
        assert_eq!(object.seek(SeekPosition::Start(3)), Err(ObjectError::WouldBlock),);
        prepared.commit(2).unwrap();
        let mut output = [0_u8; 2];
        assert_eq!(object.read(&mut output), Ok(2));
        assert_eq!(&output, b"cd");
    }

    #[test]
    fn positional_independence() {
        let object = memfd();
        let prepared = object.prepare_splice_read(Some(4), 2, false, None).unwrap().unwrap();
        assert_eq!(prepared.bytes(), b"ef");
        let mut output = [0_u8; 1];
        assert_eq!(object.read(&mut output), Ok(1));
        assert_eq!(&output, b"a");
        prepared.commit(2).unwrap();
        assert_eq!(object.seek(SeekPosition::Current(0)), Ok(1));
    }

    #[test]
    fn sealed_vectors_preserve_bytes_and_offsets() {
        let object = memfd();
        object.seek(SeekPosition::Start(2)).unwrap();
        object.add_seals(SharedSeal::WRITE).unwrap();
        let input = [IoSlice::new(b"x"), IoSlice::new(b"yz")];

        assert_eq!(
            object.write_vector_context(
                &input,
                OperationContext {
                    actor: None,
                    cancellation: None
                }
            ),
            Err(ObjectError::PermissionDenied),
        );
        assert_eq!(object.seek(SeekPosition::Current(0)), Ok(2));
        assert_eq!(object.write_vector_at(4, &input), Err(ObjectError::PermissionDenied));
        let mut bytes = [0_u8; 8];
        assert_eq!(object.read_at(0, &mut bytes), Ok(8));
        assert_eq!(&bytes, b"abcdefgh");
    }
}
