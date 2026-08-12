use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Durable ownership of a mount's host-side content.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum MountSource {
    /// Caller-owned host path.
    Bind(PathBuf),
    /// Engine-managed local volume, resolved by name when used.
    Volume(String),
    /// Engine-managed local volume owned by the container mount.
    Anonymous(String),
    /// Container-owned ephemeral storage reported as a Docker tmpfs mount.
    Tmpfs(String),
}

/// Guest access to a host-backed mount.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    ReadOnly,
    ReadWrite,
}

/// Host bind propagation supported by the isolated engine projection.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BindPropagation {
    Private,
    #[default]
    RecursivePrivate,
}

/// One host path exposed at a Linux guest path.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Mount {
    pub source: MountSource,
    pub target: PathBuf,
    pub access: Access,
    pub populate: bool,
    pub subpath: Option<PathBuf>,
    pub propagation: BindPropagation,
    pub recursive: bool,
}

impl Mount {
    #[must_use]
    pub fn read_only(source: impl Into<PathBuf>, target: impl Into<PathBuf>) -> Self {
        Self {
            source: MountSource::Bind(source.into()),
            target: target.into(),
            access: Access::ReadOnly,
            populate: false,
            subpath: None,
            propagation: BindPropagation::RecursivePrivate,
            recursive: true,
        }
    }
    #[must_use]
    pub fn read_write(source: impl Into<PathBuf>, target: impl Into<PathBuf>) -> Self {
        Self {
            source: MountSource::Bind(source.into()),
            target: target.into(),
            access: Access::ReadWrite,
            populate: false,
            subpath: None,
            propagation: BindPropagation::RecursivePrivate,
            recursive: true,
        }
    }

    /// Mount a managed named volume with the selected guest access.
    #[must_use]
    pub fn volume(name: impl Into<String>, target: impl Into<PathBuf>, access: Access) -> Self {
        Self {
            source: MountSource::Volume(name.into()),
            target: target.into(),
            access,
            populate: false,
            subpath: None,
            propagation: BindPropagation::RecursivePrivate,
            recursive: true,
        }
    }

    #[must_use]
    pub fn volume_read_only(name: impl Into<String>, target: impl Into<PathBuf>) -> Self {
        Self::volume(name, target, Access::ReadOnly)
    }

    #[must_use]
    pub fn volume_read_write(name: impl Into<String>, target: impl Into<PathBuf>) -> Self {
        Self::volume(name, target, Access::ReadWrite)
    }

    /// Mount an already-created anonymous volume without exposing its managed path.
    #[must_use]
    pub fn anonymous(volume: &crate::Volume, target: impl Into<PathBuf>, access: Access) -> Self {
        Self {
            source: MountSource::Anonymous(volume.name.clone()),
            target: target.into(),
            access,
            populate: false,
            subpath: None,
            propagation: BindPropagation::RecursivePrivate,
            recursive: true,
        }
    }

    #[must_use]
    pub fn anonymous_read_write(volume: &crate::Volume, target: impl Into<PathBuf>) -> Self {
        Self::anonymous(volume, target, Access::ReadWrite)
    }
    #[must_use]
    pub fn tmpfs(volume: &crate::Volume, target: impl Into<PathBuf>) -> Self {
        Self {
            source: MountSource::Tmpfs(volume.name.clone()),
            target: target.into(),
            access: Access::ReadWrite,
            populate: false,
            subpath: None,
            propagation: BindPropagation::RecursivePrivate,
            recursive: true,
        }
    }

    /// Populate an empty managed volume from its guest path in the image rootfs.
    #[must_use]
    pub fn populate(mut self) -> Self {
        self.populate = true;
        self
    }

    /// Selects an existing directory below a managed volume's root.
    ///
    /// # Errors
    /// Returns an invalid-spec error for empty, absolute, or non-normalized paths.
    pub fn subpath(mut self, value: impl Into<PathBuf>) -> crate::Result<Self> {
        let value = value.into();
        if value.as_os_str().is_empty()
            || value.is_absolute()
            || value
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(crate::Error::InvalidSpec(
                "volume subpath must be a non-empty normalized relative path".into(),
            ));
        }
        self.subpath = Some(value);
        self.populate = false;
        Ok(self)
    }

    #[must_use]
    pub const fn propagation(mut self, value: BindPropagation) -> Self {
        self.propagation = value;
        self
    }
}

/// Runtime-only mount with its managed source resolved to a current host path.
#[derive(Clone, Debug)]
pub(crate) struct ResolvedMount {
    pub(crate) source: PathBuf,
    pub(crate) target: PathBuf,
    pub(crate) access: Access,
}
