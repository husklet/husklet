//! Guest read, write, and scatter paths over the virtual memory arena.

use super::virtual_memory::{ArenaState, HOST_READ, Memory, MemoryError};
use hl_memory::{AtomicU32Write, Protection};
use std::sync::Arc;

impl Memory {
    pub(super) fn read(&self, offset: u64, output: &mut [u8]) -> Result<(), MemoryError> {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.poisoned {
            return Err(MemoryError::Poisoned);
        }
        state.mappings.access(offset, output.len() as u64, Protection::READ)?;
        let (address, _) = self.host_range(offset, output.len() as u64)?;
        // SAFETY: the ledger proves the complete checked arena range is readable;
        // the transaction lock excludes protection changes, output is uniquely
        // borrowed, no pointer escapes, and copy_nonoverlapping cannot unwind.
        unsafe {
            std::ptr::copy_nonoverlapping(address.cast::<u8>(), output.as_mut_ptr(), output.len());
        }
        Ok(())
    }

    pub(super) fn snapshot_read(
        &self,
        offset: u64,
        output: &mut [u8],
        protection: Protection,
    ) -> Result<(), MemoryError> {
        if protection == Protection::NONE {
            return Err(MemoryError::InvalidRange);
        }
        self.snapshot_copy(offset, output, protection, false)
    }

    pub(super) fn frozen_snapshot_read(
        &self,
        _authority: &hl_memory::FrozenSnapshotAuthority,
        offset: u64,
        output: &mut [u8],
        protection: Protection,
    ) -> Result<(), MemoryError> {
        self.snapshot_copy(offset, output, protection, true)
    }

    fn snapshot_copy(
        &self,
        offset: u64,
        output: &mut [u8],
        protection: Protection,
        frozen: bool,
    ) -> Result<(), MemoryError> {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.mappings.access(offset, output.len() as u64, protection)?;
        let (address, _) = self.host_range(offset, output.len() as u64)?;
        let native = Self::native_protection(protection)?;
        let temporary_read = native & HOST_READ == 0;
        if temporary_read && !frozen {
            return Err(MemoryError::InvalidRange);
        }
        if temporary_read {
            self.host
                .protect(address as usize, output.len(), native | HOST_READ)
                .map_err(|()| MemoryError::Host)?;
        }
        unsafe {
            // SAFETY: the ledger proves this is a mapped range, the state mutex
            // excludes mapping transitions, and either its exact host
            // projection is readable or this frozen copy temporarily opened
            // only the requested span. No pointer escapes.
            std::ptr::copy_nonoverlapping(address.cast::<u8>(), output.as_mut_ptr(), output.len());
        }
        if temporary_read && self.host.protect(address as usize, output.len(), native).is_err() {
            return Err(MemoryError::Poisoned);
        }
        Ok(())
    }

    pub(super) fn write(&self, offset: u64, input: &[u8]) -> Result<(), MemoryError> {
        self.write_scatter(&[(offset, input)])
    }

    pub(super) fn write_untracked(&self, offset: u64, input: &[u8]) -> Result<(), MemoryError> {
        self.write_scatter_with(&[(offset, input)], false)
    }

    pub(super) fn probe_write(&self, offset: u64, length: u64) -> Result<(), MemoryError> {
        if length == 0 {
            return Ok(());
        }
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.poisoned {
            return Err(MemoryError::Poisoned);
        }
        state.mappings.access(offset, length, Protection::WRITE)?;
        self.host_range(offset, length)?;
        Ok(())
    }

    pub(super) fn writable_prefix(&self, offset: u64, length: u64) -> Result<usize, MemoryError> {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.poisoned {
            return Err(MemoryError::Poisoned);
        }
        let available = state.mappings.access_prefix(offset, length, Protection::WRITE)?;
        usize::try_from(available).map_err(|_| MemoryError::InvalidRange)
    }

    pub(super) fn write_scatter(&self, writes: &[(u64, &[u8])]) -> Result<(), MemoryError> {
        self.write_scatter_with(writes, true)
    }

    pub(super) fn clear(&self, range: hl_isa::AddressRange) -> Result<(), MemoryError> {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.poisoned {
            return Err(MemoryError::Poisoned);
        }
        state
            .mappings
            .access(range.start().get(), range.length(), Protection::WRITE)?;
        let (address, length) = self.host_range(range.start().get(), range.length())?;
        // The caller performs this exact-range clear before the host discard
        // operation so the zeroing cannot repopulate discarded pages.
        // SAFETY: the ledger proves the exact range writable, the held arena lock
        // excludes mapping/protection changes, and write_bytes retains no pointer.
        unsafe { std::ptr::write_bytes(address.cast::<u8>(), 0, length) };
        self.invalidate(&state, range.start().get(), range.length())
    }

    fn write_scatter_with(&self, writes: &[(u64, &[u8])], invalidate: bool) -> Result<(), MemoryError> {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.poisoned {
            return Err(MemoryError::Poisoned);
        }
        for (offset, input) in writes.iter().filter(|(_, input)| !input.is_empty()) {
            state.mappings.access(*offset, input.len() as u64, Protection::WRITE)?;
            self.host_range(*offset, input.len() as u64)?;
        }
        for (offset, input) in writes.iter().filter(|(_, input)| !input.is_empty()) {
            let (address, _) = self.host_range(*offset, input.len() as u64)?;
            unsafe {
                // SAFETY: every destination was validated before any mutation;
                // the transaction lock excludes mapping and adapter changes,
                // each input remains live, and no pointer escapes.
                std::ptr::copy_nonoverlapping(input.as_ptr(), address.cast::<u8>(), input.len());
            }
        }
        if invalidate {
            for (offset, input) in writes.iter().filter(|(_, input)| !input.is_empty()) {
                self.invalidate(&state, *offset, input.len() as u64)?;
            }
        }
        Ok(())
    }

    fn invalidate(&self, state: &ArenaState, offset: u64, length: u64) -> Result<(), MemoryError> {
        let mut address = offset;
        let end = offset.checked_add(length).ok_or(MemoryError::InvalidRange)?;
        while address < end {
            let (coordinate, mapping_end) = state.mappings.reservation(address)?;
            let epochs = if coordinate.shared() {
                self.shared
                    .as_ref()
                    .map_or_else(|| Arc::clone(&self.reservations), |store| store.reservation_epochs())
            } else {
                Arc::clone(&self.reservations)
            };
            epochs.invalidate_at(coordinate);
            address = (address | 63).saturating_add(1).min(mapping_end).min(end);
        }
        Ok(())
    }
    pub(super) fn compare_write(&self, writes: &[AtomicU32Write]) -> Result<(), MemoryError> {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.poisoned {
            return Err(MemoryError::Poisoned);
        }
        for write in writes {
            state.mappings.access(write.address.get(), 4, Protection::WRITE)?;
            let (address, _) = self.host_range(write.address.get(), 4)?;
            // SAFETY: the range is checked writable and four-byte aligned by the
            // coordinator; the arena mutex excludes adapter access, the pointer
            // remains within the live reservation, and read_unaligned cannot unwind.
            let current = unsafe { std::ptr::read_unaligned(address.cast::<u32>()) };
            if u32::from_le(current) != write.expected {
                return Err(MemoryError::Host);
            }
        }
        for write in writes {
            let (address, _) = self.host_range(write.address.get(), 4)?;
            // SAFETY: every range was prevalidated before any mutation, the arena
            // mutex is still held, the pointer is live and writable, and
            // write_unaligned cannot unwind.
            unsafe {
                std::ptr::write_unaligned(address.cast::<u32>(), write.replacement.to_le());
            }
        }
        Ok(())
    }
}
