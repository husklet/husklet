use super::ContainerMetadata;
#[cfg(feature = "runtime")]
use super::format::{ImageName, PortKey, Ports, Signal};
#[cfg(feature = "runtime")]
use super::lifecycle::State as LifecycleState;
#[cfg(feature = "runtime")]
use super::timestamp::Timestamp;
#[cfg(feature = "runtime")]
use hl_container::{ContainerState as RuntimeState, ExitStatus};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct InspectContainer {
    #[serde(flatten)]
    pub metadata: ContainerMetadata,
    pub name: String,
    pub created: String,
    pub state: ContainerState,
    pub restart_count: i64,
    pub config: ContainerConfig,
    pub host_config: InspectHostConfig,
    pub network_settings: NetworkSettings,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_rw: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_root_fs: Option<i64>,
}

#[cfg(feature = "runtime")]
impl InspectContainer {
    pub(crate) fn size(&mut self, usage: hl_container::FilesystemUsage) {
        self.size_rw = Some(i64::try_from(usage.writable).unwrap_or(i64::MAX));
        self.size_root_fs = Some(i64::try_from(usage.rootfs).unwrap_or(i64::MAX));
    }
}

/// Port declarations shown by Docker container inspection.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ContainerConfig {
    pub exposed_ports: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    pub stop_signal: String,
    pub stop_timeout: i64,
}

/// Docker inspection view of persisted host-side container settings.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct InspectHostConfig {
    pub network_mode: String,
    #[serde(default)]
    pub extra_hosts: Vec<String>,
    pub auto_remove: bool,
    pub restart_policy: crate::api::RestartPolicy,
}

/// Published bindings shown by Docker container inspection.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct NetworkSettings {
    pub ports: BTreeMap<String, Option<Vec<crate::api::PortBinding>>>,
    pub networks: BTreeMap<String, EndpointSettings>,
}

/// Docker inspection view of one network endpoint.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct EndpointSettings {
    #[serde(rename = "NetworkID")]
    pub network_id: String,
    #[serde(rename = "EndpointID")]
    pub endpoint_id: String,
    pub gateway: String,
    #[serde(rename = "IPAddress")]
    pub ip_address: String,
    #[serde(rename = "IPPrefixLen")]
    pub ip_prefix_len: u8,
}

#[cfg(feature = "runtime")]
impl From<(&hl_container::Network, &hl_container::Endpoint)> for EndpointSettings {
    fn from((network, endpoint): (&hl_container::Network, &hl_container::Endpoint)) -> Self {
        Self {
            network_id: network.id.to_string(),
            endpoint_id: String::new(),
            gateway: network.gateway.map_or_else(String::new, |value| value.to_string()),
            ip_address: endpoint.address.map_or_else(String::new, |value| value.to_string()),
            ip_prefix_len: network.subnet.map_or(0, |subnet| subnet.prefix),
        }
    }
}

/// Docker inspection view of a resolved bind or managed-volume mount.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct MountPoint {
    #[serde(rename = "Type")]
    pub kind: String,
    pub name: String,
    pub source: String,
    pub destination: String,
    pub driver: String,
    pub mode: String,
    #[serde(rename = "RW")]
    pub read_write: bool,
    pub propagation: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ContainerState {
    pub status: String,
    #[serde(flatten)]
    pub activity: Activity,
    #[serde(flatten)]
    pub condition: Condition,
    #[serde(rename = "Pid")]
    pub pid: i64,
    pub exit_code: i64,
    pub error: String,
    pub started_at: String,
    pub finished_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<HealthState>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct Activity {
    pub running: bool,
    pub paused: bool,
    pub restarting: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct Condition {
    #[serde(rename = "OOMKilled")]
    pub oom_killed: bool,
    pub dead: bool,
}

/// Docker inspection view of durable container health.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct HealthState {
    pub status: String,
    pub failing_streak: i64,
    pub log: Vec<HealthLog>,
}

/// One bounded health-check result in container inspection.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct HealthLog {
    pub start: String,
    pub end: String,
    pub exit_code: i64,
    pub output: String,
}

#[cfg(feature = "runtime")]
impl From<hl_container::Container> for InspectContainer {
    fn from(value: hl_container::Container) -> Self {
        let lifecycle = Lifecycle::from(&value.state);
        let image = ImageName::from(&value.spec).to_string();
        let health = value.health.as_ref().map(HealthState::from);
        Self {
            metadata: ContainerMetadata {
                id: value.id.to_string(),
                image,
                mounts: Vec::new(),
            },
            name: value
                .spec
                .name
                .as_ref()
                .map(|name| format!("/{name}"))
                .unwrap_or_default(),
            created: Timestamp::from_millis(value.created_at_ms).to_string(),
            state: ContainerState {
                status: lifecycle.status.to_string(),
                activity: lifecycle.activity,
                condition: Condition {
                    oom_killed: false,
                    dead: false,
                },
                pid: i64::try_from(lifecycle.pid).unwrap_or(i64::MAX),
                exit_code: i64::from(lifecycle.exit.code),
                error: lifecycle.exit.error,
                started_at: Timestamp::from_millis(lifecycle.started).to_string(),
                finished_at: Timestamp::from_millis(lifecycle.finished).to_string(),
                health,
            },
            restart_count: i64::from(value.restart.count),
            config: ContainerConfig {
                exposed_ports: value
                    .spec
                    .ports
                    .iter()
                    .map(|port| (PortKey::from(*port).to_string(), serde_json::json!({})))
                    .collect(),
                labels: value.spec.labels.clone(),
                stop_signal: Signal::from(value.spec.stop_signal).to_string(),
                stop_timeout: i64::try_from(value.spec.stop_timeout_seconds).unwrap_or(i64::MAX),
            },
            host_config: InspectHostConfig {
                network_mode: match value.spec.network_mode {
                    hl_container::NetworkMode::Host => "host",
                    hl_container::NetworkMode::Automatic if value.spec.isolation.network_isolated => "none",
                    hl_container::NetworkMode::Automatic => "default",
                }
                .into(),
                extra_hosts: value
                    .spec
                    .hosts
                    .iter()
                    .map(|(name, address)| format!("{name}:{address}"))
                    .collect(),
                auto_remove: value.spec.removal == hl_container::RemovalPolicy::Automatic,
                restart_policy: value.spec.restart.into(),
            },
            network_settings: NetworkSettings {
                ports: Ports::from(&value.spec).bindings(),
                networks: BTreeMap::new(),
            },
            size_rw: None,
            size_root_fs: None,
        }
    }
}

#[cfg(feature = "runtime")]
struct Lifecycle {
    status: LifecycleState,
    activity: Activity,
    pid: u64,
    exit: Exit,
    started: u64,
    finished: u64,
}

#[cfg(feature = "runtime")]
impl From<&RuntimeState> for Lifecycle {
    fn from(state: &RuntimeState) -> Self {
        let status = LifecycleState::from(state);
        let inactive = || Activity {
            running: false,
            paused: false,
            restarting: false,
        };
        match state {
            RuntimeState::Created => Self {
                status,
                activity: inactive(),
                pid: 0,
                exit: Exit::success(),
                started: 0,
                finished: 0,
            },
            RuntimeState::Running {
                process_id,
                started_at_ms,
            } => Self {
                status,
                activity: Activity {
                    running: true,
                    paused: false,
                    restarting: false,
                },
                pid: *process_id,
                exit: Exit::success(),
                started: *started_at_ms,
                finished: 0,
            },
            RuntimeState::Paused {
                process_id,
                started_at_ms,
                ..
            } => Self {
                status,
                activity: Activity {
                    running: true,
                    paused: true,
                    restarting: false,
                },
                pid: *process_id,
                exit: Exit::success(),
                started: *started_at_ms,
                finished: 0,
            },
            RuntimeState::Restarting {
                result, finished_at_ms, ..
            } => Self {
                status,
                activity: Activity {
                    running: true,
                    paused: false,
                    restarting: true,
                },
                pid: 0,
                exit: Exit::from(result),
                started: 0,
                finished: *finished_at_ms,
            },
            RuntimeState::Exited { result, finished_at_ms } => Self {
                status,
                activity: inactive(),
                pid: 0,
                exit: Exit::from(result),
                started: 0,
                finished: *finished_at_ms,
            },
        }
    }
}

#[cfg(feature = "runtime")]
impl From<&hl_container::Health> for HealthState {
    fn from(value: &hl_container::Health) -> Self {
        Self {
            status: match value.status {
                hl_container::HealthStatus::Starting => "starting",
                hl_container::HealthStatus::Healthy => "healthy",
                hl_container::HealthStatus::Unhealthy => "unhealthy",
            }
            .into(),
            failing_streak: i64::from(value.failures),
            log: value.probes.iter().map(HealthLog::from).collect(),
        }
    }
}

#[cfg(feature = "runtime")]
impl From<&hl_container::Probe> for HealthLog {
    fn from(value: &hl_container::Probe) -> Self {
        Self {
            start: Timestamp::from_millis(value.started_at_ms).to_string(),
            end: Timestamp::from_millis(value.finished_at_ms).to_string(),
            exit_code: i64::from(Exit::from(&value.result).code),
            output: value.output.clone(),
        }
    }
}

#[cfg(feature = "runtime")]
struct Exit {
    code: i32,
    error: String,
}

#[cfg(feature = "runtime")]
impl Exit {
    fn success() -> Self {
        Self {
            code: 0,
            error: String::new(),
        }
    }
}

#[cfg(feature = "runtime")]
impl From<&ExitStatus> for Exit {
    fn from(result: &ExitStatus) -> Self {
        match result {
            ExitStatus::Code(code) => Self {
                code: *code,
                error: String::new(),
            },
            ExitStatus::Signal(signal) => Self {
                code: 128 + signal,
                error: format!("terminated by signal {signal}"),
            },
            ExitStatus::Fault { status, detail } => Self {
                code: *status,
                error: format!("engine fault detail={detail}"),
            },
        }
    }
}

#[cfg(all(test, feature = "runtime"))]
mod tests {
    use super::{HealthState, InspectContainer};

    #[test]
    fn health_uses_docker_status_streak_and_bounded_log_shape() {
        let check = hl_container::Healthcheck::new(hl_container::Check::Shell("true".into())).retries(1);
        let mut health = hl_container::Health::starting();
        health.record(
            hl_container::Probe::new(0, 1_000, hl_container::ExitStatus::Code(9), "not ready"),
            &check,
            std::time::Duration::ZERO,
        );
        let state = HealthState::from(&health);
        assert_eq!(state.status, "unhealthy");
        assert_eq!(state.failing_streak, 1);
        assert_eq!(state.log.len(), 1);
        assert_eq!(state.log[0].start, "1970-01-01T00:00:00.000000000Z");
        assert_eq!(state.log[0].end, "1970-01-01T00:00:01.000000000Z");
        assert_eq!(state.log[0].exit_code, 9);
        assert_eq!(state.log[0].output, "not ready");
    }

    #[test]
    fn inspect_includes_docker_runtime_fields() {
        let durable = hl_container::Container {
            id: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                .parse()
                .unwrap(),
            spec: hl_container::ContainerSpec::from_directory("/rootfs", hl_container::Process::new("/bin/server"))
                .name("web")
                .restart(hl_container::RestartPolicy::OnFailure { maximum: Some(3) }),
            state: hl_container::ContainerState::Running {
                process_id: 7,
                started_at_ms: 1_000,
            },
            created_at_ms: 0,
            generation: 1,
            restart: hl_container::Restart::default(),
            health: None,
            checkpoint: None,
        };
        let inspect = serde_json::to_value(InspectContainer::from(durable)).unwrap();
        for key in [
            "Id",
            "Name",
            "Image",
            "Created",
            "State",
            "Config",
            "HostConfig",
            "NetworkSettings",
            "Mounts",
            "RestartCount",
        ] {
            assert!(inspect.get(key).is_some(), "missing {key}: {inspect}");
        }
        assert_eq!(inspect["Config"]["StopTimeout"], 10);
        assert_eq!(inspect["HostConfig"]["AutoRemove"], false);
        assert_eq!(inspect["HostConfig"]["RestartPolicy"]["Name"], "on-failure");
        assert_eq!(inspect["HostConfig"]["RestartPolicy"]["MaximumRetryCount"], 3);
        for key in [
            "Status",
            "Running",
            "Paused",
            "Restarting",
            "OOMKilled",
            "Dead",
            "Pid",
            "ExitCode",
            "StartedAt",
            "FinishedAt",
            "Error",
        ] {
            assert!(inspect["State"].get(key).is_some(), "missing State.{key}: {inspect}");
        }
    }
}
