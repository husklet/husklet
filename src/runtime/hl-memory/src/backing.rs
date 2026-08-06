use std::sync::{Arc, Mutex};

use crate::{SharedError, SharedObjectId};

/// Opaque storage for one shared object.
///
/// Implementations may retain a native shared-file capability, but no native
/// descriptor or host pointer crosses this boundary.
pub trait SharedBacking: std::fmt::Debug + Send + Sync {
    fn len(&self) -> Result<usize, SharedError>;
    fn resize(&self, size: usize) -> Result<(), SharedError>;
    fn read(&self, offset: usize, output: &mut [u8]) -> Result<(), SharedError>;
    fn write(&self, offset: usize, input: &[u8]) -> Result<(), SharedError>;
}

/// Creates backing capabilities after the store has allocated a stable object
/// identity. Implementations may use that identity to populate a separate
/// application mapping registry transactionally.
pub trait SharedBackingFactory: std::fmt::Debug + Send + Sync {
    fn create(&self, id: SharedObjectId, size: usize) -> Result<Arc<dyn SharedBacking>, SharedError>;
}

#[derive(Debug, Default)]
pub(crate) struct MemoryFactory;

#[derive(Debug)]
struct MemoryBacking(Mutex<Vec<u8>>);

impl SharedBackingFactory for MemoryFactory {
    fn create(&self, _id: SharedObjectId, size: usize) -> Result<Arc<dyn SharedBacking>, SharedError> {
        Ok(Arc::new(MemoryBacking(Mutex::new(vec![0; size]))))
    }
}

impl SharedBacking for MemoryBacking {
    fn len(&self) -> Result<usize, SharedError> {
        Ok(self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len())
    }

    fn resize(&self, size: usize) -> Result<(), SharedError> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).resize(size, 0);
        Ok(())
    }

    fn read(&self, offset: usize, output: &mut [u8]) -> Result<(), SharedError> {
        let bytes = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let end = offset.checked_add(output.len()).ok_or(SharedError::Range)?;
        output.copy_from_slice(bytes.get(offset..end).ok_or(SharedError::Range)?);
        Ok(())
    }

    fn write(&self, offset: usize, input: &[u8]) -> Result<(), SharedError> {
        let mut bytes = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let end = offset.checked_add(input.len()).ok_or(SharedError::Range)?;
        bytes
            .get_mut(offset..end)
            .ok_or(SharedError::Range)?
            .copy_from_slice(input);
        Ok(())
    }
}
