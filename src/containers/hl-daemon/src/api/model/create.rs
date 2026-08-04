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
    pub stop_timeout: Option<i64>,
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
    #[serde(default, rename = "Dns")]
    pub dns: Vec<std::net::IpAddr>,
    #[serde(default, rename = "DnsOptions")]
    pub dns_options: Vec<String>,
    #[serde(default, rename = "DnsSearch")]
    pub dns_search: Vec<String>,
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
impl CreateContainer {
    pub(crate) fn stop_timeout_seconds(&self) -> Result<Option<u64>, String> {
        const MAXIMUM_SECONDS: u64 = 86_400;
        let Some(value) = self.stop_timeout else {
            return Ok(None);
        };
        let value = u64::try_from(value).map_err(|_| "StopTimeout must be nonnegative".to_owned())?;
        if value > MAXIMUM_SECONDS {
            return Err(format!("StopTimeout must not exceed {MAXIMUM_SECONDS} seconds"));
        }
        Ok(Some(value))
    }
}

#[cfg(all(test, feature = "runtime"))]
mod stop_timeout_tests {
    use super::{CreateContainer, HostConfig};

    #[test]
    fn resolver_fields_use_docker_wire_names_and_typed_addresses() {
        let host: HostConfig = serde_json::from_value(serde_json::json!({
            "Dns": ["192.0.2.53", "2001:db8::53"],
            "DnsSearch": ["service.test"],
            "DnsOptions": ["ndots:2"]
        }))
        .unwrap();
        assert_eq!(host.dns[0], "192.0.2.53".parse::<std::net::IpAddr>().unwrap());
        assert_eq!(host.dns[1], "2001:db8::53".parse::<std::net::IpAddr>().unwrap());
        assert_eq!(host.dns_search, ["service.test"]);
        assert_eq!(host.dns_options, ["ndots:2"]);

        let wire = serde_json::to_value(host).unwrap();
        assert_eq!(wire["Dns"], serde_json::json!(["192.0.2.53", "2001:db8::53"]));
        assert_eq!(wire["DnsSearch"], serde_json::json!(["service.test"]));
        assert_eq!(wire["DnsOptions"], serde_json::json!(["ndots:2"]));
        assert!(serde_json::from_value::<HostConfig>(serde_json::json!({"Dns": ["not-an-address"]})).is_err());
    }

    #[test]
    fn stop_timeout_is_optional_nonnegative_and_bounded() {
        assert_eq!(CreateContainer::default().stop_timeout_seconds().unwrap(), None);
        for (seconds, expected) in [(0, 0), (10, 10), (86_400, 86_400)] {
            let request = CreateContainer {
                stop_timeout: Some(seconds),
                ..CreateContainer::default()
            };
            assert_eq!(request.stop_timeout_seconds().unwrap(), Some(expected));
        }
        for seconds in [-1, 86_401] {
            let request = CreateContainer {
                stop_timeout: Some(seconds),
                ..CreateContainer::default()
            };
            assert!(request.stop_timeout_seconds().is_err());
        }
    }
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
