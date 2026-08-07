//! Open file description behaviour for runtime memfd objects.

use std::io::IoSlice;
use std::sync::atomic::Ordering;

use hl_descriptor::{
    ObjectError, ObjectKind, OfdMetadata, OfdTimestamp, OpenFileDescription, OperationContext, PreparedSpliceRead,
    SeekPosition,
};
use hl_memory::SharedSeal;

use super::{PreparedMemfdRead, RuntimeMemfd};

impl OpenFileDescription for RuntimeMemfd {
    fn domain_extension(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn kind(&self) -> ObjectKind {
        ObjectKind::File
    }

    fn metadata(&self) -> Result<OfdMetadata, ObjectError> {
        let snapshot = self
            .store
            .snapshot()
            .objects
            .into_iter()
            .find(|snapshot| snapshot.id == self.id)
            .ok_or(ObjectError::Retired)?;
        let size = self.size.load(Ordering::Acquire);
        let timestamp = OfdTimestamp {
            seconds: 0,
            nanoseconds: 0,
        };
        Ok(OfdMetadata {
            device: 0,
            inode: u64::from(self.id.slot) | (u64::from(self.id.generation) << 32),
            kind: 8,
            permissions: 0o600,
            links: 1,
            user: u32::try_from(snapshot.owner).unwrap_or(u32::MAX),
            group: 0,
            special_device: 0,
            size,
            blocks_512: size.saturating_add(511) / 512,
            accessed: timestamp,
            modified: timestamp,
            changed: timestamp,
        })
    }

    fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
        let mut position = self.position.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if position.splice_reserved {
            return Err(ObjectError::WouldBlock);
        }
        let count = self.read_at_offset(position.offset, output)?;
        position.offset += count as u64;
        Ok(count)
    }

    fn write(&self, input: &[u8]) -> Result<usize, ObjectError> {
        let mut position = self.position.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if position.splice_reserved {
            return Err(ObjectError::WouldBlock);
        }
        let count = self.write_at_offset(position.offset, input)?;
        position.offset += count as u64;
        Ok(count)
    }

    fn read_at(&self, offset: u64, output: &mut [u8]) -> Result<usize, ObjectError> {
        self.read_at_offset(offset, output)
    }

    fn write_at(&self, offset: u64, input: &[u8]) -> Result<usize, ObjectError> {
        self.write_at_offset(offset, input)
    }

    fn write_vector_context(
        &self,
        input: &[IoSlice<'_>],
        _context: OperationContext<'_>,
    ) -> Result<usize, ObjectError> {
        let bytes = Self::vector_bytes(input)?;
        self.write(&bytes)
    }

    fn write_vector_at(&self, offset: u64, input: &[IoSlice<'_>]) -> Result<usize, ObjectError> {
        let bytes = Self::vector_bytes(input)?;
        self.write_at_offset(offset, &bytes)
    }

    fn add_seals(&self, seals: u8) -> Result<u8, ObjectError> {
        if !self.allow_sealing {
            return Err(ObjectError::PermissionDenied);
        }
        self.store
            .add_seals(self.id, SharedSeal::from_bits(seals))
            .map(SharedSeal::bits)
            .map_err(Self::object_error)
    }

    fn seals(&self) -> Result<u8, ObjectError> {
        self.store
            .snapshot()
            .objects
            .into_iter()
            .find(|snapshot| snapshot.id == self.id)
            .map(|snapshot| snapshot.seals.bits())
            .ok_or(ObjectError::Retired)
    }

    fn prepare_splice_read(
        &self,
        offset: Option<u64>,
        maximum: usize,
        _nonblocking: bool,
        _cancellation: Option<&dyn hl_descriptor::OperationCancellation>,
    ) -> Result<Option<Box<dyn PreparedSpliceRead>>, ObjectError> {
        let mut position = self.position.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let reserved = offset.is_none();
        if reserved && position.splice_reserved {
            return Err(ObjectError::WouldBlock);
        }
        let start = offset.unwrap_or(position.offset);
        if reserved {
            position.splice_reserved = true;
        }
        drop(position);
        let mut bytes = vec![0; maximum];
        let count = match self.read_at_offset(start, &mut bytes) {
            Ok(count) => count,
            Err(error) => {
                self.release_splice_reservation(reserved);
                return Err(error);
            }
        };
        bytes.truncate(count);
        Ok(Some(Box::new(PreparedMemfdRead {
            position: self.position.clone(),
            start,
            bytes,
            reserved,
        })))
    }

    fn seek(&self, position: SeekPosition) -> Result<u64, ObjectError> {
        let size = self.size().map_err(Self::object_error)?;
        let mut current = self.position.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if current.splice_reserved {
            return Err(ObjectError::WouldBlock);
        }
        let next = match position {
            SeekPosition::Start(value) => Some(value),
            SeekPosition::Current(delta) => current.offset.checked_add_signed(delta),
            SeekPosition::End(delta) => size.checked_add_signed(delta),
            SeekPosition::Data(offset) => (offset < size).then_some(offset),
            SeekPosition::Hole(offset) => (offset < size).then_some(size),
        }
        .ok_or(ObjectError::InvalidArgument)?;
        current.offset = next;
        Ok(next)
    }

    fn close(&self) {
        self.release();
    }
}

impl RuntimeMemfd {
    fn vector_bytes(input: &[IoSlice<'_>]) -> Result<Vec<u8>, ObjectError> {
        let length = input
            .iter()
            .try_fold(0_usize, |total, slice| total.checked_add(slice.len()))
            .ok_or(ObjectError::ResourceLimit)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|_| ObjectError::ResourceLimit)?;
        for slice in input {
            bytes.extend_from_slice(slice);
        }
        Ok(bytes)
    }
}

impl Drop for RuntimeMemfd {
    fn drop(&mut self) {
        self.release();
    }
}

impl RuntimeMemfd {
    fn release_splice_reservation(&self, reserved: bool) {
        if reserved {
            self.position
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .splice_reserved = false;
        }
    }
}

impl PreparedSpliceRead for PreparedMemfdRead {
    fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    fn commit(mut self: Box<Self>, count: usize) -> Result<(), ObjectError> {
        if count > self.bytes.len() {
            return Err(ObjectError::InvalidArgument);
        }
        if self.reserved {
            let mut position = self.position.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if !position.splice_reserved || position.offset != self.start {
                return Err(ObjectError::Interrupted);
            }
            position.offset += count as u64;
            position.splice_reserved = false;
            self.reserved = false;
        }
        Ok(())
    }
}

impl Drop for PreparedMemfdRead {
    fn drop(&mut self) {
        if self.reserved {
            self.position
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .splice_reserved = false;
        }
    }
}
