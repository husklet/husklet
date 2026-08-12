//! Owned activation inputs that select an engine binary.
//!
//! ISA and host streams are activation concerns. They intentionally do not
//! appear in the architecture-neutral launch-config wire.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum GuestIsa {
    Aarch64 = 1,
    X86_64 = 2,
}
