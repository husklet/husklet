use std::fs::File;
use std::os::fd::AsRawFd;
use std::sync::atomic::Ordering;

use super::abi;
use super::virtual_host::MapSource;
use super::virtual_memory::{Memory, MemoryError};

#[derive(Debug)]
pub(super) struct Canonical {
    pub(super) file: File,
    pub(super) offset: u64,
}

impl Memory {
    pub(super) fn prepare_canonical(&self, request: hl_memory::MapRequest) -> Result<Option<Canonical>, MemoryError> {
        let (file, offset) = match request.backing {
            hl_memory::Backing::File { identity, shared: true } => {
                let files = self.files.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                let file = files
                    .get(&(identity.device, identity.object))
                    .ok_or(MemoryError::Host)?
                    .try_clone()
                    .map_err(|_| MemoryError::Host)?;
                (file, request.backing_offset)
            }
            hl_memory::Backing::Shared(reference) => {
                let Some(registry) = self.shared_backings.as_ref() else {
                    return Ok(None);
                };
                let Ok(file) = registry.file(reference.object) else {
                    return Ok(None);
                };
                let offset = reference
                    .offset
                    .checked_add(request.backing_offset)
                    .ok_or(MemoryError::InvalidRange)?;
                (file, offset)
            }
            _ => return Ok(None),
        };
        Ok(Some(Canonical { file, offset }))
    }

    pub(super) fn map_host(&self, offset: u64, request: hl_memory::MapRequest) -> Result<(), MemoryError> {
        if matches!(request.backing, hl_memory::Backing::File { .. }) {
            return self.map_file(offset, request);
        }
        if let hl_memory::Backing::Anonymous { identity, shared: true } = request.backing {
            return self.map_shared_anonymous(offset, request, identity);
        }
        if let hl_memory::Backing::Shared(reference) = request.backing {
            if self.shared_backings.is_some() {
                return self.map_shared_backing(offset, request, reference);
            }
            if !self.snapshot_backings {
                return Err(MemoryError::Host);
            }
        }
        let (address, length) = self.host_range(offset, request.length)?;
        self.host
            .map(address as usize, length, 1 | 2, false, MapSource::Anonymous)
            .map_err(|()| MemoryError::Host)?;
        if let hl_memory::Backing::Shared(reference) = request.backing {
            let store = self.shared.as_ref().ok_or(MemoryError::Host)?;
            let pin = if self.inherited_shared {
                store.pin_inherited(reference, false)
            } else {
                store.pin_backing(reference, false)
            }
            .map_err(|_| MemoryError::Host)?;
            // SAFETY: the range is temporarily writable, checked, and uniquely
            // borrowed while the transaction mutex is held.
            let output = unsafe { std::slice::from_raw_parts_mut(address.cast::<u8>(), length) };
            pin.read(
                usize::try_from(
                    reference
                        .offset
                        .checked_add(request.backing_offset)
                        .ok_or(MemoryError::InvalidRange)?,
                )
                .map_err(|_| MemoryError::Host)?,
                output,
            )
            .map_err(|_| MemoryError::Host)?;
        }
        let native = Self::native_protection(request.protection)?;
        if self.host.protect(address as usize, length, native).is_err() {
            if self.host.protect(address as usize, length, abi::PROT_NONE).is_err() {
                return Err(MemoryError::Poisoned);
            }
            return Err(MemoryError::Host);
        }
        Ok(())
    }

    fn map_shared_backing(
        &self,
        offset: u64,
        request: hl_memory::MapRequest,
        reference: hl_memory::SharedBackingRef,
    ) -> Result<(), MemoryError> {
        let (address, length) = self.host_range(offset, request.length)?;
        let backings = self.shared_backings.as_ref().ok_or(MemoryError::Host)?;
        let file = backings.file(reference.object).map_err(|_| MemoryError::Host)?;
        let native = Self::native_protection(request.protection)?;
        let backing_offset = i64::try_from(
            reference
                .offset
                .checked_add(request.backing_offset)
                .ok_or(MemoryError::InvalidRange)?,
        )
        .map_err(|_| MemoryError::InvalidRange)?;
        self.host
            .map(
                address as usize,
                length,
                native,
                true,
                MapSource::File {
                    descriptor: file.as_raw_fd(),
                    offset: backing_offset,
                },
            )
            .map_err(|()| MemoryError::Host)
    }

    pub(super) fn map_shared_anonymous(
        &self,
        offset: u64,
        request: hl_memory::MapRequest,
        identity: u64,
    ) -> Result<(), MemoryError> {
        let (address, length) = self.host_range(offset, request.length)?;
        let native = Self::native_protection(request.protection)?;
        let key = (u64::MAX, identity);
        let mut files = self.files.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let std::collections::btree_map::Entry::Vacant(e) = files.entry(key) {
            let file = abi::Memfd::create(c"hl-shared").map_err(|()| MemoryError::Host)?;
            file.set_len(
                request
                    .backing_offset
                    .checked_add(request.length)
                    .ok_or(MemoryError::InvalidRange)?,
            )
            .map_err(|_| MemoryError::Host)?;
            e.insert(file);
        }
        let file = files.get(&key).ok_or(MemoryError::Host)?;
        let backing_offset = i64::try_from(request.backing_offset).map_err(|_| MemoryError::InvalidRange)?;
        self.host
            .map(
                address as usize,
                length,
                native,
                true,
                MapSource::File {
                    descriptor: file.as_raw_fd(),
                    offset: backing_offset,
                },
            )
            .map_err(|()| MemoryError::Host)
    }

    pub(super) fn apply_backing(&self, change: hl_memory::BackingChange) -> Result<(), MemoryError> {
        let files = self.files.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let file = files
            .get(&(change.identity.device, change.identity.object))
            .ok_or(MemoryError::Host)?;
        let size = file.metadata().map_err(|_| MemoryError::Host)?.len();
        (size == change.new_size).then_some(()).ok_or(MemoryError::Host)
    }

    pub(super) fn map_file(&self, offset: u64, request: hl_memory::MapRequest) -> Result<(), MemoryError> {
        let hl_memory::Backing::File { identity, shared } = request.backing else {
            return Err(MemoryError::InvalidRange);
        };
        let (address, length) = self.host_range(offset, request.length)?;
        let native = Self::native_protection(request.protection)?;
        let files = self.files.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let file = files
            .get(&(identity.device, identity.object))
            .ok_or(MemoryError::Host)?;
        let offset = i64::try_from(request.backing_offset).map_err(|_| MemoryError::InvalidRange)?;
        self.host
            .map(
                address as usize,
                length,
                native,
                shared,
                MapSource::File {
                    descriptor: file.as_raw_fd(),
                    offset,
                },
            )
            .map_err(|()| MemoryError::Host)
    }

    pub(super) fn register_file(&self, identity: hl_memory::FileIdentity, file: &File) -> Result<(), MemoryError> {
        let mut files = self.files.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if files.len() >= 1024 && !files.contains_key(&(identity.device, identity.object)) {
            return Err(MemoryError::OutOfMemory);
        }
        files.insert(
            (identity.device, identity.object),
            file.try_clone().map_err(|_| MemoryError::Host)?,
        );
        Ok(())
    }

    pub(super) fn validate_file(
        &self,
        identity: hl_memory::FileIdentity,
        offset: u64,
        length: u64,
        address: u64,
    ) -> Result<(), MemoryError> {
        if self.file_prefix(identity, offset, length, address)? == length {
            return Ok(());
        }
        self.bus_fault.store(address, Ordering::Release);
        Err(MemoryError::InvalidRange)
    }

    pub(super) fn file_prefix(
        &self,
        identity: hl_memory::FileIdentity,
        offset: u64,
        length: u64,
        address: u64,
    ) -> Result<u64, MemoryError> {
        let files = self.files.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let file = files
            .get(&(identity.device, identity.object))
            .ok_or(MemoryError::Host)?;
        let size = file.metadata().map_err(|_| MemoryError::Host)?.len();
        // Linux permits the partial EOF page and zero-fills its tail. Access to
        // a later whole page raises SIGBUS even when the VMA extends farther.
        // SAFETY: sysconf receives no pointer, retains no state, and cannot unwind.
        let page = unsafe { abi::sysconf(abi::_SC_PAGESIZE) };
        let page = u64::try_from(page).map_err(|_| MemoryError::Host)?;
        let valid = size.checked_add(page - 1).ok_or(MemoryError::InvalidRange)? / page * page;
        let available = valid.saturating_sub(offset).min(length);
        if available != 0 || length == 0 {
            return Ok(available);
        }
        self.bus_fault.store(address, Ordering::Release);
        Err(MemoryError::InvalidRange)
    }

    pub(super) fn file_valid_length(&self, identity: hl_memory::FileIdentity) -> Result<u64, MemoryError> {
        let files = self.files.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let file = files
            .get(&(identity.device, identity.object))
            .ok_or(MemoryError::Host)?;
        let length = file.metadata().map_err(|_| MemoryError::Host)?.len();
        let page = 4096_u64;
        length
            .checked_add(page - 1)
            .map(|value| value & !(page - 1))
            .ok_or(MemoryError::InvalidRange)
    }

    pub(super) fn take_bus(&self, address: u64) -> bool {
        self.bus_fault
            .compare_exchange(address, u64::MAX, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}
