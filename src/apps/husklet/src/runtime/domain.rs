//! One durable container execution domain per workspace.

use crate::config::WorkspaceConfig;
use crate::paths;
use hl_container::{
    Config, ContainerSpec, Containers, Devices, Guest, Isolation, Mount, Prune, Resources, Sandbox,
};
use hl_images::remote::{Auth, Registry};
use hl_images::{Images, Platform, Reference, RuntimeOverrides};
use hl_ws::Arch;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

mod lifecycle;

use lifecycle::{Lease, Peer, Shutdown};

const CONTAINER: &str = "workspace";
const SIGNATURE: &str = "husklet.workspace.signature";
const PROTOCOL: &str = "1";

/// Owns the host process and socket serving one workspace's persistent execution domain.
pub struct Domain {
    directory: PathBuf,
}

impl Domain {
    pub fn new(workspace: &WorkspaceConfig) -> Self {
        Self {
            directory: workspace.storage_dir(&paths::hl_root()).join("runtime"),
        }
    }

    pub fn socket(&self) -> PathBuf {
        self.directory.join("domain.sock")
    }

    /// Starts the workspace domain process when needed and waits until its API accepts connections.
    pub fn ensure(&self, workspace: &WorkspaceConfig) -> io::Result<PathBuf> {
        std::fs::create_dir_all(&self.directory)?;
        let _startup = Lease::acquire_wait(
            self.directory.join("startup.lock"),
            std::time::Duration::from_secs(180),
        )?;
        if let Ok(connection) = std::os::unix::net::UnixStream::connect(self.socket()) {
            if PublishedProtocol::new(&self.directory).compatible()? {
                PublishedConfiguration::new(&self.directory).validate(workspace)?;
                return Ok(self.socket());
            }
            Peer::new(connection)?.stop(std::time::Duration::from_secs(10), || {
                std::os::unix::net::UnixStream::connect(self.socket())
            })?;
            Lease::wait_available(
                self.directory.join("domain.lock"),
                std::time::Duration::from_secs(10),
            )?;
        }
        match std::fs::remove_file(self.socket()) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let output = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.directory.join("domain.log"))?;
        let errors = output.try_clone()?;
        let mut command = std::process::Command::new(std::env::current_exe()?);
        command
            .args(["--worker", "domain", &workspace.name])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(output))
            .stderr(std::process::Stdio::from(errors));
        // SAFETY: the hook runs after `fork` and before `exec` in the child. It only invokes
        // the async-signal-safe `setsid` syscall and converts its errno into an I/O error.
        unsafe {
            use std::os::unix::process::CommandExt;
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
        let child = command.spawn()?;
        self.wait_for_start(child, std::time::Duration::from_secs(180))
    }

    fn wait_for_start(
        &self,
        mut child: std::process::Child,
        timeout: std::time::Duration,
    ) -> io::Result<PathBuf> {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if std::os::unix::net::UnixStream::connect(self.socket()).is_ok()
                && PublishedProtocol::new(&self.directory).compatible()?
            {
                return Ok(self.socket());
            }
            if let Some(status) = child.try_wait()? {
                return Err(io::Error::other(format!(
                    "workspace execution domain exited before publishing its API ({status}); see {}",
                    self.directory.join("domain.log").display()
                )));
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "workspace execution domain did not publish {}",
                self.socket().display()
            ),
        ))
    }

    pub async fn serve(workspace: &WorkspaceConfig) -> io::Result<()> {
        let owner = Self::new(workspace);
        tokio::fs::create_dir_all(&owner.directory).await?;
        let _lease = Lease::acquire(owner.directory.join("domain.lock"))?;
        let (containers, platform) = Runtime::open(workspace).await?;
        Runtime::remove_legacy_terminals(&containers).await?;
        Runtime::remove_stale_executions(&containers).await?;
        Runtime::ensure_container(&containers, workspace).await?;
        let configuration = PublishedConfiguration::new(&owner.directory);
        let protocol = PublishedProtocol::new(&owner.directory);
        protocol.publish()?;
        configuration.publish(workspace)?;
        let server = hl_daemon::Daemon::new(containers.clone())
            .platform(platform)
            .release(hl_daemon::Release::new(env!("CARGO_PKG_VERSION")))
            .server(owner.socket());
        let served = server
            .serve_with_shutdown(Shutdown::wait())
            .await
            .map_err(io::Error::other);
        let stopped = containers
            .shutdown(std::time::Duration::from_secs(5))
            .await
            .map_err(io::Error::other);
        let result = match (served, stopped) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(server), Err(cleanup)) => {
                hl_log::hl_error!(
                    hl_log::tag::CONTAINER,
                    "workspace domain server failed error={server}; cleanup failed error={cleanup}"
                );
                Err(server)
            }
        };
        let unpublished = configuration.remove().and_then(|()| protocol.remove());
        match (result, unpublished) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(runtime), Err(cleanup)) => {
                hl_log::hl_error!(
                    hl_log::tag::CONTAINER,
                    "workspace runtime failed error={runtime}; configuration cleanup failed error={cleanup}"
                );
                Err(runtime)
            }
        }
    }
}

struct PublishedProtocol {
    path: PathBuf,
}

impl PublishedProtocol {
    fn new(directory: &Path) -> Self {
        Self {
            path: directory.join("protocol"),
        }
    }

    fn publish(&self) -> io::Result<()> {
        hl_fs::File::from(self.path.clone()).replace(PROTOCOL)
    }

    fn compatible(&self) -> io::Result<bool> {
        match std::fs::read_to_string(&self.path) {
            Ok(value) => Ok(value.trim() == PROTOCOL),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn remove(&self) -> io::Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

struct PublishedConfiguration {
    path: PathBuf,
}

impl PublishedConfiguration {
    fn new(directory: &Path) -> Self {
        Self {
            path: directory.join("configuration.sha256"),
        }
    }

    fn publish(&self, workspace: &WorkspaceConfig) -> io::Result<()> {
        hl_fs::File::from(self.path.clone()).replace(Configuration::new(workspace).signature())
    }

    fn validate(&self, workspace: &WorkspaceConfig) -> io::Result<()> {
        let effective = std::fs::read_to_string(&self.path).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("live workspace domain has no verifiable configuration identity: {error}"),
            )
        })?;
        let requested = Configuration::new(workspace).signature();
        if effective.trim() == requested {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "workspace settings changed while its execution domain is running; stop the workspace runtime before reopening",
            ))
        }
    }

    fn remove(&self) -> io::Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

struct Runtime;

impl Runtime {
    async fn open(workspace: &WorkspaceConfig) -> io::Result<(Containers, Platform)> {
        let images = Images::open(paths::images_dir()).map_err(io::Error::other)?;
        let platform = Self::platform(workspace.arch);
        let devices = Devices::new().with(crate::runtime::devices::Workspace::new(workspace)?);
        let root = workspace.storage_dir(&paths::hl_root()).join("containers");
        let containers = Containers::builder(Config::new(root))
            .images(images)
            .devices(devices)
            .build()
            .await
            .map_err(io::Error::other)?;
        Ok((containers, platform))
    }

    async fn ensure_container(
        containers: &Containers,
        workspace: &WorkspaceConfig,
    ) -> io::Result<()> {
        let signature = Configuration::new(workspace).signature();
        match containers.inspect(CONTAINER).await {
            Ok(container) => {
                let stored = container.spec.labels.get(SIGNATURE);
                let legacy = Configuration::new(workspace).legacy_signature();
                let current = stored == Some(&signature);
                let legacy_match = stored == Some(&legacy);
                if !current && !legacy_match {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "workspace runtime configuration changed; reset its runtime before reopening",
                    ));
                }
                if legacy_match {
                    containers
                        .set_label(CONTAINER, SIGNATURE, &signature)
                        .await
                        .map_err(io::Error::other)?;
                    hl_log::hl_info!(
                        hl_log::tag::CONTAINER,
                        "replaced legacy workspace configuration label with a digest"
                    );
                }
                if !container.state.is_active() {
                    containers
                        .start(CONTAINER)
                        .await
                        .map_err(io::Error::other)?;
                }
                return Ok(());
            }
            Err(hl_container::Error::NotFound(_)) => {}
            Err(error) => return Err(io::Error::other(error)),
        }

        let images = containers.images().map_err(io::Error::other)?;
        let reference: Reference = workspace.image.parse().map_err(io::Error::other)?;
        let platform = Self::platform(workspace.arch);
        let image = match images.resolve(&reference).map_err(io::Error::other)? {
            Some(image) => image,
            None => images
                .pull(&Registry::new(Auth::Anonymous), reference, &platform)
                .await
                .map_err(io::Error::other)?,
        };
        let unpacked = images.unpack(&image, &platform).map_err(io::Error::other)?;
        let overrides = RuntimeOverrides {
            entrypoint: Some(vec!["/bin/sh".into()]),
            command: Some(vec![
                "-c".into(),
                "while :; do sleep 2147483647 & wait $!; done".into(),
            ]),
            environment: Configuration::new(workspace).environment(),
            working_directory: Some("/root".into()),
            user: Some("0:0".into()),
        };
        containers
            .create_image(&unpacked, overrides, |spec| {
                Configuration::new(workspace).container(spec, signature)
            })
            .await
            .map_err(io::Error::other)?;
        containers.start(CONTAINER).await.map_err(io::Error::other)
    }

    async fn remove_legacy_terminals(containers: &Containers) -> io::Result<()> {
        let removed = containers
            .prune(&Prune::default().without_label(SIGNATURE))
            .await
            .map_err(io::Error::other)?;
        if !removed.is_empty() {
            hl_log::hl_info!(
                hl_log::tag::CONTAINER,
                "removed {} legacy workspace terminal containers",
                removed.len()
            );
        }
        Ok(())
    }

    async fn remove_stale_executions(containers: &Containers) -> io::Result<()> {
        let executions = containers.executions();
        let stale = executions.list().await.map_err(io::Error::other)?;
        for execution in &stale {
            executions
                .remove(&execution.id)
                .await
                .map_err(io::Error::other)?;
        }
        if !stale.is_empty() {
            hl_log::hl_info!(
                hl_log::tag::CONTAINER,
                "removed {} stale workspace executions",
                stale.len()
            );
        }
        Ok(())
    }

    fn platform(arch: Arch) -> Platform {
        match arch {
            Arch::Arm64 => Platform::linux_arm64(),
            Arch::Amd64 => Platform::linux_amd64(),
        }
    }
}

struct Configuration<'a>(&'a WorkspaceConfig);

impl<'a> Configuration<'a> {
    fn new(workspace: &'a WorkspaceConfig) -> Self {
        Self(workspace)
    }

    fn container(&self, mut spec: ContainerSpec, signature: String) -> ContainerSpec {
        spec = spec
            .name(CONTAINER)
            .hostname(self.hostname())
            .label(SIGNATURE, signature)
            .guest(match self.0.arch {
                Arch::Arm64 => Guest::Aarch64,
                Arch::Amd64 => Guest::X86_64,
            })
            .resources(Resources {
                memory_bytes: self
                    .0
                    .memory_mb
                    .map_or(0, |value| u64::from(value) * 1024 * 1024),
                cpu_count: self.0.cpus.unwrap_or(0),
                ..Resources::default()
            })
            .isolation(Isolation {
                sandbox: Sandbox::Disabled,
                network_isolated: false,
                ..Isolation::default()
            });
        for mount in &self.0.mounts {
            spec = spec.mount(if mount.ro {
                Mount::read_only(&mount.host, &mount.container)
            } else {
                Mount::read_write(&mount.host, &mount.container)
            });
        }
        spec
    }

    fn environment(&self) -> BTreeMap<String, String> {
        let mut values = BTreeMap::from([
            ("TERM".into(), "xterm-256color".into()),
            ("HOME".into(), "/root".into()),
            (
                "PATH".into(),
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into(),
            ),
        ]);
        values.extend(self.0.env.iter().cloned());
        values
    }

    fn signature(&self) -> String {
        use sha2::Digest as _;

        let digest = sha2::Sha256::digest(self.legacy_signature().as_bytes());
        let mut signature = String::with_capacity(digest.len() * 2);
        for byte in digest {
            use std::fmt::Write as _;
            let _ = write!(signature, "{byte:02x}");
        }
        signature
    }

    fn legacy_signature(&self) -> String {
        let mut value = String::new();
        for item in [
            self.0.image.as_str(),
            self.0.arch.as_str(),
            self.0.shell.as_deref().unwrap_or_default(),
        ] {
            Self::field(&mut value, item);
        }
        for (name, item) in &self.0.env {
            Self::field(&mut value, name);
            Self::field(&mut value, item);
        }
        for mount in &self.0.mounts {
            Self::field(&mut value, &mount.host);
            Self::field(&mut value, &mount.container);
            Self::field(&mut value, if mount.ro { "ro" } else { "rw" });
        }
        for item in [
            self.0
                .cpus
                .map(|value| value.to_string())
                .unwrap_or_default(),
            self.0
                .memory_mb
                .map(|value| value.to_string())
                .unwrap_or_default(),
            self.0.docker_sock.to_string(),
            self.0.gui.to_string(),
            format!("{:?}", self.0.vpn),
            format!("{:?}", self.0.cuda),
        ] {
            Self::field(&mut value, &item);
        }
        value
    }

    fn field(output: &mut String, value: &str) {
        use std::fmt::Write as _;
        let _ = write!(output, "{}:{value}", value.len());
    }

    fn hostname(&self) -> String {
        let value: String = self
            .0
            .name
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '-' {
                    character
                } else {
                    '-'
                }
            })
            .collect();
        match value.trim_matches('-') {
            "" => "workspace".to_owned(),
            value => value.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests;
