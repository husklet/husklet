use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Requested configuration for a locally managed volume.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VolumeSpec {
    pub name: String,
    pub labels: BTreeMap<String, String>,
    pub options: BTreeMap<String, String>,
    pub source: VolumeSource,
    pub(crate) kind: VolumeKind,
}

impl VolumeSpec {
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            labels: BTreeMap::new(),
            options: BTreeMap::new(),
            source: VolumeSource::Managed,
            kind: VolumeKind::Named,
        }
    }

    #[must_use]
    pub fn label(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.labels.insert(name.into(), value.into());
        self
    }

    #[must_use]
    pub fn option(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.options.insert(name.into(), value.into());
        self
    }

    /// Use an explicitly granted host directory as this volume's backing store.
    #[must_use]
    pub fn bind(mut self, device: impl Into<PathBuf>, read_only: bool) -> Self {
        self.source = VolumeSource::Bind {
            device: device.into(),
            read_only,
        };
        self
    }

    pub(crate) fn validate(&self) -> crate::Result<()> {
        let valid = !self.name.is_empty()
            && self.name.len() <= 255
            && self.name != "."
            && self.name != ".."
            && self.name.bytes().enumerate().all(|(index, byte)| match byte {
                b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' => true,
                b'_' | b'.' | b'-' => index != 0,
                _ => false,
            });
        if !valid {
            return Err(crate::Error::InvalidVolume(format!(
                "name {:?} must start with an ASCII letter or digit and contain only letters, digits, '.', '_' or '-'",
                self.name
            )));
        }
        if self.options.keys().any(String::is_empty) {
            return Err(crate::Error::InvalidVolume("option names must not be empty".into()));
        }
        if self.labels.keys().any(String::is_empty) {
            return Err(crate::Error::InvalidVolume("label names must not be empty".into()));
        }
        Ok(())
    }
}

/// Durable metadata and managed data location for a local volume.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Volume {
    pub name: String,
    pub path: PathBuf,
    pub created_at_ms: u64,
    pub labels: BTreeMap<String, String>,
    pub options: BTreeMap<String, String>,
    pub source: VolumeSource,
    pub kind: VolumeKind,
}

/// Lifecycle ownership of a managed volume.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeKind {
    Named,
    Anonymous,
}

/// Ownership and access policy for a volume's host-side data.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VolumeSource {
    /// Data owned below the daemon's volume root.
    #[default]
    Managed,
    /// Caller-granted host directory; the daemon never deletes or populates it.
    Bind { device: PathBuf, read_only: bool },
}

impl Volume {
    pub(crate) fn from_spec(spec: VolumeSpec, path: PathBuf, created_at_ms: u64) -> Self {
        Self {
            name: spec.name,
            path,
            created_at_ms,
            labels: spec.labels,
            options: spec.options,
            source: spec.source,
            kind: spec.kind,
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}
