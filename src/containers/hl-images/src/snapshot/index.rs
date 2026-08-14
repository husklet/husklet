//! The per-chain name index sidecar.
//!
//! An unpacked chain is immutable and named by its chain digest, so enumerating
//! it once at unpack turns every later lookup against it into a hash probe. Only
//! layer-chain publications are indexed: writable uppers and forked container
//! snapshots publish generically and are deliberately left out.

use std::path::{Path, PathBuf};

use hl_fs::LayerIndex;

use super::Id;
use crate::Result;
use crate::storage::Directory;

pub(super) fn path(root: &Path, id: &Id) -> PathBuf {
    root.join("index/committed").join(format!("{}.idx", id.as_str()))
}

/// Enumerate a committed tree and publish its index atomically.
///
/// A missing or unverifiable sidecar is a miss, never a wrong answer, so a
/// failure here is reported to the caller but never corrupts the snapshot.
pub(super) fn publish(directory: &Directory, id: &Id, tree: &Path) -> Result<()> {
    let index = LayerIndex::build(tree).map_err(|source| crate::Error::LayerFilesystem {
        operation: "index snapshot tree",
        path: tree.to_owned(),
        source,
    })?;
    directory.replace(Path::new(&format!("{}.idx", id.as_str())), &index.encode())?;
    Ok(())
}

pub(super) fn discard(directory: &Directory, id: &Id) {
    let _ = directory.remove(Path::new(&format!("{}.idx", id.as_str())));
}
