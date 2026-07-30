use super::Configuration;
use crate::config::WorkspaceConfig;
use std::io;
use std::path::{Path, PathBuf};

pub(super) struct Protocol {
    pub(super) path: PathBuf,
}

impl Protocol {
    pub(super) fn new(directory: &Path) -> Self {
        Self {
            path: directory.join("protocol"),
        }
    }

    pub(super) fn publish(&self) -> io::Result<()> {
        hl_fs::File::from(self.path.clone()).replace(super::PROTOCOL)
    }

    pub(super) fn compatible(&self) -> io::Result<bool> {
        match std::fs::read_to_string(&self.path) {
            Ok(value) => Ok(value.trim() == super::PROTOCOL),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub(super) fn remove(&self) -> io::Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

pub(super) struct ConfigurationIdentity {
    pub(super) path: PathBuf,
}

impl ConfigurationIdentity {
    pub(super) fn new(directory: &Path) -> Self {
        Self {
            path: directory.join("configuration.sha256"),
        }
    }

    pub(super) fn publish(&self, workspace: &WorkspaceConfig) -> io::Result<()> {
        hl_fs::File::from(self.path.clone()).replace(Configuration::new(workspace).signature()?)
    }

    pub(super) fn validate(&self, workspace: &WorkspaceConfig) -> io::Result<()> {
        let effective = std::fs::read_to_string(&self.path).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("live workspace domain has no verifiable configuration identity: {error}"),
            )
        })?;
        let requested = Configuration::new(workspace).signature()?;
        if effective.trim() == requested {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "workspace settings changed while its execution domain is running; stop the workspace runtime before reopening",
            ))
        }
    }

    pub(super) fn remove(&self) -> io::Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}
