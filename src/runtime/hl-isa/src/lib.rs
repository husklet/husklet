//! Guest architecture identities and validated layout vocabulary.
//!
//! This crate describes the two Linux guest architectures supported by the
//! engine. It deliberately contains no instruction decoding, interpretation,
//! translation, or host execution behavior.

mod architecture;
mod geometry;
mod register;

pub use architecture::{
    ArchitecturePair, Endianness, GuestArchitecture, HostArchitecture, InvalidArchitecture, SUPPORTED_PAIRS,
};
pub use geometry::{AddressRange, GeometryError, GuestAddress, GuestPage, GuestPageSize, GuestWord, GuestWordSize};
pub use register::{CoreRegister, RegisterLayout};

#[cfg(test)]
#[path = "test.rs"]
mod tests;
