use std::sync::Arc;
use std::sync::atomic::Ordering;

use hl_isa::{AddressRange, GuestAddress};

use super::host::{Coordinator, WriteTransaction};
use super::port::MemoryAccessHost;
use crate::{Backing, MemoryError, Protection};

const RECONCILE_WINDOW: usize = 64 * 1024;

/// One admitted guest range participating in a single external vector call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExternalSpan {
    pub address: GuestAddress,
    pub length: u64,
}

impl<H: MemoryAccessHost> Coordinator<H> {
    /// Runs one external read from every span while mapping transitions are
    /// excluded. The closure performs exactly one terminal operation.
    pub fn read_vectors<E>(
        &self,
        spans: &[ExternalSpan],
        operation: impl FnOnce() -> Result<usize, E>,
    ) -> Result<Result<usize, E>, MemoryError> {
        let _admission = self.activity.admit_memory()?;
        let _transaction = self.transaction.lock().unwrap_or_else(|error| error.into_inner());
        let total = self.validate(spans, Protection::READ)?;
        let result = operation();
        if result.as_ref().is_ok_and(|count| *count as u64 > total) {
            return Err(MemoryError::InvariantViolation);
        }
        Ok(result)
    }

    /// Runs one external write into every span and commits only the prefix
    /// reported by the single terminal operation.
    pub fn write_vectors<E>(
        &self,
        spans: &[ExternalSpan],
        operation: impl FnOnce() -> Result<usize, E>,
    ) -> Result<Result<usize, E>, MemoryError> {
        let _admission = self.activity.admit_memory()?;
        self.request_mapping_change();
        let _transaction = self.transaction.lock().unwrap_or_else(|error| error.into_inner());
        let total = self.validate(spans, Protection::WRITE)?;
        let mut prepared = Vec::with_capacity(spans.len());
        for span in spans.iter().filter(|span| span.length != 0) {
            let range = AddressRange::nonempty(span.address, span.length).map_err(|_| MemoryError::AddressOverflow)?;
            let reservation = self.host.prepare_write(range)?;
            prepared.push(WriteTransaction {
                access: Arc::clone(&self.host),
                reservation,
                generation: self.ledger.generation(),
                observed_epoch: None,
                range,
                committed: false,
            });
        }
        let count = match operation() {
            Ok(count) => count,
            Err(error) => return Ok(Err(error)),
        };
        let count = count as u64;
        if count > total {
            return Err(MemoryError::InvariantViolation);
        }
        let mut remaining = count;
        let mut executable = Vec::new();
        for (span, transaction) in spans.iter().filter(|span| span.length != 0).zip(&mut prepared) {
            let changed = span.length.min(remaining);
            if changed == 0 {
                self.host.commit_external_write(transaction.reservation, 0)?;
                transaction.committed = true;
                continue;
            }
            let resolution = self
                .ledger
                .resolve(span.address, Protection::WRITE)
                .ok_or(MemoryError::NoAddressSpace)?;
            if let Backing::Shared(_) = resolution.region.backing() {
                self.reconcile(resolution.region, resolution.backing_offset, span.address, changed)?;
            }
            if let Err(error) = self.host.commit_external_write(transaction.reservation, changed) {
                self.host.executable.publish(executable);
                return Err(error);
            }
            transaction.committed = true;
            if changed != 0 {
                let range = AddressRange::nonempty(span.address, changed).map_err(|_| MemoryError::AddressOverflow)?;
                self.invalidate_exclusive(range)?;
                executable.extend(self.executable_write_ranges(span.address, resolution, changed));
            }
            remaining -= changed;
        }
        if count != 0 {
            self.host.executable.publish(executable);
            self.host.epoch.fetch_add(1, Ordering::AcqRel);
        }
        Ok(Ok(count as usize))
    }

    /// Runs one host operation while mappings are stable and reconciles the
    /// successfully written prefix without staging the whole transfer.
    pub fn external_write<E>(
        &self,
        address: GuestAddress,
        length: u64,
        operation: impl FnOnce() -> Result<usize, E>,
    ) -> Result<Result<usize, E>, MemoryError> {
        let _admission = self.activity.admit_memory()?;
        self.request_mapping_change();
        let _transaction = self.transaction.lock().unwrap_or_else(|error| error.into_inner());
        let range = AddressRange::nonempty(address, length).map_err(|_| MemoryError::AddressOverflow)?;
        let resolution = self
            .ledger
            .resolve(address, Protection::WRITE)
            .ok_or(MemoryError::NoAddressSpace)?;
        if resolution.contiguous < length {
            return Err(MemoryError::NoAddressSpace);
        }
        if let Backing::File { identity, .. } = resolution.region.backing() {
            self.host
                .host
                .validate_file(identity, resolution.backing_offset, length, address)?;
        }
        let reservation = self.host.prepare_write(range)?;
        let mut prepared = WriteTransaction {
            access: Arc::clone(&self.host),
            reservation,
            generation: self.ledger.generation(),
            observed_epoch: None,
            range,
            committed: false,
        };
        let count = match operation() {
            Ok(count) => count,
            Err(error) => return Ok(Err(error)),
        };
        let count = u64::try_from(count).map_err(|_| MemoryError::InvariantViolation)?;
        if count > length {
            return Err(MemoryError::InvariantViolation);
        }
        if let Backing::Shared(_) = resolution.region.backing() {
            self.reconcile(resolution.region, resolution.backing_offset, address, count)?;
        }
        self.host.commit_external_write(prepared.reservation, count)?;
        prepared.committed = true;
        if count != 0 {
            let changed = AddressRange::nonempty(address, count).map_err(|_| MemoryError::AddressOverflow)?;
            self.invalidate_exclusive(changed)?;
            self.host
                .executable
                .publish(self.executable_write_ranges(address, resolution, count));
            self.host.epoch.fetch_add(1, Ordering::AcqRel);
        }
        Ok(Ok(count as usize))
    }

    /// Runs one host read while mmap/munmap/mprotect transitions are excluded.
    pub fn external_read<E>(
        &self,
        address: GuestAddress,
        length: u64,
        operation: impl FnOnce() -> Result<usize, E>,
    ) -> Result<Result<usize, E>, MemoryError> {
        let _admission = self.activity.admit_memory()?;
        let _transaction = self.transaction.lock().unwrap_or_else(|error| error.into_inner());
        let range = AddressRange::nonempty(address, length).map_err(|_| MemoryError::AddressOverflow)?;
        let resolution = self
            .ledger
            .resolve(address, Protection::READ)
            .ok_or(MemoryError::NoAddressSpace)?;
        if resolution.contiguous < length {
            return Err(MemoryError::NoAddressSpace);
        }
        if let Backing::File { identity, .. } = resolution.region.backing() {
            self.host
                .host
                .validate_file(identity, resolution.backing_offset, length, address)?;
        }
        let result = operation();
        if result.as_ref().is_ok_and(|count| *count > range.length() as usize) {
            return Err(MemoryError::InvariantViolation);
        }
        Ok(result)
    }

    pub(crate) fn reconcile(
        &self,
        region: crate::Region,
        backing_offset: u64,
        address: GuestAddress,
        count: u64,
    ) -> Result<(), MemoryError> {
        let pin = self.retained_pin(region)?;
        let mut reconciled = 0_u64;
        let mut bytes = vec![0; RECONCILE_WINDOW.min(count as usize)];
        while reconciled < count {
            let amount = bytes.len().min((count - reconciled) as usize);
            let start = GuestAddress::new(address.get() + reconciled);
            let chunk = AddressRange::nonempty(start, amount as u64).map_err(|_| MemoryError::AddressOverflow)?;
            self.host.read(chunk, &mut bytes[..amount], Protection::WRITE)?;
            let offset = usize::try_from(backing_offset + reconciled).map_err(|_| MemoryError::BackingOverflow)?;
            pin.write(offset, &bytes[..amount])?;
            reconciled += amount as u64;
        }
        Ok(())
    }

    fn validate(&self, spans: &[ExternalSpan], protection: Protection) -> Result<u64, MemoryError> {
        let mut total = 0_u64;
        for span in spans {
            if span.length == 0 {
                continue;
            }
            let resolution = self
                .ledger
                .resolve(span.address, protection)
                .ok_or(MemoryError::NoAddressSpace)?;
            if resolution.contiguous < span.length {
                return Err(MemoryError::NoAddressSpace);
            }
            if let Backing::File { identity, .. } = resolution.region.backing() {
                self.host
                    .host
                    .validate_file(identity, resolution.backing_offset, span.length, span.address)?;
            }
            total = total.checked_add(span.length).ok_or(MemoryError::AddressOverflow)?;
        }
        Ok(total)
    }
}
