use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fmt::Write as _,
    io::{Cursor, Read, Seek, SeekFrom, Write},
    path::Path,
    sync::Arc,
};

use bytes::Bytes;
use futures_util::StreamExt;
use oci_spec::image::{DescriptorBuilder, MediaType};
use sha2::Digest as _;

use crate::{
    content::{FsStore, Store},
    remote::Source,
    rootfs::{Reference as RootReference, Roots},
    snapshot::{Id, Snapshots},
    Descriptor, DescriptorKind as _, Digest, Error, FsImageStore, Image, ImageStore, LeaseStore,
    Leases, Platform, Reference, Result,
};

/// Composition façade for content, names, leases, snapshots, pull, unpack, and GC.
#[derive(Clone, Debug)]
pub struct Images {
    content: FsStore,
    metadata: FsImageStore,
    leases: Leases,
    snapshots: Snapshots,
    operation_lock: std::path::PathBuf,
}

/// Deduplicated compressed content usage for one immutable image graph.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageUsage {
    pub size: u64,
    pub shared: u64,
}

#[derive(Clone, Debug)]
pub struct UnpackedImage {
    image: Image,
    manifest: Descriptor,
    snapshot: Id,
    platform: Platform,
    runtime: RuntimeConfig,
    _lease: Arc<UnpackedLease>,
}

#[derive(Debug)]
struct UnpackedLease {
    leases: Leases,
    id: String,
}

impl Drop for UnpackedLease {
    fn drop(&mut self) {
        let _ = self.leases.delete(&self.id);
    }
}
impl UnpackedImage {
    #[must_use]
    pub fn image(&self) -> &Image {
        &self.image
    }
    #[must_use]
    pub fn manifest(&self) -> &Descriptor {
        &self.manifest
    }
    #[must_use]
    pub fn snapshot(&self) -> &Id {
        &self.snapshot
    }
    #[must_use]
    pub fn platform(&self) -> &Platform {
        &self.platform
    }
    #[must_use]
    pub fn runtime(&self) -> &RuntimeConfig {
        &self.runtime
    }
}

/// Validated process defaults from the selected OCI image configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeConfig {
    pub entrypoint: Vec<String>,
    pub command: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub working_directory: String,
    pub user: String,
}

/// Immutable metadata read from an image's selected OCI configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Metadata {
    pub platform: Platform,
    pub created: Option<String>,
    pub author: Option<String>,
    pub labels: BTreeMap<String, String>,
    pub history: Vec<History>,
    pub runtime: RuntimeConfig,
    pub onbuild: Vec<String>,
    pub exposed_ports: std::collections::BTreeSet<String>,
    pub volumes: std::collections::BTreeSet<String>,
    pub healthcheck: Option<serde_json::Value>,
    pub stop_signal: Option<String>,
}

impl Metadata {
    fn created_at_ms(&self) -> Option<u64> {
        self.created
            .as_deref()
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .and_then(|value| value.timestamp_millis().try_into().ok())
    }

    fn config_bytes(&self, diff_ids: &[String]) -> Result<Vec<u8>> {
        let environment = self
            .runtime
            .environment
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>();
        Ok(serde_json::to_vec(&serde_json::json!({
            "architecture": self.platform.architecture,
            "os": self.platform.os,
            "variant": self.platform.variant,
            "created": self.created,
            "author": self.author,
            "config": {
                "Entrypoint": self.runtime.entrypoint,
                "Cmd": self.runtime.command,
                "Env": environment,
                "WorkingDir": self.runtime.working_directory,
                "User": self.runtime.user,
                "Labels": self.labels,
                "OnBuild": self.onbuild,
                "ExposedPorts": self.exposed_ports.iter().map(|port| (port, serde_json::json!({}))).collect::<BTreeMap<_, _>>(),
                "Volumes": self.volumes.iter().map(|path| (path, serde_json::json!({}))).collect::<BTreeMap<_, _>>(),
                "Healthcheck": self.healthcheck,
                "StopSignal": self.stop_signal,
            },
            "rootfs": {"type": "layers", "diff_ids": diff_ids},
            "history": self.history,
        }))?)
    }
}

/// One OCI image history entry.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct History {
    pub created: Option<String>,
    pub created_by: Option<String>,
    pub comment: Option<String>,
    #[serde(default)]
    pub empty_layer: bool,
}

/// Explicit container overrides. `None` inherits, except that an explicit entrypoint resets the
/// image command when no non-empty command override accompanies it, matching Docker run/create.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RuntimeOverrides {
    pub entrypoint: Option<Vec<String>>,
    pub command: Option<Vec<String>>,
    pub environment: BTreeMap<String, String>,
    pub working_directory: Option<String>,
    pub user: Option<String>,
}

impl RuntimeConfig {
    /// Merge explicit container values over immutable image defaults.
    ///
    /// # Errors
    /// Returns an error for invalid environment names, NUL bytes, or a relative working directory.
    pub fn merge(&self, overrides: RuntimeOverrides) -> Result<Self> {
        let entrypoint_is_explicit = overrides.entrypoint.is_some();
        let mut merged = Self {
            entrypoint: overrides
                .entrypoint
                .unwrap_or_else(|| self.entrypoint.clone()),
            command: match overrides.command {
                Some(command) if !command.is_empty() => command,
                Some(_) | None if entrypoint_is_explicit => Vec::new(),
                Some(_) | None => self.command.clone(),
            },
            environment: self.environment.clone(),
            working_directory: overrides
                .working_directory
                .unwrap_or_else(|| self.working_directory.clone()),
            user: overrides.user.unwrap_or_else(|| self.user.clone()),
        };
        if merged.entrypoint.is_empty() && merged.command.is_empty() {
            merged.command.clone_from(&self.command);
        }
        merged.environment.extend(overrides.environment);
        merged.validate()?;
        Ok(merged)
    }

    #[must_use]
    pub fn argv(&self) -> Vec<&str> {
        self.entrypoint
            .iter()
            .chain(&self.command)
            .map(String::as_str)
            .collect()
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self
            .entrypoint
            .iter()
            .chain(&self.command)
            .any(|value| value.contains('\0'))
            || self.user.contains('\0')
        {
            return Err(Error::MalformedOci("runtime strings contain NUL".into()));
        }
        if !self.working_directory.starts_with('/') {
            return Err(Error::MalformedOci(
                "working directory must be absolute".into(),
            ));
        }
        if self
            .environment
            .keys()
            .any(|name| name.is_empty() || name.contains('=') || name.contains('\0'))
            || self.environment.values().any(|value| value.contains('\0'))
        {
            return Err(Error::MalformedOci("invalid environment entry".into()));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GcReport {
    pub content_removed: u64,
    pub content_bytes_removed: u64,
    pub content_kept: u64,
    pub snapshots_removed: u64,
    pub snapshots_kept: u64,
}

mod catalog;
mod commit;
mod document;
mod gc;
mod metadata;
mod pull;

use document::{Blob, ConfigDocument, IndexDocument, ManifestDocument};

#[cfg(test)]
mod tests;
