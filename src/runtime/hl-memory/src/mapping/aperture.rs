use std::sync::MutexGuard;

use hl_isa::{AddressRange, GuestAddress};

use super::ProjectionGeneration;
use super::host::Coordinator;
use super::port::{HostProjection, MemoryAccessHost};
use crate::MemoryError;

/// Host-retained evidence for one invariant guest-to-storage transform.
///
/// The retained projection owns the host lifetime. The aperture deliberately
/// carries no protection: consumers must validate every access against the
/// guest ledger or an independently generation-qualified authority.
pub struct HostAperture<P: HostProjection> {
    range: AddressRange,
    storage_first: u64,
    _projection: P,
}

impl<P: HostProjection> HostAperture<P> {
    pub fn new(range: AddressRange, projection: P) -> Result<Self, MemoryError> {
        let storage_first = projection.storage_address();
        storage_first
            .checked_add(range.length().saturating_sub(1))
            .ok_or(MemoryError::AddressOverflow)?;
        Ok(Self {
            range,
            storage_first,
            _projection: projection,
        })
    }

    #[must_use]
    pub const fn range(&self) -> AddressRange {
        self.range
    }

    fn translate(&self, range: AddressRange) -> Option<u64> {
        if range.start() < self.range.start() || range.end() > self.range.end() {
            return None;
        }
        self.storage_first
            .checked_add(range.start().get() - self.range.start().get())
    }
}

/// Address-transform capability held across a complete mapping transaction.
///
/// Mapping publication cannot begin while this lease exists. Checkpoint
/// admission and the host projection additionally keep its storage lifetime
/// valid. Access permissions, dirty publication, shared reconciliation, and
/// instruction invalidation are intentionally outside this capability.
pub struct ApertureLease<'a, H: MemoryAccessHost> {
    coordinator: &'a Coordinator<H>,
    _admission: crate::checkpoint_activity::ActivityAdmission,
    _transaction: MutexGuard<'a, ()>,
    mapping_requests: u64,
    aperture: HostAperture<H::Projection>,
    generation: ProjectionGeneration,
}

impl<H: MemoryAccessHost> Coordinator<H> {
    pub fn project_aperture(&self, incarnation: u64) -> Result<Option<ApertureLease<'_, H>>, MemoryError> {
        let admission = self.activity.admit_memory()?;
        let transaction = self
            .transaction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mapping_requests = self.mapping_requests.load(std::sync::atomic::Ordering::Acquire);
        let Some(aperture) = self.host.host.project_aperture()? else {
            return Ok(None);
        };
        Ok(Some(ApertureLease {
            coordinator: self,
            _admission: admission,
            _transaction: transaction,
            mapping_requests,
            aperture,
            generation: ProjectionGeneration {
                incarnation,
                mappings: self.ledger.generation(),
                instructions: self.host.executable.generation(),
            },
        }))
    }
}

impl<H: MemoryAccessHost> ApertureLease<'_, H> {
    /// Captures whether a mapping mutation has queued behind this aperture.
    pub fn request_continuation(&self) -> crate::RequestContinuation {
        crate::RequestContinuation::new(&self.coordinator.mapping_requests, self.mapping_requests)
    }
    #[must_use]
    pub const fn range(&self) -> AddressRange {
        self.aperture.range()
    }

    #[must_use]
    pub const fn generation(&self) -> ProjectionGeneration {
        self.generation
    }

    /// Translates a bounded range without granting access authority.
    pub fn storage_address(&self, address: GuestAddress, length: u64) -> Option<u64> {
        let range = AddressRange::nonempty(address, length).ok()?;
        self.aperture.translate(range)
    }
}
