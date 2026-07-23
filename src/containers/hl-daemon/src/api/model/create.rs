use super::{Attachment, Console, Healthcheck, RestartPolicy};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Docker create-container request.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct CreateContainer {
    pub image: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cmd: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_signal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub healthcheck: Option<Healthcheck>,
    #[serde(default)]
    pub volumes: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub exposed_ports: crate::api::ExposedPorts,
    #[serde(flatten)]
    pub attach: Attachment,
    #[serde(flatten)]
    pub console: Console,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_config: Option<HostConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub networking_config: Option<NetworkingConfig>,
    #[serde(flatten, default)]
    pub unsupported: BTreeMap<String, serde_json::Value>,
}

/// Docker create-time network endpoint selection.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct NetworkingConfig {
    #[serde(default)]
    pub endpoints_config: EndpointsConfig,
}

/// Named endpoint configurations applied atomically after container creation.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(transparent)]
pub struct EndpointsConfig(pub BTreeMap<String, crate::api::EndpointConfig>);

impl EndpointsConfig {
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[must_use]
    pub fn into_inner(self) -> BTreeMap<String, crate::api::EndpointConfig> {
        self.0
    }
}

/// Supported, runtime-effective subset of Docker's `HostConfig`.
///
/// Unknown fields are retained so the daemon can reject meaningful unsupported requests instead
/// of silently pretending to honor them. Docker's zero-valued compatibility fields are accepted.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct HostConfig {
    #[serde(default)]
    pub binds: Vec<String>,
    #[serde(default)]
    pub mounts: Vec<DockerMount>,
    #[serde(default)]
    pub tmpfs: BTreeMap<String, String>,
    #[serde(default)]
    pub extra_hosts: Vec<String>,
    #[serde(default)]
    pub memory: i64,
    #[serde(default)]
    pub pids_limit: Option<i64>,
    #[serde(default)]
    pub nano_cpus: i64,
    #[serde(default)]
    pub readonly_rootfs: bool,
    #[serde(default)]
    pub network_mode: String,
    #[serde(default)]
    pub links: Vec<String>,
    #[serde(default)]
    pub restart_policy: RestartPolicy,
    #[serde(default)]
    pub auto_remove: bool,
    #[serde(default)]
    pub port_bindings: crate::api::PortBindings,
    #[serde(flatten, default)]
    pub unsupported: BTreeMap<String, serde_json::Value>,
}

/// Docker's structured bind or local-volume mount request.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct DockerMount {
    #[serde(rename = "Type")]
    pub kind: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bind_options: Option<BindOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_options: Option<VolumeOptions>,
    #[serde(flatten, default)]
    pub unsupported: BTreeMap<String, serde_json::Value>,
}

/// Supported Docker bind-mount options.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct BindOptions {
    #[serde(default)]
    pub propagation: String,
    #[serde(default)]
    pub non_recursive: bool,
    #[serde(default)]
    pub create_mountpoint: bool,
    #[serde(flatten)]
    pub read_only: BindReadOnly,
}

/// Recursive read-only behavior nested into Docker bind options.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct BindReadOnly {
    #[serde(default)]
    pub read_only_non_recursive: bool,
    #[serde(default)]
    pub read_only_force_recursive: bool,
}

/// Supported Docker local-volume options.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct VolumeOptions {
    #[serde(default)]
    pub no_copy: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subpath: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver_config: Option<DriverConfig>,
}

/// Docker volume driver selection nested inside a mount request.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct DriverConfig {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub options: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ContainerCreation {
    #[serde(rename = "Id")]
    pub id: String,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[cfg(feature = "runtime")]
impl ContainerCreation {
    pub(crate) fn created(id: String, published_ports_discarded: bool) -> Self {
        Self {
            id,
            warnings: published_ports_discarded
                .then(|| "Published ports are discarded when using host network mode".into())
                .into_iter()
                .collect(),
        }
    }
}
