use crate::{service::ProcessConfig, Error, Result};

pub(super) struct Spec(hl_engine::MachineSpec);

impl TryFrom<&ProcessConfig> for Spec {
    type Error = Error;

    fn try_from(launch: &ProcessConfig) -> Result<Self> {
        let guest = match launch.guest {
            crate::Guest::Aarch64 => hl_engine::Guest::Aarch64,
            crate::Guest::X86_64 => hl_engine::Guest::X86_64,
        };
        let mut spec = hl_engine::MachineSpec::new(guest, &launch.process.program);
        Self::process(&mut spec, launch)?;
        Self::filesystem(&mut spec, launch)?;
        Self::resources(&mut spec, launch);
        Self::network(&mut spec, launch)?;
        if launch.checkpoint_directory.is_some() || launch.restore_directory.is_some() {
            spec.checkpoint.enabled = true;
            spec.checkpoint
                .capture_directory
                .clone_from(&launch.checkpoint_directory);
            spec.checkpoint
                .restore_directory
                .clone_from(&launch.restore_directory);
        }
        spec.extensions.clone_from(&launch.extensions);
        Ok(Self(spec))
    }
}

impl From<Spec> for hl_engine::MachineSpec {
    fn from(spec: Spec) -> Self {
        spec.0
    }
}

impl Spec {
    fn process(spec: &mut hl_engine::MachineSpec, launch: &ProcessConfig) -> Result<()> {
        spec.process
            .argv
            .extend(launch.process.args.iter().map(Into::into));
        spec.process.env.extend(
            launch
                .process
                .env
                .iter()
                .map(|(name, value)| (name.into(), value.into())),
        );
        launch
            .process
            .working_dir
            .as_os_str()
            .clone_into(&mut spec.process.cwd);
        spec.process.domain = launch.domain;
        spec.process.terminal = launch
            .terminal
            .map(|size| hl_engine::Size::new(size.rows(), size.columns()))
            .transpose()
            .map_err(|error| Error::Runtime(error.to_string()))?;
        spec.identity.uid = launch
            .process
            .uid
            .map(|uid| {
                u32::try_from(uid)
                    .map_err(|_| Error::InvalidSpec("process uid must be nonnegative".into()))
            })
            .transpose()?;
        spec.identity.gid = launch
            .process
            .gid
            .map(|gid| {
                u32::try_from(gid)
                    .map_err(|_| Error::InvalidSpec("process gid must be nonnegative".into()))
            })
            .transpose()?;
        spec.identity.hostname = launch.hostname.as_ref().map(Into::into);
        spec.security.sandbox = match launch.isolation.sandbox {
            crate::Sandbox::Disabled => hl_engine::Sandbox::Disabled,
            crate::Sandbox::Enabled => hl_engine::Sandbox::Enabled,
            crate::Sandbox::SentryOnly => hl_engine::Sandbox::SentryOnly,
        };
        Ok(())
    }

    fn filesystem(spec: &mut hl_engine::MachineSpec, launch: &ProcessConfig) -> Result<()> {
        use hl_engine::spec::{InitialOwnership, TreeSource};

        spec.filesystem.root = Some(match &launch.overlay {
            Some(overlay) => TreeSource::Overlay {
                lower: vec![TreeSource::HostDirectory(overlay.lower.clone())],
                upper: overlay.upper.clone(),
                work: overlay.work.clone(),
            },
            None => TreeSource::HostDirectory(launch.rootfs.clone()),
        });
        spec.filesystem.read_only = launch.isolation.read_only_root;
        spec.filesystem.coherence = Some(
            hl_engine::spec::CoherenceHandle::from_host_file(&launch.filesystem_generation)
                .map_err(|error| Error::InvalidSpec(error.to_string()))?,
        );
        spec.filesystem.ownership = launch
            .owners
            .iter()
            .map(|(path, uid, gid)| InitialOwnership {
                path: std::path::Path::new("/").join(path),
                uid: *uid,
                gid: *gid,
            })
            .collect();
        spec.filesystem.mounts = launch
            .mounts
            .iter()
            .map(|mount| hl_engine::extension::HostBindEntry {
                path: mount.target.clone(),
                host: mount.source.clone(),
                access: match mount.access {
                    crate::Access::ReadOnly => hl_engine::extension::BindAccess::ReadOnly,
                    crate::Access::ReadWrite => hl_engine::extension::BindAccess::ReadWrite,
                },
            })
            .collect();
        Ok(())
    }

    fn resources(spec: &mut hl_engine::MachineSpec, launch: &ProcessConfig) {
        spec.resources.memory_bytes =
            (launch.resources.memory_bytes != 0).then_some(launch.resources.memory_bytes);
        spec.resources.process_limit =
            (launch.resources.process_count != 0).then_some(launch.resources.process_count);
        spec.resources.cpu_limit =
            (launch.resources.cpu_count != 0).then_some(launch.resources.cpu_count);
    }

    fn network(spec: &mut hl_engine::MachineSpec, launch: &ProcessConfig) -> Result<()> {
        use hl_engine::spec::NetworkMode;

        spec.network.mode = if launch.network_mode == crate::NetworkMode::Host {
            if !launch.networks.is_empty() || !launch.publish.is_empty() {
                return Err(Error::InvalidNetwork(
                    "host networking cannot carry endpoints or port publications".into(),
                ));
            }
            NetworkMode::Host
        } else if launch.isolation.network_isolated {
            NetworkMode::None
        } else {
            NetworkMode::Virtual
        };
        if spec.network.mode == NetworkMode::None {
            spec.network.namespace = Some(
                hl_engine::network::Namespace::new(&launch.network_namespace)
                    .map_err(|error| Error::InvalidNetwork(error.to_string()))?,
            );
        }
        if spec.network.mode == NetworkMode::Virtual {
            let namespace = launch
                .networks
                .iter()
                .find(|network| network.driver == crate::NetworkDriver::Bridge)
                .map_or(launch.network_namespace.as_str(), |network| {
                    network.namespace.as_str()
                });
            spec.network.namespace = Some(
                hl_engine::network::Namespace::new(namespace)
                    .map_err(|error| Error::InvalidNetwork(error.to_string()))?,
            );
        }
        for network in &launch.networks {
            if let (Some(bridge), Some(address), Some(prefix)) =
                (&network.bridge, network.address, network.prefix)
            {
                let bridge = hl_engine::network::Bridge::new(bridge)
                    .map_err(|error| Error::InvalidNetwork(error.to_string()))?;
                spec.network.interfaces.push(
                    hl_engine::network::Interface::new(bridge, address, prefix)
                        .map_err(|error| Error::InvalidNetwork(error.to_string()))?,
                );
            }
        }
        for publish in &launch.publish {
            let rule = hl_engine::network::Rule::new(publish.host, publish.port.guest)
                .map_err(|error| Error::InvalidNetwork(error.to_string()))?
                .address(publish.host_ip);
            spec.network.port_forwards.push(rule);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Spec;
    use crate::service::ProcessConfig;

    fn launch() -> ProcessConfig {
        ProcessConfig {
            network_namespace: "container-test".to_owned(),
            rootfs: "/rootfs".into(),
            overlay: None,
            owners: Vec::new(),
            filesystem_generation: "/generation".into(),
            checkpoint_directory: None,
            restore_directory: None,
            guest: crate::Guest::Aarch64,
            process: crate::Process::new("/bin/true"),
            hostname: None,
            mounts: Vec::new(),
            resources: crate::Resources::default(),
            isolation: crate::Isolation {
                network_isolated: false,
                ..Default::default()
            },
            network_mode: crate::NetworkMode::Automatic,
            networks: Vec::new(),
            publish: Vec::new(),
            input: None,
            terminal: None,
            domain: None,
            domain_owner: true,
            extensions: Vec::new(),
            authorities: Vec::new(),
        }
    }

    #[test]
    fn automatic_networking_uses_a_private_namespace_without_endpoints() {
        let spec = hl_engine::MachineSpec::from(Spec::try_from(&launch()).unwrap());

        assert_eq!(spec.network.mode, hl_engine::spec::NetworkMode::Virtual);
        assert_eq!(
            spec.network
                .namespace
                .as_ref()
                .map(hl_engine::network::Namespace::as_str),
            Some("container-test")
        );
    }

    #[test]
    fn published_ports_use_the_private_namespace_without_an_explicit_bridge() {
        let mut launch = launch();
        launch
            .publish
            .push(crate::Publication::tcp(std::net::Ipv4Addr::LOCALHOST, 8_080, 80).unwrap());

        let spec = hl_engine::MachineSpec::from(Spec::try_from(&launch).unwrap());

        assert_eq!(spec.network.mode, hl_engine::spec::NetworkMode::Virtual);
        assert_eq!(
            spec.network
                .namespace
                .as_ref()
                .map(hl_engine::network::Namespace::as_str),
            Some("container-test")
        );
        assert_eq!(spec.network.port_forwards.len(), 1);
    }

    #[test]
    fn maps_checkpoint_capture_and_restore_directories() {
        let mut capture = launch();
        capture.checkpoint_directory = Some("/checkpoints/capture".into());
        let capture = hl_engine::MachineSpec::from(Spec::try_from(&capture).unwrap());

        assert!(capture.checkpoint.enabled);
        assert_eq!(
            capture.checkpoint.capture_directory.as_deref(),
            Some(std::path::Path::new("/checkpoints/capture"))
        );
        assert!(capture.checkpoint.restore_directory.is_none());

        let mut restore = launch();
        restore.restore_directory = Some("/checkpoints/restore".into());
        let restore = hl_engine::MachineSpec::from(Spec::try_from(&restore).unwrap());

        assert!(restore.checkpoint.enabled);
        assert_eq!(
            restore.checkpoint.restore_directory.as_deref(),
            Some(std::path::Path::new("/checkpoints/restore"))
        );
        assert!(restore.checkpoint.capture_directory.is_none());
    }

    #[test]
    fn maps_process_filesystem_identity_and_resource_contracts() {
        let temporary = tempfile::tempdir().unwrap();
        let rootfs = temporary.path().join("rootfs");
        let generation = temporary.path().join("generation");
        std::fs::create_dir(&rootfs).unwrap();
        std::fs::write(&generation, b"0").unwrap();
        let mut launch = launch();
        launch.rootfs = rootfs.clone();
        launch.filesystem_generation = generation;
        launch.process = crate::Process::new("/bin/tool")
            .args(["first", "second"])
            .env("MODE", "test")
            .working_dir("/workspace")
            .user(1000, 1001);
        launch.resources = crate::Resources {
            memory_bytes: 64 * 1024 * 1024,
            process_count: 32,
            cpu_count: 2,
        };
        launch.isolation.read_only_root = true;

        let spec = hl_engine::MachineSpec::from(Spec::try_from(&launch).unwrap());

        assert_eq!(spec.process.argv.len(), 3);
        assert_eq!(spec.identity.uid, Some(1000));
        assert_eq!(spec.identity.gid, Some(1001));
        assert!(spec.filesystem.read_only);
        assert!(matches!(
            spec.filesystem.root,
            Some(hl_engine::spec::TreeSource::HostDirectory(path)) if path == rootfs
        ));
        assert_eq!(spec.resources.memory_bytes, Some(64 * 1024 * 1024));
        assert_eq!(spec.resources.process_limit, Some(32));
        assert_eq!(spec.resources.cpu_limit, Some(2));
    }
}
