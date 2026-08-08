//! Write publication and reconciliation for projection leases.

use std::sync::atomic::Ordering;

use hl_isa::AddressRange;

use super::{DIRTY_RANGE_MAXIMUM, ProjectionLease, ProjectionView, contains};
use crate::mapping::port::{HostProjection, MemoryAccessHost, WriteReservation};
use crate::{Backing, MemoryError, Protection};

impl<H: MemoryAccessHost> ProjectionLease<'_, H> {
    /// Publishes a conservatively dirty writable projection after native exit.
    /// The full leased range is reconciled and invalidated because native code
    /// does not provide a trustworthy per-store callback.
    pub fn publish_written(mut self) -> Result<(), MemoryError> {
        if self.write_reservation.is_none() && self.additional.iter().all(|entry| entry.write_reservation.is_none()) {
            return Err(MemoryError::InvariantViolation);
        }
        let primary = ProjectionView {
            range: self.range,
            storage_address: self.storage_address,
            protection: self.authority,
            index: 0,
        };
        if self.write_reservation.is_some() {
            self.validate_writable(primary)?;
        }
        for projection in &self.additional {
            if projection.write_reservation.is_some() {
                self.validate_writable(projection.view)?;
            }
        }
        let mut executable = Vec::new();
        if let Some(reservation) = self.write_reservation.take() {
            if let Err(error) = self.publish_view(primary, reservation) {
                self.coordinator.host.host.rollback_write(reservation);
                return Err(error);
            }
            if let Some(resolution) = self.coordinator.ledger.resolve(self.range.start(), Protection::WRITE) {
                executable.extend(self.coordinator.executable_write_ranges(
                    self.range.start(),
                    resolution,
                    self.range.length(),
                ));
            }
        }
        for index in 0..self.additional.len() {
            let Some(reservation) = self.additional[index].write_reservation.take() else {
                continue;
            };
            let view = self.additional[index].view;
            if let Err(error) = self.publish_view(view, reservation) {
                self.coordinator.host.host.rollback_write(reservation);
                self.coordinator.host.executable.publish(executable);
                return Err(error);
            }
            if let Some(resolution) = self.coordinator.ledger.resolve(view.range.start(), Protection::WRITE) {
                executable.extend(self.coordinator.executable_write_ranges(
                    view.range.start(),
                    resolution,
                    view.range.length(),
                ));
            }
        }
        self.coordinator.host.epoch.fetch_add(1, Ordering::AcqRel);
        self.coordinator.host.executable.publish(executable);
        Ok(())
    }

    /// Publishes a complete, bounded journal of ranges changed through this
    /// lease. Reservations retain their original full-view authority, while
    /// shared reconciliation, executable invalidation, and exclusives are
    /// limited to the ranges proven dirty by the caller.
    pub fn publish_written_ranges(mut self, dirty: &[AddressRange]) -> Result<(), MemoryError> {
        if dirty.len() > DIRTY_RANGE_MAXIMUM {
            return Err(MemoryError::ResourceLimit);
        }
        let primary = ProjectionView {
            range: self.range,
            storage_address: self.storage_address,
            protection: self.authority,
            index: 0,
        };
        // Native exits already retain every view in this lease. Do not copy
        // that table merely to classify the bounded dirty journal.
        let mut journals = vec![Vec::new(); self.additional.len() + 1];
        for range in dirty {
            let index = if contains(primary, *range, Protection::WRITE) {
                0
            } else {
                self.additional
                    .iter()
                    .position(|entry| contains(entry.view, *range, Protection::WRITE))
                    .map(|index| index + 1)
                    .ok_or(MemoryError::NoAddressSpace)?
            };
            journals[index].push(*range);
        }
        let mut executable = Vec::new();
        if let Some(reservation) = self.write_reservation.take() {
            if let Err(error) = self.publish_ranges(primary, reservation, &journals[0], &mut executable) {
                self.coordinator.host.host.rollback_write(reservation);
                self.coordinator.host.executable.publish(executable.iter().copied());
                return Err(error);
            }
        } else if !journals[0].is_empty() {
            return Err(MemoryError::InvariantViolation);
        }
        for index in 0..self.additional.len() {
            let view = self.additional[index].view;
            let Some(reservation) = self.additional[index].write_reservation.take() else {
                if !journals[index + 1].is_empty() {
                    return Err(MemoryError::InvariantViolation);
                }
                continue;
            };
            if let Err(error) = self.publish_ranges(view, reservation, &journals[index + 1], &mut executable) {
                self.coordinator.host.host.rollback_write(reservation);
                self.coordinator.host.executable.publish(executable.iter().copied());
                return Err(error);
            }
        }
        if dirty.is_empty() {
            return Ok(());
        }
        self.coordinator.host.epoch.fetch_add(1, Ordering::AcqRel);
        self.coordinator.host.executable.publish(executable);
        Ok(())
    }

    fn publish_ranges(
        &self,
        view: ProjectionView,
        reservation: WriteReservation,
        dirty: &[AddressRange],
        executable: &mut Vec<AddressRange>,
    ) -> Result<(), MemoryError> {
        if dirty.is_empty() {
            self.coordinator.host.host.rollback_write(reservation);
            return Ok(());
        }
        for range in dirty {
            let resolution = self.validate_writable(ProjectionView {
                range: *range,
                storage_address: view.storage_address + (range.start().get() - view.range.start().get()),
                protection: Protection::WRITE,
                index: view.index,
            })?;
            if let Backing::Shared(_) = resolution.region.backing()
                && !self.view_shared_backing_is_coherent(view)
            {
                self.coordinator.reconcile(
                    resolution.region,
                    resolution.backing_offset,
                    range.start(),
                    range.length(),
                )?;
            }
            executable.extend(
                self.coordinator
                    .executable_write_ranges(range.start(), resolution, range.length()),
            );
            self.coordinator.invalidate_exclusive(*range)?;
        }
        self.coordinator
            .host
            .host
            .commit_external_write(reservation, view.range.length())
    }

    fn publish_view(&self, view: ProjectionView, reservation: WriteReservation) -> Result<(), MemoryError> {
        let resolution = self.validate_writable(view)?;
        if let Backing::Shared(_) = resolution.region.backing()
            && !self.view_shared_backing_is_coherent(view)
        {
            self.coordinator.reconcile(
                resolution.region,
                resolution.backing_offset,
                view.range.start(),
                view.range.length(),
            )?;
        }
        self.coordinator
            .host
            .host
            .commit_external_write(reservation, view.range.length())?;
        self.coordinator.invalidate_exclusive(view.range)?;
        Ok(())
    }

    fn view_shared_backing_is_coherent(&self, view: ProjectionView) -> bool {
        if view.range == self.range && view.storage_address == self.storage_address {
            return self.projection.shared_backing_is_coherent();
        }
        self.additional
            .iter()
            .find(|projection| projection.view == view)
            .is_some_and(|projection| projection.projection.shared_backing_is_coherent())
    }

    fn validate_writable(&self, view: ProjectionView) -> Result<crate::Resolution, MemoryError> {
        let resolution = self
            .coordinator
            .ledger
            .resolve(view.range.start(), Protection::WRITE)
            .ok_or(MemoryError::NoAddressSpace)?;
        if resolution.contiguous < view.range.length() {
            return Err(MemoryError::NoAddressSpace);
        }
        Ok(resolution)
    }
}
