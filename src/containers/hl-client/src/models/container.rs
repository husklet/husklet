use super::*;
use bollard::models::ContainerSummary;

/// One entry of `GET /containers/json`.
#[derive(Debug, Clone, Default)]
pub struct Container {
    /// Full container id.
    pub id: String,
    /// Image name or id the container was created from.
    pub image: String,
    /// The container's entrypoint command line.
    pub command: String,
    /// The container's names, each with a leading `/`.
    pub names: Vec<String>,
    /// Lifecycle state (e.g. `running`, `exited`, `paused`).
    pub state: String,
    /// Human-readable status string (e.g. `Up 3 minutes`).
    pub status: String,
    /// Creation time as a Unix timestamp in seconds.
    pub created: i64,
    /// Ports the container exposes or publishes.
    pub ports: Vec<Port>,
    /// Bind and volume mounts attached to the container.
    pub mounts: Vec<Mount>,
    /// Exit code of the last run (0 unless populated via inspect).
    pub exit_code: i64,
}

/// A published port from `GET /containers/json`.
#[derive(Debug, Clone, Default)]
pub struct Port {
    /// Port number inside the container.
    pub private_port: u16,
    /// Host port the container port is published on (0 if unpublished).
    pub public_port: u16,
    /// Transport protocol (e.g. `tcp`, `udp`).
    pub typ: String,
}

/// A bind/volume mount of a container (from `Mounts`).
#[derive(Debug, Clone, Default)]
pub struct Mount {
    /// Host path or named volume the mount originates from.
    pub source: String,
    /// Mount path inside the container.
    pub destination: String,
    /// Mount type (e.g. `bind`, `volume`).
    pub typ: String,
}

impl From<ContainerSummary> for Container {
    fn from(c: ContainerSummary) -> Self {
        Container {
            id: c.id.unwrap_or_default(),
            image: c.image.unwrap_or_default(),
            command: c.command.unwrap_or_default(),
            names: c.names.unwrap_or_default(),
            state: c.state.map(|s| s.to_string()).unwrap_or_default(),
            status: c.status.unwrap_or_default(),
            created: c.created.unwrap_or_default(),
            ports: c
                .ports
                .unwrap_or_default()
                .into_iter()
                .map(|p| Port {
                    private_port: p.private_port,
                    public_port: p.public_port.unwrap_or_default(),
                    typ: p.typ.map(|t| t.to_string()).unwrap_or_default(),
                })
                .collect(),
            mounts: c
                .mounts
                .unwrap_or_default()
                .into_iter()
                .map(|m| Mount {
                    source: m.source.unwrap_or_default(),
                    destination: m.destination.unwrap_or_default(),
                    typ: m.typ.map(|t| t.to_string()).unwrap_or_default(),
                })
                .collect(),
            // bollard's ContainerSummary carries no ExitCode; surfaced via inspect if needed.
            exit_code: 0,
        }
    }
}

impl Container {
    /// Short 12-char id like the docker CLI shows.
    pub fn short_id(&self) -> String {
        self.id
            .trim_start_matches("sha256:")
            .chars()
            .take(12)
            .collect()
    }
    /// Display name (first `Names` entry without the leading slash), falling back to short id.
    pub fn name(&self) -> String {
        self.names
            .first()
            .map(|n| n.trim_start_matches('/').to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| self.short_id())
    }
    /// True when the daemon considers this container running.
    pub fn running(&self) -> bool {
        self.state.eq_ignore_ascii_case("running")
    }
    /// True when the container is paused (SIGSTOP'd).
    pub fn paused(&self) -> bool {
        self.state.eq_ignore_ascii_case("paused")
    }
    /// A short status word for display (falls back to "created").
    pub fn display_status(&self) -> String {
        if self.status.is_empty() {
            "created".into()
        } else {
            self.status.clone()
        }
    }
    /// Human "80->18080/tcp, …" string.
    pub fn ports_str(&self) -> String {
        self.ports
            .iter()
            .map(|p| {
                let t = if p.typ.is_empty() { "tcp" } else { &p.typ };
                if p.public_port != 0 {
                    format!("{}->{}/{}", p.public_port, p.private_port, t)
                } else {
                    format!("{}/{}", p.private_port, t)
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Body for `create_container`, built by the CLI/GUI "run" flow. Only `image` is set today.
#[derive(Debug, Clone, Default)]
pub struct CreateContainer {
    /// Image to create the container from.
    pub image: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── short_id() ───────────────────────────────────────────────────────────
    #[test]
    fn container_short_id_delegates_to_short() {
        let c = Container {
            id: "sha256:deadbeefcafe0123456789ab".into(),
            ..Default::default()
        };
        assert_eq!(c.short_id(), "deadbeefcafe");
    }

    // ── display_status() ─────────────────────────────────────────────────────
    #[test]
    fn display_status_empty_falls_back_to_created() {
        let c = Container {
            status: String::new(),
            ..Default::default()
        };
        assert_eq!(c.display_status(), "created");
    }

    #[test]
    fn display_status_passes_through_non_empty() {
        let c = Container {
            status: "Up 3 minutes".into(),
            ..Default::default()
        };
        assert_eq!(c.display_status(), "Up 3 minutes");
    }

    // ── ports_str() ──────────────────────────────────────────────────────────
    #[test]
    fn ports_str_published_port_renders_public_arrow_private() {
        let c = Container {
            ports: vec![Port {
                private_port: 80,
                public_port: 18080,
                typ: "tcp".into(),
            }],
            ..Default::default()
        };
        assert_eq!(c.ports_str(), "18080->80/tcp");
    }

    #[test]
    fn ports_str_unpublished_port_renders_private_only() {
        let c = Container {
            ports: vec![Port {
                private_port: 53,
                public_port: 0,
                typ: "udp".into(),
            }],
            ..Default::default()
        };
        assert_eq!(c.ports_str(), "53/udp");
    }

    #[test]
    fn ports_str_empty_type_defaults_to_tcp() {
        let c = Container {
            ports: vec![Port {
                private_port: 9000,
                public_port: 0,
                typ: String::new(),
            }],
            ..Default::default()
        };
        assert_eq!(c.ports_str(), "9000/tcp");
    }

    #[test]
    fn ports_str_joins_multiple_with_comma_space() {
        let c = Container {
            ports: vec![
                Port {
                    private_port: 80,
                    public_port: 8080,
                    typ: "tcp".into(),
                },
                Port {
                    private_port: 443,
                    public_port: 0,
                    typ: String::new(),
                },
            ],
            ..Default::default()
        };
        assert_eq!(c.ports_str(), "8080->80/tcp, 443/tcp");
    }

    #[test]
    fn ports_str_empty_when_no_ports() {
        let c = Container::default();
        assert_eq!(c.ports_str(), "");
    }

    // ── From<bollard::models::ContainerSummary> (cheap Default-built input) ───
    #[test]
    fn from_container_summary_reshapes_and_defaults_exit_code() {
        let cs = bollard::models::ContainerSummary {
            id: Some("abcdef012345".into()),
            image: Some("alpine:latest".into()),
            ..Default::default()
        };
        let c = Container::from(cs);
        assert_eq!(c.id, "abcdef012345");
        assert_eq!(c.image, "alpine:latest");
        // ContainerSummary carries no ExitCode → hard-coded 0.
        assert_eq!(c.exit_code, 0);
        // absent Option fields become their defaults (empty), not panics.
        assert!(c.names.is_empty());
        assert!(c.ports.is_empty());
        assert_eq!(c.command, "");
    }
}
