#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub struct CreateFlags(u32);

impl CreateFlags {
    pub const NONBLOCKING: u32 = 0x800;
    pub const CLOSE_ON_EXEC: u32 = 0x8_0000;
    const ALLOWED: u32 = Self::NONBLOCKING | Self::CLOSE_ON_EXEC;

    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    #[must_use]
    pub const fn closes_on_exec(self) -> bool {
        self.0 & Self::CLOSE_ON_EXEC != 0
    }

    pub(super) const fn valid(self) -> bool {
        self.0 & !Self::ALLOWED == 0
    }

    pub(super) const fn nonblocking(self) -> bool {
        self.0 & Self::NONBLOCKING != 0
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub struct SetFlags(u32);

impl SetFlags {
    pub const ABSOLUTE: u32 = 1;
    pub const CANCEL_ON_SET: u32 = 2;
    const ALLOWED: u32 = Self::ABSOLUTE | Self::CANCEL_ON_SET;

    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    pub(super) const fn valid(self) -> bool {
        self.0 & !Self::ALLOWED == 0
    }

    pub(super) const fn absolute(self) -> bool {
        self.0 & Self::ABSOLUTE != 0
    }

    pub(super) const fn cancel_on_set(self) -> bool {
        self.0 & Self::CANCEL_ON_SET != 0
    }
}
