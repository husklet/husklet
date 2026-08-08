use std::sync::Arc;

use hl_isa::{AddressRange, GuestAddress};

use crate::{MapRequest, MemoryError, Protection};

pub trait Host: std::fmt::Debug + Send + Sync {
    fn reservation_epochs(&self) -> Option<Arc<crate::ReservationEpochs>> {
        None
    }
    fn stage_map(&self, address: GuestAddress, request: MapRequest) -> Result<u64, MemoryError>;
    fn stage_unmap(&self, range: AddressRange) -> Result<u64, MemoryError>;
    fn stage_protect(&self, range: AddressRange, protection: Protection) -> Result<u64, MemoryError>;
    fn commit(&self, reservations: &[u64]) -> Result<(), MemoryError>;
    fn rollback(&self, reservation: u64);
    fn stage_remap(
        &self,
        _source: AddressRange,
        _destination: GuestAddress,
        _request: MapRequest,
        _keep_source: bool,
    ) -> Result<u64, MemoryError> {
        Err(MemoryError::InvariantViolation)
    }
}

/// A staged write returned by [`MemoryAccessHost::prepare_write`].
///
/// The reservation carries the validated range by value so a host that can
/// address the range directly needs no side table to recover it at commit.
/// Hosts that still need per-reservation state keep using `token`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WriteReservation {
    pub token: u64,
    pub range: AddressRange,
}

impl WriteReservation {
    #[must_use]
    pub fn new(token: u64, range: AddressRange) -> Self {
        Self { token, range }
    }
}

pub trait MemoryAccessHost: Host {
    type Projection: HostProjection;

    /// Pins stable host storage for an already validated range.
    fn project(&self, _range: AddressRange) -> Result<Self::Projection, MemoryError> {
        Err(MemoryError::InvariantViolation)
    }
    /// Retains one invariant guest-to-storage address transform.
    ///
    /// This is addressability evidence only. It grants no read, write, or
    /// execute authority and does not prove that any address in the aperture
    /// is currently mapped. Hosts return `None` while an exceptional
    /// projection can supersede the transform.
    fn project_aperture(&self) -> Result<Option<super::HostAperture<Self::Projection>>, MemoryError> {
        Ok(None)
    }
    fn validate_file(
        &self,
        _identity: crate::FileIdentity,
        _offset: u64,
        _length: u64,
        _address: GuestAddress,
    ) -> Result<(), MemoryError> {
        Ok(())
    }

    fn file_prefix(
        &self,
        identity: crate::FileIdentity,
        offset: u64,
        length: u64,
        address: GuestAddress,
    ) -> Result<u64, MemoryError> {
        self.validate_file(identity, offset, length, address)?;
        Ok(length)
    }

    /// Copies bytes after the coordinator has admitted `access` for the full
    /// range. The host must preserve that authority when its own mapping model
    /// performs a second protection check; substituting read authority would
    /// reject execute-only instruction fetches and write-only reconciliation.
    fn read(&self, range: AddressRange, output: &mut [u8], access: Protection) -> Result<(), MemoryError>;
    fn prepare_write(&self, range: AddressRange) -> Result<WriteReservation, MemoryError>;
    fn commit_write(&self, reservation: WriteReservation, input: &[u8]) -> Result<(), MemoryError>;
    /// Commits one already validated atomic write without requiring a retained
    /// host reservation. Hosts with directly addressable memory should override
    /// this to avoid allocating a transaction record for every atomic update.
    fn write_atomic(&self, range: AddressRange, input: &[u8]) -> Result<(), MemoryError> {
        let reservation = self.prepare_write(range)?;
        if let Err(error) = self.commit_write(reservation, input) {
            self.rollback_write(reservation);
            return Err(error);
        }
        Ok(())
    }
    /// Compare-exchanges an already validated, naturally aligned one-, two-,
    /// four-, or eight-byte word with a single host atomic instruction. Native
    /// execution runs guest atomics directly on this same storage, so a
    /// coordinator read-modify-write serialized only against other coordinator
    /// callers silently loses those updates. Returns `None` when the host
    /// cannot address the range directly, leaving the caller its serialized
    /// fallback.
    fn compare_exchange_atomic(
        &self,
        _range: AddressRange,
        _expected: u64,
        _replacement: u64,
    ) -> Result<Option<u64>, MemoryError> {
        Ok(None)
    }
    /// Finalizes a write performed directly into the reserved host mapping.
    ///
    /// Implementations must only accept a prefix of the range supplied to
    /// `prepare_write`. The bytes are already present in host storage.
    fn commit_external_write(&self, reservation: WriteReservation, length: u64) -> Result<(), MemoryError> {
        self.rollback_write(reservation);
        let _ = length;
        Err(MemoryError::InvariantViolation)
    }
    fn rollback_write(&self, reservation: WriteReservation);
}

pub trait HostProjection: Send {
    fn storage_address(&self) -> u64;

    /// The projection aliases the coordinator's canonical shared backing, so
    /// writes through it need no arena-to-backing reconciliation.
    fn shared_backing_is_coherent(&self) -> bool {
        false
    }
}

impl HostProjection for u64 {
    fn storage_address(&self) -> u64 {
        *self
    }
}
