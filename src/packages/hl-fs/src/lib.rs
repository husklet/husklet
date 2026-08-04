//! Transferable, safe host-filesystem mechanisms.
//!
//! The package contains no guest paths, Linux VFS policy, descriptor tables,
//! mount behavior, or engine configuration.

mod directory;
mod error;
mod file;
mod root;

pub use directory::{Directory, DirectoryEntry, EntryKind};
pub use error::{FsError, Result};
pub use file::{AtomicFile, BoundedFile, Durability, FileIdentity};
pub use root::Root;

#[cfg(test)]
#[path = "test.rs"]
mod tests;
