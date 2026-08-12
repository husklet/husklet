//! Regular-file descriptions, metadata, splice, and filesystem statistics.

mod adapter;
mod cursor;
mod description;
mod metadata;
mod splice;
mod statfs;
mod transfer;
mod vector;

pub use description::{SeekPosition, VfsFileDescription, VfsFileHost, VfsFileToken};
pub use metadata::*;
pub use statfs::*;
pub use transfer::Transfer;

#[cfg(test)]
mod description_test;
