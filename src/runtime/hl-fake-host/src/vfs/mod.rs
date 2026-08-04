//! Deterministic inode tree and VFS host adapter.

mod tree;

pub use tree::{InodeIdentity, NodeMetadata, Tree, WatchEvent};

#[cfg(test)]
mod tree_test;
