use super::Configuration;
use crate::config::WorkspaceConfig;
use std::io;
use std::path::{Path, PathBuf};

/// What the runtime directory says about the protocol its domain speaks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Publication {
    /// The published version is the one this build speaks.
    Compatible,
    /// A version is published and it is not ours.
    Mismatched(String),
    /// No version is published: the owner is mid-startup, mid-teardown, or gone. This is
    /// "cannot tell", not "wrong version", and the two deserve different handling.
    Unpublished,
}

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

    pub(super) fn state(&self) -> io::Result<Publication> {
        match std::fs::read_to_string(&self.path) {
            Ok(value) if value.trim() == super::PROTOCOL => Ok(Publication::Compatible),
            Ok(value) => Ok(Publication::Mismatched(value.trim().to_owned())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Publication::Unpublished),
            Err(error) => Err(error),
        }
    }

    pub(super) fn compatible(&self) -> io::Result<bool> {
        Ok(self.state()? == Publication::Compatible)
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
        hl_fs::File::from(self.path.clone()).replace(Configuration::new(workspace).signature())
    }

    pub(super) fn validate(&self, workspace: &WorkspaceConfig) -> io::Result<()> {
        let effective = std::fs::read_to_string(&self.path).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("live workspace domain has no verifiable configuration identity: {error}"),
            )
        })?;
        let requested = Configuration::new(workspace).signature();
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
