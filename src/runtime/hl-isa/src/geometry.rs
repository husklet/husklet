use std::{error::Error, fmt};

/// Linux guest page size presented by both supported guest ABIs.
pub const LINUX_GUEST_PAGE_BYTES: u64 = 4096;

/// Validated architectural word size.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct GuestWordSize(u8);

impl GuestWordSize {
    /// Word size of both supported guests.
    pub const BITS_64: Self = Self(64);

    /// Accepts only the 64-bit word size implemented by the engine.
    pub const fn new(bits: u8) -> Result<Self, GeometryError> {
        if bits == 64 {
            Ok(Self(bits))
        } else {
            Err(GeometryError::UnsupportedWordSize(bits))
        }
    }

    /// Width in bits.
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Width in bytes.
    pub const fn bytes(self) -> u8 {
        self.0 / 8
    }
}

/// One unsigned guest machine word.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GuestWord(u64);

impl GuestWord {
    /// Constructs a 64-bit guest word without host-width conversion.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Raw architectural bits.
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl From<u64> for GuestWord {
    fn from(value: u64) -> Self {
        Self::new(value)
    }
}

impl From<GuestWord> for u64 {
    fn from(value: GuestWord) -> Self {
        value.get()
    }
}

/// Validated Linux guest page size.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct GuestPageSize(u64);

impl GuestPageSize {
    /// Fixed page geometry exposed by the Linux personality.
    pub const LINUX: Self = Self(LINUX_GUEST_PAGE_BYTES);

    /// Accepts only the engine's fixed 4 KiB Linux guest page.
    pub const fn new(bytes: u64) -> Result<Self, GeometryError> {
        if bytes == LINUX_GUEST_PAGE_BYTES {
            Ok(Self(bytes))
        } else {
            Err(GeometryError::UnsupportedPageSize(bytes))
        }
    }

    /// Page size in bytes.
    pub const fn bytes(self) -> u64 {
        self.0
    }

    /// Mask selecting the offset within a page.
    pub const fn offset_mask(self) -> u64 {
        self.0 - 1
    }
}

/// One address in a guest's 64-bit virtual address vocabulary.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GuestAddress(u64);

impl GuestAddress {
    /// Lowest guest address.
    pub const ZERO: Self = Self(0);

    /// Constructs an address from its complete 64-bit representation.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Raw guest virtual address.
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Adds a byte count, rejecting address-space wraparound.
    pub const fn checked_add(self, bytes: u64) -> Result<Self, GeometryError> {
        match self.0.checked_add(bytes) {
            Some(value) => Ok(Self(value)),
            None => Err(GeometryError::AddressOverflow),
        }
    }

    /// Subtracts two addresses when ordered.
    pub const fn checked_offset_from(self, base: Self) -> Result<u64, GeometryError> {
        match self.0.checked_sub(base.0) {
            Some(value) => Ok(value),
            None => Err(GeometryError::AddressUnderflow),
        }
    }

    /// Whether this address begins a guest page.
    pub const fn is_page_aligned(self, page: GuestPageSize) -> bool {
        self.0 & page.offset_mask() == 0
    }

    /// Page containing this address.
    pub const fn page(self, page: GuestPageSize) -> GuestPage {
        GuestPage(self.0 / page.bytes())
    }

    /// Address rounded down to its containing guest page.
    pub const fn page_base(self, page: GuestPageSize) -> Self {
        Self(self.0 & !page.offset_mask())
    }
}

impl From<u64> for GuestAddress {
    fn from(value: u64) -> Self {
        Self::new(value)
    }
}

impl From<GuestAddress> for u64 {
    fn from(value: GuestAddress) -> Self {
        value.get()
    }
}

impl TryFrom<u128> for GuestAddress {
    type Error = GeometryError;

    fn try_from(value: u128) -> Result<Self, Self::Error> {
        u64::try_from(value)
            .map(Self)
            .map_err(|_| GeometryError::AddressOverflow)
    }
}

/// Guest page number, independent of host mapping granularity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GuestPage(u64);

impl GuestPage {
    /// Page number.
    pub const fn number(self) -> u64 {
        self.0
    }

    /// First address in this guest page.
    pub const fn address(self, size: GuestPageSize) -> Result<GuestAddress, GeometryError> {
        match self.0.checked_mul(size.bytes()) {
            Some(value) => Ok(GuestAddress::new(value)),
            None => Err(GeometryError::AddressOverflow),
        }
    }
}

/// Half-open guest address range `[start, end)`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AddressRange {
    start: GuestAddress,
    end: GuestAddress,
}

impl AddressRange {
    /// Constructs a range after proving `start + length` does not wrap.
    pub const fn new(start: GuestAddress, length: u64) -> Result<Self, GeometryError> {
        match start.checked_add(length) {
            Ok(end) => Ok(Self { start, end }),
            Err(error) => Err(error),
        }
    }

    /// Constructs a nonempty range.
    pub const fn nonempty(start: GuestAddress, length: u64) -> Result<Self, GeometryError> {
        if length == 0 {
            Err(GeometryError::EmptyRange)
        } else {
            Self::new(start, length)
        }
    }

    /// Inclusive first address.
    pub const fn start(self) -> GuestAddress {
        self.start
    }

    /// Exclusive end address.
    pub const fn end(self) -> GuestAddress {
        self.end
    }

    /// Length in bytes.
    pub const fn length(self) -> u64 {
        self.end.0 - self.start.0
    }

    /// Whether the range is empty.
    pub const fn is_empty(self) -> bool {
        self.start.0 == self.end.0
    }

    /// Whether both bounds are guest-page aligned.
    pub const fn is_page_aligned(self, page: GuestPageSize) -> bool {
        self.start.is_page_aligned(page) && self.end.is_page_aligned(page)
    }

    /// Whether an address belongs to this half-open range.
    pub const fn contains(self, address: GuestAddress) -> bool {
        address.0 >= self.start.0 && address.0 < self.end.0
    }
}

/// Guest scalar or range geometry is incompatible with the engine contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GeometryError {
    /// Only 64-bit guest words are implemented.
    UnsupportedWordSize(u8),
    /// Linux guest pages are fixed at 4 KiB.
    UnsupportedPageSize(u64),
    /// Address arithmetic exceeded the 64-bit guest vocabulary.
    AddressOverflow,
    /// Address subtraction was requested in reverse order.
    AddressUnderflow,
    /// An operation requiring storage received an empty range.
    EmptyRange,
}

impl fmt::Display for GeometryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedWordSize(bits) => {
                write!(formatter, "unsupported guest word size {bits} bits")
            }
            Self::UnsupportedPageSize(bytes) => {
                write!(formatter, "unsupported Linux guest page size {bytes} bytes")
            }
            Self::AddressOverflow => formatter.write_str("guest address overflow"),
            Self::AddressUnderflow => formatter.write_str("guest address underflow"),
            Self::EmptyRange => formatter.write_str("guest address range is empty"),
        }
    }
}

impl Error for GeometryError {}
