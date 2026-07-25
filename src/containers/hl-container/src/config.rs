use std::path::{Path, PathBuf};

/// Persistence selected for container metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Persistence {
    /// Durable, versioned records below the configured state directory.
    File,
    /// Ephemeral state useful for embedded and test processes.
    Memory,
}

/// Configuration for a headless container service.
#[derive(Clone, Debug)]
pub struct Config {
    pub(crate) root: PathBuf,
    pub(crate) persistence: Persistence,
}

impl Config {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            persistence: Persistence::File,
        }
    }

    #[must_use]
    pub fn persistence(mut self, value: Persistence) -> Self {
        self.persistence = value;
        self
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
}
