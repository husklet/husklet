use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::IpAddr,
    path::PathBuf,
};

use super::{Execution, Guest, Process, Rootfs};
use crate::{Error, Healthcheck, Isolation, Mount, MountSource, Resources, RestartPolicy, Result, VolumeSpec};

/// Immutable launch definition persisted with a container.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContainerSpec {
    pub name: Option<String>,
    pub labels: BTreeMap<String, String>,
    /// OCI image name used to prepare this rootfs, independent from its snapshot path.
    pub image: Option<hl_images::Reference>,
    pub rootfs: Rootfs,
    pub guest: Guest,
    /// Execution backend requested for this container and its child processes.
    #[serde(default)]
    pub execution: Execution,
    pub process: Process,
    pub hostname: Option<String>,
    pub hosts: BTreeMap<String, IpAddr>,
    pub mounts: Vec<Mount>,
    pub resources: Resources,
    pub isolation: Isolation,
    pub network_mode: crate::NetworkMode,
    pub healthcheck: Option<Healthcheck>,
    pub restart: RestartPolicy,
    pub removal: crate::RemovalPolicy,
    /// Signal delivered by a graceful stop before its timeout expires.
    pub stop_signal: crate::Signal,
    /// Default grace period used when a stop request does not override it.
    #[serde(default = "default_stop_timeout_seconds")]
    pub stop_timeout_seconds: u64,
    /// Declared process ports, including ports that are not host-published.
    pub ports: BTreeSet<crate::Port>,
    /// Durable host-to-container publications, re-applied on every process generation.
    pub publish: Vec<crate::Publication>,
}

impl ContainerSpec {
    /// Creates a container backed by a durable OCI rootfs lease.
    #[must_use]
    pub fn new(rootfs: hl_images::rootfs::Reference, process: Process) -> Self {
        Self {
            name: None,
            labels: BTreeMap::new(),
            image: None,
            rootfs: Rootfs::Image(rootfs),
            guest: Guest::default(),
            execution: Execution::default(),
            process,
            hostname: None,
            hosts: BTreeMap::new(),
            mounts: Vec::new(),
            resources: Resources::default(),
            isolation: Isolation::default(),
            network_mode: crate::NetworkMode::default(),
            healthcheck: None,
            restart: RestartPolicy::default(),
            removal: crate::RemovalPolicy::default(),
            stop_signal: crate::Signal::default(),
            stop_timeout_seconds: default_stop_timeout_seconds(),
            ports: BTreeSet::new(),
            publish: Vec::new(),
        }
    }

    /// Creates a container from an unmanaged host directory.
    ///
    /// This is an advanced primitive for imported/test filesystems. OCI callers should use
    /// [`Self::new`] so snapshot reachability is held until container removal.
    #[must_use]
    pub fn from_directory(rootfs: impl Into<PathBuf>, process: Process) -> Self {
        Self {
            name: None,
            labels: BTreeMap::new(),
            image: None,
            rootfs: Rootfs::Directory(rootfs.into()),
            guest: Guest::default(),
            execution: Execution::default(),
            process,
            hostname: None,
            hosts: BTreeMap::new(),
            mounts: Vec::new(),
            resources: Resources::default(),
            isolation: Isolation::default(),
            network_mode: crate::NetworkMode::default(),
            healthcheck: None,
            restart: RestartPolicy::default(),
            removal: crate::RemovalPolicy::default(),
            stop_signal: crate::Signal::default(),
            stop_timeout_seconds: default_stop_timeout_seconds(),
            ports: BTreeSet::new(),
            publish: Vec::new(),
        }
    }

    #[must_use]
    pub fn name(mut self, value: impl Into<String>) -> Self {
        self.name = Some(value.into());
        self
    }

    #[must_use]
    pub fn label(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.labels.insert(name.into(), value.into());
        self
    }

    #[must_use]
    pub fn image(mut self, value: hl_images::Reference) -> Self {
        self.image = Some(value);
        self
    }

    #[must_use]
    pub const fn guest(mut self, value: Guest) -> Self {
        self.guest = value;
        self
    }

    #[must_use]
    pub const fn execution(mut self, value: Execution) -> Self {
        self.execution = value;
        self
    }

    #[must_use]
    pub fn hostname(mut self, value: impl Into<String>) -> Self {
        self.hostname = Some(value.into());
        self
    }

    #[must_use]
    pub fn host(mut self, name: impl Into<String>, address: IpAddr) -> Self {
        self.hosts.insert(name.into(), address);
        self
    }

    #[must_use]
    pub fn mount(mut self, value: Mount) -> Self {
        self.mounts.push(value);
        self
    }

    #[must_use]
    pub const fn resources(mut self, value: Resources) -> Self {
        self.resources = value;
        self
    }

    #[must_use]
    pub const fn isolation(mut self, value: Isolation) -> Self {
        self.isolation = value;
        self
    }

    #[must_use]
    pub const fn network_mode(mut self, value: crate::NetworkMode) -> Self {
        self.network_mode = value;
        self
    }

    #[must_use]
    pub fn healthcheck(mut self, value: Healthcheck) -> Self {
        self.healthcheck = Some(value);
        self
    }

    #[must_use]
    pub const fn restart(mut self, value: RestartPolicy) -> Self {
        self.restart = value;
        self
    }

    #[must_use]
    pub const fn removal(mut self, value: crate::RemovalPolicy) -> Self {
        self.removal = value;
        self
    }

    #[must_use]
    pub const fn stop_signal(mut self, value: crate::Signal) -> Self {
        self.stop_signal = value;
        self
    }

    #[must_use]
    pub const fn stop_timeout_seconds(mut self, value: u64) -> Self {
        self.stop_timeout_seconds = value;
        self
    }

    #[must_use]
    pub fn expose(mut self, value: crate::Port) -> Self {
        self.ports.insert(value);
        self
    }

    #[must_use]
    pub fn publish(mut self, value: crate::Publication) -> Self {
        self.ports.insert(value.port);
        self.publish.push(value);
        self
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self.labels.keys().any(String::is_empty) {
            return Err(Error::InvalidSpec("container label name must not be empty".into()));
        }
        if let Some(healthcheck) = &self.healthcheck {
            healthcheck.validate()?;
        }
        self.restart.validate()?;
        if self.removal == crate::RemovalPolicy::Automatic && self.restart != RestartPolicy::Never {
            return Err(Error::InvalidSpec(
                "automatic removal cannot be combined with a restart policy".into(),
            ));
        }
        self.validate_publish()?;
        if self.network_mode == crate::NetworkMode::Host {
            if self.isolation.network_isolated {
                return Err(Error::InvalidSpec(
                    "host network mode requires networking to be enabled".into(),
                ));
            }
            if !self.publish.is_empty() {
                return Err(Error::InvalidSpec("host network mode cannot publish ports".into()));
            }
        }
        if self.process.program.is_empty() {
            return Err(Error::InvalidSpec("process program must not be empty".into()));
        }
        if matches!(&self.rootfs, Rootfs::Directory(path) if !path.is_absolute()) {
            return Err(Error::InvalidSpec("rootfs must be an absolute path".into()));
        }
        if !self.process.working_dir.is_absolute() {
            return Err(Error::InvalidSpec("working directory must be absolute".into()));
        }
        if let Some(name) = self.name.as_deref() {
            let mut characters = name.chars();
            if !characters.next().is_some_and(|value| value.is_ascii_alphanumeric())
                || !characters.all(|value| value.is_ascii_alphanumeric() || matches!(value, '_' | '.' | '-'))
            {
                return Err(Error::InvalidSpec(
                    "name must start with an ASCII letter or digit and contain only letters, digits, '_', '.', or '-'"
                        .into(),
                ));
            }
        }
        if self.process.env.keys().any(|key| key.is_empty() || key.contains('=')) {
            return Err(Error::InvalidSpec(
                "environment names must be non-empty and exclude '='".into(),
            ));
        }
        if self.process.program.contains('\0')
            || self.process.args.iter().any(|value| value.contains('\0'))
            || self
                .process
                .env
                .iter()
                .any(|(key, value)| key.contains('\0') || value.contains('\0'))
        {
            return Err(Error::InvalidSpec("process strings must not contain NUL".into()));
        }
        if self.hostname.as_deref().is_some_and(str::is_empty) {
            return Err(Error::InvalidSpec("hostname must not be empty".into()));
        }
        if self
            .hosts
            .keys()
            .any(|name| name.is_empty() || name.chars().any(|value| value.is_whitespace() || value == '\0'))
        {
            return Err(Error::InvalidSpec(
                "extra host names must be non-empty and contain no whitespace or NUL".into(),
            ));
        }
        self.validate_mounts()
    }

    fn validate_publish(&self) -> Result<()> {
        let mut host = Vec::new();
        for publish in &self.publish {
            if publish.port.guest == 0 {
                return Err(Error::InvalidSpec("invalid published TCP port".into()));
            }
            if publish.host != 0
                && host
                    .iter()
                    .any(|existing: &crate::Publication| existing.conflicts(*publish))
            {
                return Err(Error::InvalidSpec(format!(
                    "host port {}:{} is published more than once",
                    publish.host_ip, publish.host
                )));
            }
            host.push(*publish);
        }
        Ok(())
    }

    fn validate_mounts(&self) -> Result<()> {
        let mut targets = std::collections::BTreeSet::new();
        for mount in &self.mounts {
            if !mount.target.is_absolute()
                || mount.target.components().any(|component| {
                    !matches!(
                        component,
                        std::path::Component::RootDir | std::path::Component::Normal(_)
                    )
                })
            {
                return Err(Error::InvalidSpec(
                    "mount target must be a normalized absolute path".into(),
                ));
            }
            match &mount.source {
                MountSource::Bind(source) if !source.is_absolute() => {
                    return Err(Error::InvalidSpec("bind mount source must be absolute".into()));
                }
                MountSource::Volume(name) | MountSource::Anonymous(name) | MountSource::Tmpfs(name) => {
                    VolumeSpec::new(name).validate()?;
                }
                MountSource::Bind(_) => {}
            }
            if !targets.insert(&mount.target) {
                return Err(Error::InvalidSpec(format!(
                    "duplicate mount target {}",
                    mount.target.display()
                )));
            }
        }
        Ok(())
    }
}

const fn default_stop_timeout_seconds() -> u64 {
    10
}

#[cfg(test)]
mod tests {
    use super::ContainerSpec;
    use crate::{Guest, Process, Size};

    #[test]
    fn terminal_size_rejects_empty_dimensions() {
        assert!(Size::new(0, 80).is_err());
        assert!(Size::new(24, 0).is_err());
        assert_eq!(Size::new(24, 80).unwrap(), Size::default());
    }

    #[test]
    fn stop_timeout_defaults_and_round_trips_durably() {
        let spec = ContainerSpec::from_directory("/rootfs", Process::new("/bin/true"));
        assert_eq!(spec.stop_timeout_seconds, 10);
        let mut stored = serde_json::to_value(spec.stop_timeout_seconds(23)).unwrap();
        assert_eq!(stored["stop_timeout_seconds"], 23);
        let restored: ContainerSpec = serde_json::from_value(stored.clone()).unwrap();
        assert_eq!(restored.stop_timeout_seconds, 23);
        stored.as_object_mut().unwrap().remove("stop_timeout_seconds");
        let legacy: ContainerSpec = serde_json::from_value(stored).unwrap();
        assert_eq!(legacy.stop_timeout_seconds, 10);
    }

    #[test]
    fn guest_backend_follows_oci_platform_architecture() {
        assert_eq!(
            Guest::for_platform(&hl_images::Platform::linux_arm64()).unwrap(),
            Guest::Aarch64
        );
        assert_eq!(
            Guest::for_platform(&hl_images::Platform::linux_amd64()).unwrap(),
            Guest::X86_64
        );
    }

    #[test]
    fn oci_platform_selects_the_matching_engine_frontend() {
        assert_eq!(
            Guest::for_platform(&hl_images::Platform::linux_arm64()).unwrap(),
            Guest::Aarch64
        );
        assert_eq!(
            Guest::for_platform(&hl_images::Platform::linux_amd64()).unwrap(),
            Guest::X86_64
        );
        assert!(Guest::for_platform(&hl_images::Platform::new("linux", "arm", None)).is_err());
    }

    #[test]
    fn named_user_and_group_resolve_from_linux_databases() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("etc")).unwrap();
        std::fs::write(root.path().join("etc/passwd"), "worker:x:123:456::/tmp:/bin/sh\n").unwrap();
        std::fs::write(root.path().join("etc/group"), "staff:x:789:\n").unwrap();
        assert_eq!(Process::resolve_user("worker:staff", root.path()).unwrap(), (123, 789));
        assert_eq!(Process::resolve_user("worker", root.path()).unwrap(), (123, 456));
        assert_eq!(Process::resolve_user("123:staff", root.path()).unwrap(), (123, 789));
        assert_eq!(Process::resolve_user("worker:789", root.path()).unwrap(), (123, 789));
        assert!(Process::resolve_user("missing", root.path()).is_err());
        std::fs::write(
            root.path().join("etc/passwd"),
            "worker:x:123:456::/tmp:/bin/sh\nworker:x:124:457::/tmp:/bin/sh\n",
        )
        .unwrap();
        assert!(
            Process::resolve_user("worker", root.path())
                .unwrap_err()
                .to_string()
                .contains("ambiguous")
        );
    }

    #[test]
    fn host_network_mode_is_typed_and_rejects_isolation_and_publication() {
        let root = tempfile::tempdir().unwrap();
        let enabled = crate::Isolation {
            network_isolated: false,
            ..crate::Isolation::default()
        };
        let spec = ContainerSpec::from_directory(root.path(), Process::new("/bin/true"))
            .isolation(enabled)
            .network_mode(crate::NetworkMode::Host);
        assert!(spec.validate().is_ok());
        assert!(spec.clone().isolation(crate::Isolation::default()).validate().is_err());
        assert!(
            spec.publish(crate::Publication::tcp(std::net::Ipv4Addr::LOCALHOST, 8080, 80).unwrap())
                .validate()
                .is_err()
        );
    }
}
