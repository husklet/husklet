//! Flag words shared by a description and held per descriptor number.
/// Mutable flags shared by every descriptor referencing one description.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub struct StatusFlags(u32);
impl StatusFlags {
    pub const ACCESS_MODE_MASK: u32 = 0o00_000_003;
    pub const APPEND: u32 = 0o00_002_000;
    pub const NONBLOCKING: u32 = 0o00_004_000;
    pub const DIRECT: u32 = 0o00_040_000;
    pub const ASYNC: u32 = 0o00_020_000;
    pub const PATH_ONLY: u32 = 0o10_000_000;
    pub const SETTABLE_MASK: u32 = Self::APPEND | Self::NONBLOCKING | Self::DIRECT | Self::ASYNC;
    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }
    #[must_use]
    pub const fn contains(self, bit: u32) -> bool {
        self.0 & bit != 0
    }
    #[must_use]
    pub const fn update_from_fcntl(self, requested: u32) -> Self {
        Self((self.0 & (Self::ACCESS_MODE_MASK | Self::PATH_ONLY)) | (requested & Self::SETTABLE_MASK))
    }
}
/// Flags attached to one descriptor number rather than to its description.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub struct DescriptorFlags(u32);
impl DescriptorFlags {
    pub const CLOSE_ON_EXEC: u32 = 1;
    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }
    #[must_use]
    pub const fn from_fcntl(bits: u32) -> Self {
        Self(bits & Self::CLOSE_ON_EXEC)
    }
    #[must_use]
    pub const fn closes_on_exec(self) -> bool {
        self.0 & Self::CLOSE_ON_EXEC != 0
    }
}
