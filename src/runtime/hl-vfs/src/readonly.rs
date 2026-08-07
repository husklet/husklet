use std::sync::RwLock;

use crate::{GuestPath, GuestPathBytes, PathError};

/// Maximum number of separately configured read-only path roots.
pub const READ_ONLY_PATH_MAXIMUM: usize = 16;
/// C-compatible capacity includes one trailing NUL byte.
pub const READ_ONLY_PATH_CAPACITY: usize = 256;

/// Failure to extend the bounded read-only path set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadOnlyError {
    InvalidPath(PathError),
    RelativePath,
    PathTooLong,
    Capacity,
}

/// Bounded read-only subtrees in a guest filesystem namespace.
#[derive(Debug, Default)]
pub struct ReadOnlyPaths {
    paths: RwLock<Vec<String>>,
}

impl ReadOnlyPaths {
    /// Creates an empty path set.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            paths: RwLock::new(Vec::new()),
        }
    }

    /// Appends one absolute subtree. Duplicate entries are idempotent.
    ///
    /// Publication happens only after the complete owned string is present,
    /// so concurrent readers observe either the old set or the new set.
    pub fn add(&self, path: &str) -> Result<(), ReadOnlyError> {
        if path.is_empty() {
            return Err(ReadOnlyError::InvalidPath(PathError::Empty));
        }
        if !path.starts_with('/') {
            return Err(ReadOnlyError::RelativePath);
        }
        if path.len() >= READ_ONLY_PATH_CAPACITY {
            return Err(ReadOnlyError::PathTooLong);
        }
        let mut paths = self.paths.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        if paths.iter().any(|stored| stored == path) {
            return Ok(());
        }
        if paths.len() == READ_ONLY_PATH_MAXIMUM {
            return Err(ReadOnlyError::Capacity);
        }
        paths.push(path.to_owned());
        Ok(())
    }

    /// Returns whether a guest path lies in any configured read-only subtree.
    #[must_use]
    pub fn denies(&self, path: &GuestPath) -> bool {
        self.denies_raw(path.as_str().as_bytes())
    }

    /// Returns whether exact guest pathname bytes lie in a configured subtree.
    #[must_use]
    pub fn denies_bytes(&self, path: &GuestPathBytes) -> bool {
        self.denies_raw(path.as_bytes())
    }

    fn denies_raw(&self, path: &[u8]) -> bool {
        self.paths
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|root| {
                path.strip_prefix(root.as_bytes())
                    .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with(b"/"))
            })
    }

    /// Returns the number of distinct configured roots.
    #[must_use]
    pub fn len(&self) -> usize {
        self.paths
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Returns whether no read-only roots are configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
