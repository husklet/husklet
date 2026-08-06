use std::sync::atomic::Ordering;
use std::sync::{Arc, MutexGuard};

use hl_isa::{AddressRange, GuestAddress};

use super::host::Coordinator;
use super::port::HostProjection;
use super::port::MemoryAccessHost;
use crate::{Backing, MemoryError, Protection};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectionGeneration {
    pub incarnation: u64,
    pub mappings: u64,
    pub instructions: u64,
}

/// Stable contiguous guest-to-host projection owned by one mapping lease.
///
/// `storage_address` is deliberately an integer. Converting it to a pointer is
/// an unsafe consumer responsibility and that pointer must not escape this
/// lease. Mapping transitions cannot begin while the lease is alive.
pub struct ProjectionLease<'a, H: MemoryAccessHost> {
    coordinator: &'a Coordinator<H>,
    _admission: crate::checkpoint_activity::ActivityAdmission,
    _transaction: MutexGuard<'a, ()>,
    mapping_requests: u64,
    range: AddressRange,
    storage_address: u64,
    _projection: H::Projection,
    protection: Protection,
    authority: Protection,
    backing: Backing,
    generation: ProjectionGeneration,
    write_reservation: Option<u64>,
    additional: Vec<LiveProjection<H::Projection>>,
}

/// Non-forgeable Rust ownership of one generation-qualified direct projection.
///
/// This type grants no bare execution by itself. It retains checkpoint and
/// mapping admission, host storage, ledger authority, and any write rollback
/// reservation until the consumer retires its matching native token.
pub struct DirectAuthorityLease<'a, H: MemoryAccessHost> {
    projection: ProjectionLease<'a, H>,
    protection: Protection,
}

pub const LIVE_PROJECTION_MAXIMUM: usize = 64;
pub const DIRTY_RANGE_MAXIMUM: usize = 64;

/// Publication granularity proven for a writable projected view.
///
/// Production native views remain [`Self::Exact`] until the versioned native
/// result contract can carry a generation-qualified view identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum WritePublication {
    Exact = 0,
    WholeView = 1,
}

impl WritePublication {
    /// Classifies source evidence without enabling the candidate in production.
    ///
    /// The result is advisory until the native ABI can return the matching
    /// generation-qualified view identity. Callers must use
    /// [`ProjectionLease::write_publication`] for the active contract.
    /// This pure value classifier allocates nothing and retains no token,
    /// backing, reservation, or host state.
    #[must_use]
    pub fn classify_candidate(
        backing: Backing,
        protection: Protection,
        coherent_shared_projection: bool,
        executable_alias: bool,
    ) -> Self {
        if protection.contains(Protection::EXECUTE) || executable_alias {
            return Self::Exact;
        }
        match backing {
            Backing::Anonymous { shared: false, .. } => Self::WholeView,
            Backing::Shared(_) if coherent_shared_projection => Self::WholeView,
            Backing::Anonymous { shared: true, .. } | Backing::Shared(_) | Backing::File { .. } => Self::Exact,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectionView {
    pub range: AddressRange,
    pub storage_address: u64,
    pub protection: Protection,
    /// Stable identity within this lease: primary is zero, additional views
    /// retain their insertion ordinal across native cache promotion.
    pub index: u16,
}

struct LiveProjection<P> {
    view: ProjectionView,
    backing: Backing,
    write_reservation: Option<u64>,
    _projection: P,
}

impl<H: MemoryAccessHost> Coordinator<H> {
    pub fn project_direct(
        &self,
        address: GuestAddress,
        length: u64,
        required: Protection,
        incarnation: u64,
    ) -> Result<DirectAuthorityLease<'_, H>, MemoryError> {
        if required == Protection::NONE {
            return Err(MemoryError::InvariantViolation);
        }
        self.project_contiguous(address, length, required, incarnation)
            .map(|projection| DirectAuthorityLease {
                projection,
                protection: required,
            })
    }

    pub fn project_contiguous(
        &self,
        address: GuestAddress,
        length: u64,
        required: Protection,
        incarnation: u64,
    ) -> Result<ProjectionLease<'_, H>, MemoryError> {
        let admission = self.activity.admit_memory()?;
        let transaction = self.transaction.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mapping_requests = self.mapping_requests.load(Ordering::Acquire);
        let range = AddressRange::nonempty(address, length).map_err(|_| MemoryError::AddressOverflow)?;
        let resolution = self
            .ledger
            .resolve(address, required)
            .ok_or(MemoryError::NoAddressSpace)?;
        if resolution.contiguous < length {
            return Err(MemoryError::NoAddressSpace);
        }
        if let Backing::File { identity, .. } = resolution.region.backing() {
            self.host
                .host
                .validate_file(identity, resolution.backing_offset, length, address)?;
        }
        let projection = self.host.host.project(range)?;
        let storage_address = projection.storage_address();
        let write_reservation = required
            .contains(Protection::WRITE)
            .then(|| self.host.host.prepare_write(range))
            .transpose()?;
        Ok(ProjectionLease {
            coordinator: self,
            _admission: admission,
            _transaction: transaction,
            mapping_requests,
            range,
            storage_address,
            _projection: projection,
            protection: resolution.region.protection(),
            authority: required,
            backing: resolution.region.backing(),
            generation: ProjectionGeneration {
                incarnation,
                mappings: self.ledger.generation(),
                instructions: self.host.executable.generation(),
            },
            write_reservation,
            additional: Vec::new(),
        })
    }
}

impl<'a, H: MemoryAccessHost> DirectAuthorityLease<'a, H> {
    pub const fn range(&self) -> AddressRange {
        self.projection.range
    }

    pub const fn storage_address(&self) -> u64 {
        self.projection.storage_address
    }

    pub const fn protection(&self) -> Protection {
        self.protection
    }

    pub const fn generation(&self) -> ProjectionGeneration {
        self.projection.generation
    }

    pub fn publish_written_ranges(self, dirty: &[AddressRange]) -> Result<(), MemoryError> {
        self.projection.publish_written_ranges(dirty)
    }

    pub fn into_projection(self) -> ProjectionLease<'a, H> {
        self.projection
    }
}

impl<H: MemoryAccessHost> ProjectionLease<'_, H> {
    /// Captures checkpoint-request validity for this admitted projection.
    /// Mapping and scheduler validity remain separate continuation gates.
    pub fn checkpoint_continuation(&self) -> crate::CheckpointContinuation {
        self._admission.continuation()
    }

    /// Captures whether a mapping or externally published write has requested
    /// this projection's transaction authority since it was admitted.
    pub fn request_continuation(&self) -> RequestContinuation {
        RequestContinuation::new(&self.coordinator.mapping_requests, self.mapping_requests)
    }
}

/// Lock-free evidence that no mapping transition is queued behind a projection.
#[must_use = "a mapping continuation must be checked before extending projected execution"]
#[derive(Clone, Debug)]
pub struct RequestContinuation {
    requests: Arc<std::sync::atomic::AtomicU64>,
    observed: u64,
}

impl RequestContinuation {
    pub(crate) fn new(requests: &Arc<std::sync::atomic::AtomicU64>, observed: u64) -> Self {
        Self {
            requests: Arc::clone(requests),
            observed,
        }
    }

    #[must_use]
    pub fn is_current(&self) -> bool {
        self.observed != u64::MAX && self.requests.load(Ordering::Acquire) == self.observed
    }
}

impl<'a, H: MemoryAccessHost> ProjectionLease<'a, H> {
    /// Returns the publication contract currently exposed to native consumers.
    ///
    /// Stage one deliberately keeps every production view exact. The
    /// source-evidence classifier records narrower future eligibility without
    /// changing write reconciliation, executable invalidation, or native ABI.
    /// This constant query performs no allocation or host operation.
    pub const fn write_publication(&self, _address: GuestAddress) -> WritePublication {
        WritePublication::Exact
    }

    pub fn allows(&self, protection: Protection) -> bool {
        protection != Protection::NONE && self.authority.contains(protection)
    }

    pub fn into_direct(self, protection: Protection) -> Result<DirectAuthorityLease<'a, H>, MemoryError> {
        if !self.allows(protection) {
            return Err(MemoryError::InvariantViolation);
        }
        Ok(DirectAuthorityLease {
            projection: self,
            protection,
        })
    }
    pub const fn range(&self) -> AddressRange {
        self.range
    }
    pub const fn storage_address(&self) -> u64 {
        self.storage_address
    }
    pub const fn protection(&self) -> Protection {
        self.protection
    }

    /// Permission authority granted to this projection invocation.
    ///
    /// This can be narrower than the mapped region's complete protection and
    /// is the only permission set a native consumer may publish for the
    /// primary view.
    pub const fn authority(&self) -> Protection {
        self.authority
    }
    pub const fn generation(&self) -> ProjectionGeneration {
        self.generation
    }
    pub fn projection_count(&self) -> usize {
        1 + self.additional.len()
    }

    /// Returns executable-alias evidence while this lease keeps mapping and
    /// checkpoint transitions excluded. The address must belong to one of the
    /// retained projection views.
    pub fn executable_aliases(&self, address: GuestAddress) -> Result<crate::ExecutableAliasEvidence, MemoryError> {
        let backing = if self.range.contains(address) {
            self.backing
        } else {
            self.additional
                .iter()
                .find(|projection| projection.view.range.contains(address))
                .map(|projection| projection.backing)
                .ok_or(MemoryError::NoAddressSpace)?
        };
        let evidence = self.coordinator.ledger.executable_aliases(backing);
        if evidence.generation != self.generation.mappings {
            return Err(MemoryError::InvariantViolation);
        }
        Ok(evidence)
    }

    pub fn project_additional(
        &mut self,
        address: GuestAddress,
        length: u64,
        required: Protection,
    ) -> Result<ProjectionView, MemoryError> {
        let range = AddressRange::nonempty(address, length).map_err(|_| MemoryError::AddressOverflow)?;
        if required == Protection::NONE {
            return Err(MemoryError::InvariantViolation);
        }
        let primary = ProjectionView {
            range: self.range,
            storage_address: self.storage_address,
            protection: self.authority,
            index: 0,
        };
        if contains(primary, range, required) {
            return Ok(primary);
        }
        if let Some(existing) = self
            .additional
            .iter()
            .find(|entry| contains(entry.view, range, required))
        {
            return Ok(existing.view);
        }
        // The primary view consumes one slot. Keep the named public bound true
        // for the complete lease, not merely for its additional-view vector.
        if self.projection_count() >= LIVE_PROJECTION_MAXIMUM {
            return Err(MemoryError::ResourceLimit);
        }
        let resolution = self
            .coordinator
            .ledger
            .resolve(address, required)
            .ok_or(MemoryError::NoAddressSpace)?;
        if resolution.contiguous < length {
            return Err(MemoryError::NoAddressSpace);
        }
        if let Backing::File { identity, .. } = resolution.region.backing() {
            self.coordinator
                .host
                .host
                .validate_file(identity, resolution.backing_offset, length, address)?;
        }
        let projection = self.coordinator.host.host.project(range)?;
        let storage_address = projection.storage_address();
        let write_reservation = required
            .contains(Protection::WRITE)
            .then(|| self.coordinator.host.host.prepare_write(range))
            .transpose()?;
        let view = ProjectionView {
            range,
            storage_address,
            protection: required.union(if resolution.region.protection().contains(Protection::EXECUTE) {
                Protection::EXECUTE
            } else {
                Protection::NONE
            }),
            index: u16::try_from(self.additional.len() + 1).map_err(|_| MemoryError::ResourceLimit)?,
        };
        self.additional.push(LiveProjection {
            view,
            backing: resolution.region.backing(),
            write_reservation,
            _projection: projection,
        });
        Ok(view)
    }

    pub fn project_bounded(
        &mut self,
        address: GuestAddress,
        length: u64,
        required: Protection,
        maximum: u64,
    ) -> Result<ProjectionView, MemoryError> {
        if maximum == 0 || length > maximum {
            return Err(MemoryError::ResourceLimit);
        }
        let access = AddressRange::nonempty(address, length).map_err(|_| MemoryError::AddressOverflow)?;
        let resolution = self
            .coordinator
            .ledger
            .resolve(address, required)
            .ok_or(MemoryError::NoAddressSpace)?;
        if resolution.contiguous < length {
            return Err(MemoryError::NoAddressSpace);
        }
        let region = resolution.region.range();
        let aligned = address.get() / maximum * maximum;
        let first = aligned.max(region.start().get());
        let last = first.saturating_add(maximum).min(region.end().get());
        if last < access.end().get() {
            return self.project_additional(address, length, required);
        }
        self.project_additional(GuestAddress::new(first), last - first, required)
    }

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
            if let Some(reservation) = self.additional[index].write_reservation.take() {
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
            if let Some(reservation) = self.additional[index].write_reservation.take() {
                if let Err(error) = self.publish_ranges(view, reservation, &journals[index + 1], &mut executable) {
                    self.coordinator.host.host.rollback_write(reservation);
                    self.coordinator.host.executable.publish(executable.iter().copied());
                    return Err(error);
                }
            } else if !journals[index + 1].is_empty() {
                return Err(MemoryError::InvariantViolation);
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
        reservation: u64,
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

    fn publish_view(&self, view: ProjectionView, reservation: u64) -> Result<(), MemoryError> {
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
            return self._projection.shared_backing_is_coherent();
        }
        self.additional
            .iter()
            .find(|projection| projection.view == view)
            .is_some_and(|projection| projection._projection.shared_backing_is_coherent())
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

impl<H: MemoryAccessHost> Drop for ProjectionLease<'_, H> {
    fn drop(&mut self) {
        if let Some(reservation) = self.write_reservation.take() {
            self.coordinator.host.host.rollback_write(reservation);
        }
        for projection in &mut self.additional {
            if let Some(reservation) = projection.write_reservation.take() {
                self.coordinator.host.host.rollback_write(reservation);
            }
        }
    }
}

fn contains(view: ProjectionView, requested: AddressRange, required: Protection) -> bool {
    view.range.start() <= requested.start() && view.range.end() >= requested.end() && view.protection.contains(required)
}

#[cfg(test)]
mod publication_test {
    use super::{Backing, Protection, WritePublication};
    use crate::FileIdentity;

    #[test]
    fn private_candidate() {
        assert_eq!(WritePublication::Exact as u16, 0);
        assert_eq!(WritePublication::WholeView as u16, 1);
        assert_eq!(
            WritePublication::classify_candidate(
                Backing::Anonymous {
                    identity: 1,
                    shared: false,
                },
                Protection::READ.union(Protection::WRITE),
                false,
                false,
            ),
            WritePublication::WholeView
        );
    }

    #[test]
    fn executable_candidate() {
        let backing = Backing::Anonymous {
            identity: 2,
            shared: false,
        };
        assert_eq!(
            WritePublication::classify_candidate(
                backing,
                Protection::WRITE.union(Protection::EXECUTE),
                true,
                false,
            ),
            WritePublication::Exact
        );
        assert_eq!(
            WritePublication::classify_candidate(backing, Protection::WRITE, true, true),
            WritePublication::Exact
        );
    }

    #[test]
    fn unproven_candidate() {
        assert_eq!(
            WritePublication::classify_candidate(
                Backing::Anonymous {
                    identity: 3,
                    shared: true,
                },
                Protection::WRITE,
                false,
                false,
            ),
            WritePublication::Exact
        );
        assert_eq!(
            WritePublication::classify_candidate(
                Backing::File {
                    identity: FileIdentity { device: 4, object: 5 },
                    shared: false,
                },
                Protection::WRITE,
                true,
                false,
            ),
            WritePublication::Exact
        );
    }
}
