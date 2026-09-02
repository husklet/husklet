//! Changing container state on behalf of an extension.

use std::sync::Arc;

use hl_client::model::{CreateContainer, DockerMount, EndpointConfig, EndpointsConfig, ExecConfig, ExecStart,
    ExposedPorts, HostConfig, NetworkingConfig, PortBinding, PortBindings};
use hl_extension::port::{ContainerControl, ContainerCreateSpec, HostError};

use super::{failure, Bridge};

/// How long a stop waits for the initial process before the daemon forces it.
const STOP_SECONDS: u64 = 10;

/// The container control port over the workspace's container daemon.
///
/// Reaching this port is reaching code execution inside the workspace, which is
/// why the protocol gates it behind its own capability.
pub struct ContainerLifecycle {
    bridge: Arc<Bridge>,
}

impl ContainerLifecycle {
    pub(super) fn new(bridge: Arc<Bridge>) -> Self {
        Self { bridge }
    }

    fn creation(spec: &ContainerCreateSpec) -> CreateContainer {
        let exposed = spec.ports.iter().map(|port| {
            (format!("{}/{}", port.container, port.protocol), serde_json::json!({}))
        }).collect();
        let bindings = spec.ports.iter().filter_map(|port| port.host.map(|host| {
            (format!("{}/{}", port.container, port.protocol), Some(vec![PortBinding {
                host_ip: "127.0.0.1".into(), host_port: host.to_string(),
            }]))
        })).collect();
        let network_mode = spec.network.clone().unwrap_or_default();
        let networking_config = spec.network.as_ref().map(|network| NetworkingConfig {
            endpoints_config: EndpointsConfig([(network.clone(), EndpointConfig::default())].into_iter().collect()),
        });
        CreateContainer {
            image: spec.image.clone(), labels: spec.labels.iter().cloned().collect(),
            entrypoint: spec.entrypoint.clone(), cmd: (!spec.command.is_empty()).then(|| spec.command.clone()),
            env: (!spec.environment.is_empty()).then(|| spec.environment.iter()
                .map(|(name, value)| format!("{name}={value}")).collect()),
            working_dir: spec.working_directory.clone(), user: spec.user.clone(),
            exposed_ports: ExposedPorts(exposed),
            host_config: Some(HostConfig {
                mounts: spec.mounts.iter().map(|mount| DockerMount {
                    kind: "volume".into(), source: mount.volume.clone(), target: mount.target.clone(),
                    read_only: mount.read_only, ..DockerMount::default()
                }).collect(),
                memory: spec.memory_mb.map_or(0, |value| i64::from(value) * 1024 * 1024),
                nano_cpus: spec.cpus.map_or(0, |value| i64::from(value) * 1_000_000_000),
                pids_limit: spec.pids_limit.map(i64::from), network_mode,
                port_bindings: PortBindings(bindings), ..HostConfig::default()
            }),
            networking_config, ..CreateContainer::default()
        }
    }
}

impl ContainerControl for ContainerLifecycle {
    /// Creates a container from an image already present locally.
    ///
    /// Pulling stays in the image port so a control grant alone cannot reach
    /// the network.
    ///
    /// # Errors
    /// Returns `HostError::Absent` for an unknown image, `HostError::Conflict`
    /// for a name already taken, and a failure otherwise.
    fn create(&self, image: &str, name: &str) -> Result<String, HostError> {
        self.create_spec(&ContainerCreateSpec {
            image: image.to_owned(), name: name.to_owned(), entrypoint: None, command: Vec::new(),
            environment: Vec::new(), working_directory: None, user: None, labels: Vec::new(),
            mounts: Vec::new(), network: None, ports: Vec::new(), memory_mb: None, cpus: None,
            pids_limit: None,
        })
    }

    fn create_spec(&self, spec: &ContainerCreateSpec) -> Result<String, HostError> {
        let request = Self::creation(spec);
        let client = self.bridge.client();
        let created = self
            .bridge
            .wait(client.containers().create(&request, Some(&spec.name)))
            .map_err(|error| failure(&error))?;
        Ok(created.id)
    }

    /// # Errors
    /// Returns `HostError::Absent` when no such container exists.
    fn start(&self, id: &str) -> Result<(), HostError> {
        let client = self.bridge.client();
        self.bridge
            .wait(client.containers().start(id))
            .map_err(|error| failure(&error))
    }

    /// # Errors
    /// Returns `HostError::Absent` when no such container exists.
    fn stop(&self, id: &str) -> Result<(), HostError> {
        let client = self.bridge.client();
        self.bridge
            .wait(client.containers().stop(id, Some(STOP_SECONDS)))
            .map_err(|error| failure(&error))
    }

    /// Removes a container without forcing it.
    ///
    /// A running container is reported as a conflict rather than killed: an
    /// extension that means to stop it can say so.
    ///
    /// # Errors
    /// Returns `HostError::Absent` when no such container exists and
    /// `HostError::Conflict` when it is still running.
    fn remove(&self, id: &str) -> Result<(), HostError> {
        let client = self.bridge.client();
        self.bridge
            .wait(client.containers().remove(id, false, false))
            .map_err(|error| failure(&error))
    }

    fn pause(&self, id: &str) -> Result<(), HostError> {
        let client = self.bridge.client();
        self.bridge
            .wait(client.containers().pause(id))
            .map_err(|error| failure(&error))
    }

    fn unpause(&self, id: &str) -> Result<(), HostError> {
        let client = self.bridge.client();
        self.bridge
            .wait(client.containers().unpause(id))
            .map_err(|error| failure(&error))
    }

    fn restart(&self, id: &str) -> Result<(), HostError> {
        let client = self.bridge.client();
        self.bridge
            .wait(client.containers().restart(id, Some(STOP_SECONDS)))
            .map_err(|error| failure(&error))
    }

    fn kill(&self, id: &str, signal: &str) -> Result<(), HostError> {
        let client = self.bridge.client();
        self.bridge
            .wait(client.containers().kill(id, signal))
            .map_err(|error| failure(&error))
    }

    fn execution_kill(&self, id: &str, signal: &str) -> Result<(), HostError> {
        let client = self.bridge.client();
        self.bridge
            .wait(client.executions().signal(id, signal))
            .map_err(|error| failure(&error))
    }

    fn execution_remove(&self, id: &str) -> Result<(), HostError> {
        let client = self.bridge.client();
        self.bridge.wait(client.executions().remove(id)).map_err(|error| failure(&error))
    }

    fn execute(
        &self,
        id: &str,
        command: &[String],
        user: Option<&str>,
        working_directory: Option<&str>,
    ) -> Result<String, HostError> {
        let config = ExecConfig {
            command: command.to_vec(),
            user: user.unwrap_or_default().to_owned(),
            working_dir: working_directory.unwrap_or_default().to_owned(),
            ..ExecConfig::default()
        };
        let client = self.bridge.client();
        let created = self
            .bridge
            .wait(client.executions().create(id, &config))
            .map_err(|error| failure(&error))?;
        let start = ExecStart {
            detach: true,
            ..ExecStart::default()
        };
        self.bridge
            .wait(client.executions().start_detached(&created.id, &start))
            .map_err(|error| failure(&error))?;
        Ok(created.id)
    }
}

#[cfg(test)]
mod tests {
    use hl_extension::port::{ContainerCreateSpec, ContainerPort, ContainerVolumeMount};
    use super::ContainerLifecycle;

    #[test]
    fn configured_creation_maps_only_supported_native_authority() {
        let request = ContainerLifecycle::creation(&ContainerCreateSpec {
            image: "alpine:3.20".into(), name: "worker".into(), entrypoint: Some(vec!["/init".into()]),
            command: vec!["serve".into()], environment: vec![("MODE".into(), "agent".into())],
            working_directory: Some("/work".into()), user: Some("1000".into()),
            labels: vec![("owner".into(), "agent".into())],
            mounts: vec![ContainerVolumeMount { volume: "cache".into(), target: "/cache".into(), read_only: true }],
            network: Some("private".into()),
            ports: vec![ContainerPort { container: 8080, host: Some(18080), protocol: "tcp".into() }],
            memory_mb: Some(512), cpus: Some(2), pids_limit: Some(128),
        });
        assert_eq!(request.image, "alpine:3.20");
        assert_eq!(request.entrypoint.as_deref(), Some(["/init".into()].as_slice()));
        assert_eq!(request.cmd.as_deref(), Some(["serve".into()].as_slice()));
        assert_eq!(request.env.as_deref(), Some(["MODE=agent".into()].as_slice()));
        assert_eq!(request.working_dir.as_deref(), Some("/work"));
        assert_eq!(request.user.as_deref(), Some("1000"));
        assert_eq!(request.labels.get("owner").map(String::as_str), Some("agent"));
        let host = request.host_config.expect("host config");
        assert_eq!(host.mounts.len(), 1);
        assert_eq!((host.mounts[0].kind.as_str(), host.mounts[0].source.as_str(), host.mounts[0].target.as_str()),
            ("volume", "cache", "/cache"));
        assert!(host.mounts[0].read_only);
        assert_eq!((host.memory, host.nano_cpus, host.pids_limit), (512 * 1024 * 1024, 2_000_000_000, Some(128)));
        assert_eq!(host.network_mode, "private");
        let binding = host.port_bindings.0["8080/tcp"].as_ref().expect("published");
        assert_eq!((binding[0].host_ip.as_str(), binding[0].host_port.as_str()), ("127.0.0.1", "18080"));
        assert!(request.exposed_ports.0.contains_key("8080/tcp"));
        assert!(request.networking_config.expect("network").endpoints_config.0.contains_key("private"));
    }
}
