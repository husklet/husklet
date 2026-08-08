//! Guest read and staged-write access over a mapping coordinator.

use super::{AccessState, Coordinator};
use crate::mapping::port::{MemoryAccessHost, WriteReservation};
use crate::{Backing, MemoryError, Protection, Region, SharedBackingPin};
use hl_isa::{AddressRange, GuestAddress};
use std::sync::Arc;
use std::sync::atomic::Ordering;

pub struct WriteTransaction<H: MemoryAccessHost> {
    pub(in crate::mapping) access: Arc<AccessState<H>>,
    pub(in crate::mapping) reservation: WriteReservation,
    pub(crate) generation: u64,
    pub(crate) observed_epoch: Option<u64>,
    pub(in crate::mapping) range: AddressRange,
    pub(in crate::mapping) committed: bool,
}

/// Staged spans of one guest write. The first span is inline because almost
/// every guest store is contiguous, so the common case never touches the heap.
pub struct WriteSpanTransaction<H: MemoryAccessHost> {
    first: WriteTransaction<H>,
    rest: Vec<WriteTransaction<H>>,
    length: usize,
}

impl<H: MemoryAccessHost> WriteSpanTransaction<H> {
    fn spans(&self) -> impl Iterator<Item = &WriteTransaction<H>> {
        std::iter::once(&self.first).chain(self.rest.iter())
    }

    fn spans_mut(&mut self) -> impl Iterator<Item = &mut WriteTransaction<H>> {
        std::iter::once(&mut self.first).chain(self.rest.iter_mut())
    }

    fn count(&self) -> usize {
        self.rest.len() + 1
    }
}

impl<H: MemoryAccessHost> Drop for WriteTransaction<H> {
    fn drop(&mut self) {
        if !self.committed {
            self.access.rollback_write(self.reservation);
        }
    }
}

impl<H: MemoryAccessHost> Coordinator<H> {
    pub(crate) fn retained_pin(&self, region: Region) -> Result<SharedBackingPin, MemoryError> {
        self.pins
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|entry| entry.region == region)
            .map(|entry| entry.pin.retain())
            .ok_or(MemoryError::InvariantViolation)?
            .map_err(MemoryError::from)
    }

    pub fn read(&self, address: GuestAddress, output: &mut [u8], access: Protection) -> Result<(), MemoryError> {
        let length = u64::try_from(output.len()).map_err(|_| MemoryError::AddressOverflow)?;
        let range = AddressRange::nonempty(address, length).map_err(|_| MemoryError::AddressOverflow)?;
        let resolution = self
            .ledger
            .resolve(address, access)
            .ok_or(MemoryError::NoAddressSpace)?;
        if resolution.contiguous < length {
            return Err(MemoryError::NoAddressSpace);
        }
        if matches!(resolution.region.backing(), Backing::Shared(_)) {
            let pin = self.retained_pin(resolution.region)?;
            let offset = usize::try_from(resolution.backing_offset).map_err(|_| MemoryError::BackingOverflow)?;
            return pin.read(offset, output).map_err(MemoryError::from);
        }
        if let Backing::File { identity, .. } = resolution.region.backing() {
            self.host
                .host
                .validate_file(identity, resolution.backing_offset, length, address)?;
        }
        self.host.read(range, output, access)
    }
    pub fn prepare_write(&self, address: GuestAddress, length: u64) -> Result<WriteTransaction<H>, MemoryError> {
        let _admission = self.activity.admit_memory()?;
        self.prepare_write_admitted(address, length)
    }

    /// Stages one write while the caller already holds a memory admission.
    fn prepare_write_admitted(&self, address: GuestAddress, length: u64) -> Result<WriteTransaction<H>, MemoryError> {
        let resolution = self
            .ledger
            .resolve(address, Protection::WRITE)
            .ok_or(MemoryError::NoAddressSpace)?;
        self.prepare_write_resolved(address, length, resolution)
    }

    /// Stages one write from a resolution the caller already proved writable.
    fn prepare_write_resolved(
        &self,
        address: GuestAddress,
        length: u64,
        resolution: crate::Resolution,
    ) -> Result<WriteTransaction<H>, MemoryError> {
        let range = AddressRange::nonempty(address, length).map_err(|_| MemoryError::AddressOverflow)?;
        if resolution.contiguous < length {
            return Err(MemoryError::NoAddressSpace);
        }
        if let Backing::File { identity, .. } = resolution.region.backing() {
            self.host
                .host
                .validate_file(identity, resolution.backing_offset, length, address)?;
        }
        let generation = self.ledger.generation();
        let reservation = self.host.prepare_write(range)?;
        Ok(WriteTransaction {
            access: Arc::clone(&self.host),
            reservation,
            generation,
            observed_epoch: None,
            range,
            committed: false,
        })
    }

    pub fn prepare_write_spans(
        &self,
        address: GuestAddress,
        length: u64,
    ) -> Result<WriteSpanTransaction<H>, MemoryError> {
        let _admission = self.activity.admit_memory()?;
        address.get().checked_add(length).ok_or(MemoryError::AddressOverflow)?;
        if length == 0 {
            return Err(MemoryError::EmptyRange);
        }
        let stage = |offset: u64| -> Result<(WriteTransaction<H>, u64), MemoryError> {
            let current = address.get().checked_add(offset).ok_or(MemoryError::AddressOverflow)?;
            let (resolution, available) =
                self.access_prefix_resolved(GuestAddress::new(current), length - offset, Protection::WRITE)?;
            let (Some(resolution), true) = (resolution, available > 0) else {
                return Err(MemoryError::NoAddressSpace);
            };
            let staged = self.prepare_write_resolved(GuestAddress::new(current), available, resolution)?;
            Ok((staged, available))
        };
        let (first, available) = stage(0)?;
        let mut rest = Vec::new();
        let mut copied = available;
        while copied < length {
            let (staged, available) = stage(copied)?;
            rest.push(staged);
            copied = copied.checked_add(available).ok_or(MemoryError::AddressOverflow)?;
        }
        Ok(WriteSpanTransaction {
            first,
            rest,
            length: usize::try_from(length).map_err(|_| MemoryError::AddressOverflow)?,
        })
    }

    pub fn commit_write_spans(&self, mut prepared: WriteSpanTransaction<H>, input: &[u8]) -> Result<u64, MemoryError> {
        let _admission = self.activity.admit_memory()?;
        self.request_mapping_change();
        let _transaction = self
            .transaction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if input.len() != prepared.length || prepared.spans().any(|span| span.generation != self.ledger.generation()) {
            return Err(MemoryError::InvariantViolation);
        }
        let mut executable = Vec::new();
        // A lone span publishes all or nothing on its own, so its commit pass is
        // already the validation pass and resolving it twice buys nothing.
        if prepared.count() > 1 {
            let mut copied = 0_usize;
            for span in prepared.spans() {
                let length = usize::try_from(span.range.length()).map_err(|_| MemoryError::AddressOverflow)?;
                let end = copied.checked_add(length).ok_or(MemoryError::AddressOverflow)?;
                if end > input.len() {
                    return Err(MemoryError::InvariantViolation);
                }
                let resolution = self
                    .ledger
                    .resolve(span.range.start(), Protection::WRITE)
                    .ok_or(MemoryError::NoAddressSpace)?;
                if resolution.contiguous < span.range.length() {
                    return Err(MemoryError::NoAddressSpace);
                }
                copied = end;
            }
            if copied != input.len() {
                return Err(MemoryError::InvariantViolation);
            }
        } else if u64::try_from(input.len()).unwrap_or(u64::MAX) != prepared.first.range.length() {
            return Err(MemoryError::InvariantViolation);
        }
        let mut copied = 0_usize;
        for span in prepared.spans_mut() {
            let length = usize::try_from(span.range.length()).map_err(|_| MemoryError::AddressOverflow)?;
            let end = copied + length;
            let resolution = self
                .ledger
                .resolve(span.range.start(), Protection::WRITE)
                .ok_or(MemoryError::NoAddressSpace)?;
            if let Backing::Shared(_) = resolution.region.backing() {
                let pin = self.retained_pin(resolution.region)?;
                let offset = usize::try_from(resolution.backing_offset).map_err(|_| MemoryError::BackingOverflow)?;
                pin.write(offset, &input[copied..end])?;
            }
            if let Err(error) = self.host.commit_write(span.reservation, &input[copied..end]) {
                self.host.executable.publish(executable);
                return Err(error);
            }
            span.committed = true;
            executable.extend(self.executable_write_ranges(span.range.start(), resolution, span.range.length()));
            self.invalidate_exclusive(span.range)?;
            copied = end;
        }
        self.host.executable.publish(executable);
        Ok(self.host.epoch.fetch_add(1, Ordering::AcqRel).wrapping_add(1))
    }

    pub fn commit_write(&self, mut prepared: WriteTransaction<H>, input: &[u8]) -> Result<u64, MemoryError> {
        let _admission = self.activity.admit_memory()?;
        self.request_mapping_change();
        let _transaction = self
            .transaction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if input.len() as u64 != prepared.range.length()
            || self.ledger.generation() != prepared.generation
            || prepared
                .observed_epoch
                .is_some_and(|epoch| self.observer_epoch() != epoch)
        {
            return Err(MemoryError::InvariantViolation);
        }
        let resolution = self
            .ledger
            .resolve(prepared.range.start(), Protection::WRITE)
            .ok_or(MemoryError::NoAddressSpace)?;
        if resolution.contiguous < prepared.range.length() {
            return Err(MemoryError::NoAddressSpace);
        }
        let shared = match resolution.region.backing() {
            Backing::Shared(_) => {
                let pin = self.retained_pin(resolution.region)?;
                let offset = usize::try_from(resolution.backing_offset).map_err(|_| MemoryError::BackingOverflow)?;
                let mut previous = vec![0; input.len()];
                pin.read(offset, &mut previous)?;
                pin.write(offset, input)?;
                Some((pin, offset, previous))
            }
            _ => None,
        };
        if let Err(error) = self.host.commit_write(prepared.reservation, input) {
            if let Some((pin, offset, previous)) = shared {
                pin.write(offset, &previous)?;
            }
            return Err(error);
        }
        prepared.committed = true;
        self.invalidate_exclusive(prepared.range)?;
        self.host.executable.publish(self.executable_write_ranges(
            prepared.range.start(),
            resolution,
            prepared.range.length(),
        ));
        let epoch = self.host.epoch.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        Ok(epoch)
    }
}
