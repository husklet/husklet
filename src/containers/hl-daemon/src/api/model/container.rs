#[cfg(feature = "runtime")]
use super::format::{ImageName, Ports};
use super::MountPoint;
#[cfg(feature = "runtime")]
use hl_container::{ContainerState as RuntimeState, ExitStatus};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[cfg(feature = "runtime")]
use std::fmt;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
#[serde(default)]
pub struct Container {
    #[serde(flatten)]
    pub metadata: ContainerMetadata,
    pub names: Vec<String>,
    pub command: String,
    pub created: i64,
    pub state: String,
    pub status: String,
    pub ports: Vec<crate::api::PortSummary>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

/// Docker wire metadata shared by container summary and inspection views.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct ContainerMetadata {
    #[serde(rename = "Id")]
    pub id: String,
    pub image: String,
    pub mounts: Vec<MountPoint>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct Wait {
    pub status_code: i64,
}

/// Result of removing all stopped containers.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ContainerPrune {
    pub containers_deleted: Vec<String>,
    pub space_reclaimed: u64,
}

impl Container {
    /// Docker's conventional twelve-character display identity.
    #[must_use]
    pub fn short_id(&self) -> String {
        self.metadata
            .id
            .trim_start_matches("sha256:")
            .chars()
            .take(12)
            .collect()
    }

    /// First Docker name without its wire-format slash, or the short identity.
    #[must_use]
    pub fn name(&self) -> String {
        self.names
            .first()
            .map(|name| name.trim_start_matches('/'))
            .filter(|name| !name.is_empty())
            .map_or_else(|| self.short_id(), str::to_owned)
    }

    /// Human status with Docker's created-state fallback.
    #[must_use]
    pub fn display_status(&self) -> &str {
        if self.status.is_empty() {
            "created"
        } else {
            &self.status
        }
    }

    /// Docker CLI-style comma-separated port summary.
    #[must_use]
    pub fn ports_string(&self) -> String {
        self.ports
            .iter()
            .map(crate::api::PortSummary::display)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[cfg(feature = "runtime")]
impl From<hl_container::Container> for Container {
    fn from(value: hl_container::Container) -> Self {
        let lifecycle = Lifecycle::from_runtime(&value.state, value.created_at_ms);
        let image = ImageName::from(&value.spec).to_string();
        let command = std::iter::once(value.spec.process.program.as_str())
            .chain(value.spec.process.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        Self {
            metadata: ContainerMetadata {
                id: value.id.to_string(),
                image,
                mounts: Vec::new(),
            },
            names: value
                .spec
                .name
                .as_ref()
                .map(|name| vec![format!("/{name}")])
                .unwrap_or_default(),
            command,
            created: i64::try_from(value.created_at_ms / 1_000).unwrap_or(i64::MAX),
            state: lifecycle.state.into(),
            status: lifecycle.status,
            ports: Ports::from(&value.spec).summaries(),
            labels: value.spec.labels,
        }
    }
}

#[cfg(feature = "runtime")]
pub(super) struct Lifecycle {
    state: &'static str,
    pub(super) status: String,
}

#[cfg(feature = "runtime")]
impl Lifecycle {
    fn from_runtime(value: &RuntimeState, created_at_ms: u64) -> Self {
        match value {
            RuntimeState::Created => Self {
                state: "created",
                status: "Created".into(),
            },
            RuntimeState::Running { started_at_ms, .. } => {
                let started_at_ms = if *started_at_ms == 0 {
                    created_at_ms
                } else {
                    *started_at_ms
                };
                Self {
                    state: "running",
                    status: format!("Up {}", Age::since(started_at_ms)),
                }
            }
            RuntimeState::Paused { started_at_ms, .. } => {
                let started_at_ms = if *started_at_ms == 0 {
                    created_at_ms
                } else {
                    *started_at_ms
                };
                Self {
                    state: "paused",
                    status: format!("Up {} (Paused)", Age::since(started_at_ms)),
                }
            }
            RuntimeState::Restarting {
                result,
                finished_at_ms,
                ..
            } => {
                let code = match result {
                    ExitStatus::Code(code) => *code,
                    ExitStatus::Signal(signal) => 128 + signal,
                    ExitStatus::Fault { status, .. } => *status,
                };
                Self {
                    state: "restarting",
                    status: format!("Restarting ({code}) {} ago", Age::since(*finished_at_ms)),
                }
            }
            RuntimeState::Exited {
                result,
                finished_at_ms,
            } => {
                let code = match result {
                    ExitStatus::Code(code) => *code,
                    ExitStatus::Signal(signal) => 128 + signal,
                    ExitStatus::Fault { status, .. } => *status,
                };
                let finished_at_ms = (*finished_at_ms).max(created_at_ms);
                Self {
                    state: "exited",
                    status: format!("Exited ({code}) {} ago", Age::since(finished_at_ms)),
                }
            }
        }
    }
}

#[cfg(feature = "runtime")]
struct Age(u64);

#[cfg(feature = "runtime")]
impl Age {
    fn since(since_ms: u64) -> Self {
        let now_ms = u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        )
        .unwrap_or(u64::MAX);
        Self(now_ms.saturating_sub(since_ms) / 1_000)
    }
}

#[cfg(feature = "runtime")]
impl fmt::Display for Age {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            0..=59 => write!(formatter, "{} seconds", self.0),
            60..=3_599 => write!(formatter, "{} minutes", self.0 / 60),
            3_600..=86_399 => write!(formatter, "{} hours", self.0 / 3_600),
            _ => write!(formatter, "{} days", self.0 / 86_400),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Container;

    fn container() -> Container {
        Container::default()
    }

    fn port(
        private_port: u16,
        public_port: Option<u16>,
        protocol: &str,
    ) -> crate::api::PortSummary {
        crate::api::PortSummary {
            ip: None,
            private_port,
            public_port,
            protocol: protocol.into(),
        }
    }

    #[test]
    fn container_short_id_delegates_to_short() {
        let mut container = container();
        container.metadata.id = "sha256:deadbeefcafe0123456789ab".into();
        assert_eq!(container.short_id(), "deadbeefcafe");
    }

    #[test]
    fn display_status_empty_falls_back_to_created() {
        assert_eq!(container().display_status(), "created");
    }

    #[test]
    fn display_status_passes_through_non_empty() {
        let mut container = container();
        container.status = "Up 3 minutes".into();
        assert_eq!(container.display_status(), "Up 3 minutes");
    }

    #[test]
    fn ports_str_published_port_renders_public_arrow_private() {
        let mut container = container();
        container.ports = vec![port(80, Some(18_080), "tcp")];
        assert_eq!(container.ports_string(), "18080->80/tcp");
    }

    #[test]
    fn ports_str_unpublished_port_renders_private_only() {
        let mut container = container();
        container.ports = vec![port(53, None, "udp")];
        assert_eq!(container.ports_string(), "53/udp");
    }

    #[test]
    fn ports_str_empty_type_defaults_to_tcp() {
        let mut container = container();
        container.ports = vec![port(9000, None, "")];
        assert_eq!(container.ports_string(), "9000/tcp");
    }

    #[test]
    fn ports_str_joins_multiple_with_comma_space() {
        let mut container = container();
        container.ports = vec![port(80, Some(8080), "tcp"), port(443, None, "")];
        assert_eq!(container.ports_string(), "8080->80/tcp, 443/tcp");
    }

    #[test]
    fn ports_str_empty_when_no_ports() {
        assert_eq!(container().ports_string(), "");
    }

    #[test]
    fn container_summary_is_owned_wire_model_without_inspect_exit_code() {
        let container: Container = serde_json::from_value(
            serde_json::json!({"Id":"abcdef012345","Image":"alpine:latest"}),
        )
        .unwrap();
        assert_eq!(container.metadata.id, "abcdef012345");
        assert_eq!(container.metadata.image, "alpine:latest");
        assert!(container.names.is_empty());
        assert!(container.ports.is_empty());
        assert!(serde_json::to_value(container)
            .unwrap()
            .get("ExitCode")
            .is_none());
    }

    #[cfg(feature = "runtime")]
    #[test]
    fn runtime_summary_preserves_command_and_lifecycle_status() {
        let now_ms = u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap();
        let mut process = hl_container::Process::new("/bin/server");
        process.args = vec!["--port".into(), "8080".into()];
        let durable = hl_container::Container {
            id: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                .parse()
                .unwrap(),
            spec: hl_container::ContainerSpec::from_directory("/rootfs", process).name("web"),
            state: hl_container::ContainerState::Running {
                process_id: 7,
                started_at_ms: now_ms.saturating_sub(90_000),
            },
            created_at_ms: now_ms.saturating_sub(3_600_000),
            generation: 1,
            restart: hl_container::Restart::default(),
            health: None,
            checkpoint: None,
        };
        let value = serde_json::to_value(Container::from(durable)).unwrap();
        assert_eq!(value["Command"], "/bin/server --port 8080");
        assert_eq!(value["State"], "running");
        assert_eq!(value["Status"], "Up 1 minutes");

        assert_eq!(
            super::Lifecycle::from_runtime(&hl_container::ContainerState::Created, now_ms).status,
            "Created"
        );
        assert_eq!(
            super::Lifecycle::from_runtime(
                &hl_container::ContainerState::Exited {
                    result: hl_container::ExitStatus::Code(2),
                    finished_at_ms: now_ms.saturating_sub(5_000),
                },
                now_ms.saturating_sub(100_000),
            )
            .status,
            "Exited (2) 5 seconds ago"
        );
        assert!(super::Lifecycle::from_runtime(
            &hl_container::ContainerState::Restarting {
                result: hl_container::ExitStatus::Code(1),
                finished_at_ms: now_ms.saturating_sub(5_000),
                ready_at_ms: now_ms,
            },
            now_ms.saturating_sub(100_000),
        )
        .status
        .starts_with("Restarting (1) "));
        assert!(super::Lifecycle::from_runtime(
            &hl_container::ContainerState::Paused {
                process_id: 7,
                started_at_ms: now_ms.saturating_sub(90_000),
                paused_at_ms: now_ms,
            },
            now_ms.saturating_sub(100_000),
        )
        .status
        .ends_with(" (Paused)"));
    }
}
