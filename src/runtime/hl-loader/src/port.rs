use std::{error::Error, fmt};

/// Which executable object is requested from an image source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageRole {
    Main,
    Interpreter,
}

/// Host-neutral source failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageSourceError {
    NotFound,
    AccessDenied,
    TooLarge,
    Io,
}

impl fmt::Display for ImageSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "image source failure: {self:?}")
    }
}

impl Error for ImageSourceError {}

/// Bounded source of complete executable image bytes.
pub trait ImageSource {
    /// Reads one regular image object, rejecting it before allocation when its
    /// known size exceeds `max_bytes`.
    fn read_image(&mut self, role: ImageRole, path: &[u8], max_bytes: usize) -> Result<Vec<u8>, ImageSourceError>;
}

/// Purpose of one private mapping reservation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MappingKind {
    MainImage,
    Interpreter,
    Stack,
}

/// Address-selection contract for a reservation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MappingPlacement {
    /// Reserve this exact storage span without replacing an existing mapping.
    /// An occupied span returns [`AddressSpaceError::Conflict`].
    Fixed(u64),
    /// The adapter may choose an address, preferring this deterministic hint.
    Hint(Option<u64>),
}

/// Opaque reservation metadata returned by the address-space adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReservedMapping<R> {
    token: R,
    address: u64,
    size: u64,
}

impl<R> ReservedMapping<R> {
    /// Constructs metadata after the adapter has reserved a private span.
    #[must_use]
    pub const fn new(token: R, address: u64, size: u64) -> Self {
        Self { token, address, size }
    }

    #[must_use]
    pub const fn token(&self) -> &R {
        &self.token
    }

    #[must_use]
    pub const fn address(&self) -> u64 {
        self.address
    }

    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }
}

/// Permissions staged for a region before publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Protection(u8);

impl Protection {
    pub const READ: u8 = 4;
    pub const WRITE: u8 = 2;
    pub const EXECUTE: u8 = 1;

    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits & 7)
    }

    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddressSpaceError {
    Unavailable,
    Conflict,
    OutOfMemory,
    InvalidRange,
    CommitFailed,
}

impl fmt::Display for AddressSpaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "address-space transaction failure: {self:?}")
    }
}

impl Error for AddressSpaceError {}

/// Transactional staging port owned by the loader.
///
/// Reservations and all staged mutations remain private until `commit` makes
/// the complete slice visible atomically. A failed commit publishes nothing.
pub trait TransactionalAddressSpace {
    type Reservation: Clone;

    fn reserve(
        &mut self,
        kind: MappingKind,
        size: u64,
        placement: MappingPlacement,
    ) -> Result<ReservedMapping<Self::Reservation>, AddressSpaceError>;

    fn stage_write(
        &mut self,
        reservation: &Self::Reservation,
        offset: u64,
        bytes: &[u8],
    ) -> Result<(), AddressSpaceError>;

    fn stage_zero(&mut self, reservation: &Self::Reservation, offset: u64, size: u64) -> Result<(), AddressSpaceError>;

    fn stage_protection(
        &mut self,
        reservation: &Self::Reservation,
        offset: u64,
        size: u64,
        protection: Protection,
    ) -> Result<(), AddressSpaceError>;

    fn commit(&mut self, reservations: &[Self::Reservation]) -> Result<(), AddressSpaceError>;

    /// Discards one unpublished reservation and every mutation staged in it.
    fn rollback(&mut self, reservation: &Self::Reservation);
}

/// Registry operations that share the address-space transaction's publication
/// boundary. The adapter must publish or discard these records with the same
/// `commit`/`rollback` decision as its mapping mutations.
pub trait ImageProtectionRegistry<R> {
    /// Stages storage used by translated executable-image lookup.
    fn stage_executable(&mut self, reservation: &R, mapping_offset: u64, size: u64) -> Result<(), AddressSpaceError>;

    /// Stages the guest-coordinate read-only classification used by fault
    /// routing and `/proc`/uaccess projections. `read_only == false` clears a
    /// prior classification for the range.
    fn stage_guest_access(
        &mut self,
        reservation: &R,
        guest_address: u64,
        size: u64,
        read_only: bool,
    ) -> Result<(), AddressSpaceError>;
}
