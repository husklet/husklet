use crate::{
    service::{OverlayConfig, ProcessConfig, Running, Runtime},
    Error, Result, Stream,
};
use async_trait::async_trait;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::Mutex;

mod process;
mod spec;
mod stream;
use process::Process;
use spec::Spec;

#[derive(Default)]
pub(crate) struct Engine;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CheckpointLaunch {
    Ordinary,
    Store,
}

impl Engine {
    fn checkpoint_launch(
        engine: hl_engine::Engine,
        guest: crate::Guest,
        restore: Option<bool>,
    ) -> CheckpointLaunch {
        let Some(restore) = restore else {
            return CheckpointLaunch::Ordinary;
        };
        let guest = match guest {
            crate::Guest::Aarch64 => hl_engine::Guest::Aarch64,
            crate::Guest::X86_64 => hl_engine::Guest::X86_64,
        };
        if engine.capabilities().checkpoint.supports(guest) || restore {
            CheckpointLaunch::Store
        } else {
            // Every ordinary container gets a best-effort capture store. It must
            // not turn an otherwise supported guest into an invalid launch.
            CheckpointLaunch::Ordinary
        }
    }

    fn io(launch: &ProcessConfig) -> hl_engine::ProcessIo {
        hl_engine::ProcessIo {
            stdin: if launch.input.is_some() {
                hl_engine::Stdio::piped()
            } else {
                hl_engine::Stdio::null()
            },
            stdout: hl_engine::Stdio::piped(),
            stderr: hl_engine::Stdio::piped(),
        }
    }

    fn authorities(grants: Vec<crate::Authority>) -> Result<hl_engine::Authorities> {
        let mut authorities = hl_engine::Authorities::new();
        for grant in grants {
            authorities
                .grant(
                    grant.provider,
                    hl_engine::ProviderAuthority {
                        handles: grant.handles,
                        memory: grant.memory,
                    },
                )
                .map_err(|error| Error::InvalidSpec(format!("device authority: {error:?}")))?;
        }
        Ok(authorities)
    }
}

#[async_trait]
impl Runtime for Engine {
    fn validate_overlay(&self, overlay: &OverlayConfig) -> bool {
        use hl_engine::spec::{FilesystemFeature, TreeSource};

        let engine = hl_engine::Engine::new();
        if !engine
            .capabilities()
            .filesystems
            .features
            .contains(&FilesystemFeature::Overlay)
        {
            return false;
        }
        let mut spec = hl_engine::MachineSpec::new(hl_engine::Guest::Aarch64, "/bin/true");
        spec.filesystem.root = Some(TreeSource::Overlay {
            lower: vec![TreeSource::HostDirectory(overlay.lower.clone())],
            upper: overlay.upper.clone(),
            work: overlay.work.clone(),
        });
        engine.validate(&spec).is_ok()
    }

    async fn start(&self, config: ProcessConfig) -> Result<Arc<dyn Running>> {
        if !config.rootfs.is_dir() {
            return Err(Error::InvalidSpec(format!(
                "rootfs does not exist or is not a directory: {}",
                config.rootfs.display()
            )));
        }
        let engine = hl_engine::Engine::new();
        let checkpoint = config.checkpoint.clone();
        let checkpoint_launch = Self::checkpoint_launch(
            engine,
            config.guest,
            checkpoint.as_ref().map(|checkpoint| checkpoint.restore),
        );
        let checkpoint = (checkpoint_launch == CheckpointLaunch::Store)
            .then_some(checkpoint)
            .flatten();
        let checkpointable = checkpoint.is_some();
        let spec = hl_engine::MachineSpec::from(Spec::try_from(&config)?);
        let io = Self::io(&config);
        let authorities = Self::authorities(config.authorities.clone())?;
        let started = match checkpoint {
            Some(checkpoint) => engine.spawn_with_store_and_authorities(
                spec,
                io,
                Arc::new(crate::checkpoint::EngineImage::new(checkpoint.image)),
                if checkpoint.restore {
                    hl_engine::StoreDirection::Both
                } else {
                    hl_engine::StoreDirection::Capture
                },
                authorities,
            ),
            None => engine.spawn_with_authorities(spec, io, authorities),
        };
        let mut child = started.map_err(|error| Error::Runtime(error.to_string()))?;
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let terminal = if let Some(terminal) = child.take_terminal() {
            let reader = terminal
                .try_clone()
                .map_err(|error| Error::Runtime(error.to_string()))?;
            let terminal = Arc::new(StdMutex::new(terminal));
            Self::reader(reader, Stream::Stdout, sender.clone());
            if let Some(input) = config.input {
                Self::terminal_writer(Arc::clone(&terminal), input);
            }
            Some(terminal)
        } else {
            if let (Some(file), Some(input)) = (child.take_stdin(), config.input) {
                Self::writer(file, input);
            }
            if let Some(file) = child.take_stdout() {
                Self::reader(file, Stream::Stdout, sender.clone());
            }
            if let Some(file) = child.take_stderr() {
                Self::reader(file, Stream::Stderr, sender.clone());
            }
            None
        };
        drop(sender);
        let domain = child.domain();
        Ok(Arc::new(Process {
            id: child.id(),
            child: Mutex::new(Some(child)),
            logs: StdMutex::new(Some(receiver)),
            terminal,
            domain,
            domain_owner: config.domain_owner,
            checkpointable,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::{Engine, Process};
    use crate::service::ProcessConfig;
    use hl_engine::extension::{
        DeviceEntry, DeviceKind, ExtensionConfig, ExtensionSpec, Feature, Inheritance,
        MemoryRequirement, Metadata, NamespaceEntry, Protections, ProviderId, ServiceEntry,
        ServiceId, ServiceRegistration, Sharing, SocketEntry,
    };
    use std::{collections::BTreeSet, sync::Arc};

    struct Handles;

    impl hl_engine::extension::Handles for Handles {
        fn open(
            &self,
            _request: hl_engine::extension::OpenRequest,
        ) -> std::result::Result<
            Box<dyn hl_engine::extension::OpenHandle>,
            hl_engine::extension::LinuxError,
        > {
            Err(hl_engine::extension::LinuxError {
                errno: 95,
                context: "compile-only provider".into(),
            })
        }
    }

    struct Memory;

    impl hl_engine::extension::Memory for Memory {
        fn allocate(
            &self,
            _request: hl_engine::extension::AllocationRequest,
        ) -> std::result::Result<
            hl_engine::extension::HostResource,
            hl_engine::extension::ResourceError,
        > {
            Err(resource_error())
        }

        fn import(
            &self,
            _descriptor: &hl_engine::extension::ResourceDescriptor,
        ) -> std::result::Result<
            hl_engine::extension::HostResource,
            hl_engine::extension::ResourceError,
        > {
            Err(resource_error())
        }
    }

    fn resource_error() -> hl_engine::extension::ResourceError {
        hl_engine::extension::ResourceError {
            category: hl_engine::extension::ResourceErrorCategory::Unsupported,
            context: "compile-only provider".into(),
        }
    }

    fn launch() -> ProcessConfig {
        ProcessConfig {
            network_namespace: "container-test".to_owned(),
            rootfs: "/rootfs".into(),
            overlay: None,
            owners: Vec::new(),
            filesystem_generation: "/generation".into(),
            translation_cache: None,
            checkpoint: None,
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
    fn optional_checkpointing_follows_each_guest_capability() {
        let engine = hl_engine::Engine::new();
        for (guest, engine_guest) in [
            (crate::Guest::Aarch64, hl_engine::Guest::Aarch64),
            (crate::Guest::X86_64, hl_engine::Guest::X86_64),
        ] {
            let expected = if engine.capabilities().checkpoint.supports(engine_guest) {
                super::CheckpointLaunch::Store
            } else {
                super::CheckpointLaunch::Ordinary
            };
            assert_eq!(
                Engine::checkpoint_launch(engine, guest, Some(false)),
                expected
            );
        }
        assert_eq!(
            Engine::checkpoint_launch(engine, crate::Guest::X86_64, Some(true)),
            super::CheckpointLaunch::Store,
            "an explicit restore must reach engine validation instead of being dropped"
        );
        assert_eq!(
            Engine::checkpoint_launch(engine, crate::Guest::Aarch64, None),
            super::CheckpointLaunch::Ordinary
        );
    }

    #[test]
    fn signaling_an_already_reaped_process_is_idempotent() {
        let mut child = std::process::Command::new("/usr/bin/true")
            .spawn()
            .expect("spawn short-lived process");
        let id = u64::from(child.id());
        child.wait().expect("reap short-lived process");

        Process::send_id(id, nix::sys::signal::Signal::SIGTERM)
            .expect("an already-gone process satisfies termination");
    }

    #[test]
    fn combined_provider_capabilities_cross_the_container_facade() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("provider.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let generation = directory.path().join("generation");
        std::fs::write(&generation, b"0").unwrap();
        let provider = ProviderId::new("engine.handles").unwrap();
        let service = ServiceId(77);
        let namespace = ExtensionSpec {
            provider: ProviderId::new("engine.namespace").unwrap(),
            version: hl_engine::spec::Version::new(1, 0),
            required: true,
            required_features: BTreeSet::from([Feature::new("unix-sockets").unwrap()]),
            optional_features: BTreeSet::new(),
            config: ExtensionConfig::empty("engine.namespace/v1"),
            namespace: vec![NamespaceEntry::Socket(SocketEntry {
                path: "/run/provider.sock".into(),
                host: socket,
            })],
            rules: Vec::new(),
            services: Vec::new(),
            memory: Vec::new(),
            environment: Vec::new(),
        };
        let extension = ExtensionSpec {
            provider: provider.clone(),
            version: hl_engine::spec::Version::new(1, 0),
            required: true,
            required_features: BTreeSet::from([
                Feature::new("devices").unwrap(),
                Feature::new("memory-allocation").unwrap(),
                Feature::new("read").unwrap(),
            ]),
            optional_features: BTreeSet::new(),
            config: ExtensionConfig::empty("engine.handles/v1"),
            namespace: vec![
                NamespaceEntry::Service(ServiceEntry {
                    path: "/run/provider/service".into(),
                    metadata: Metadata {
                        mode: 0o660,
                        uid: 0,
                        gid: 0,
                    },
                    service,
                }),
                NamespaceEntry::Device(DeviceEntry {
                    path: "/dev/provider".into(),
                    metadata: Metadata {
                        mode: 0o660,
                        uid: 0,
                        gid: 0,
                    },
                    kind: DeviceKind::Character,
                    major: 226,
                    minor: 128,
                    service: Some(service),
                }),
            ],
            rules: Vec::new(),
            services: vec![ServiceRegistration {
                id: service,
                operations: BTreeSet::from([hl_engine::extension::HandleOperation::Read]),
                max_request_bytes: 4096,
            }],
            memory: vec![MemoryRequirement {
                size: 4096,
                alignment: 4096,
                protections: Protections {
                    read: true,
                    write: true,
                    execute: false,
                },
                sharing: Sharing::Shared,
                inheritance: Inheritance::Retain,
            }],
            environment: Vec::new(),
        };
        let request = crate::DeviceRequest {
            extensions: vec![namespace, extension],
            authorities: vec![crate::Authority::new(provider.clone())
                .handles(Arc::new(Handles))
                .memory(Arc::new(Memory))],
            ..Default::default()
        };
        let mut launch = launch();
        launch.rootfs = directory.path().to_owned();
        launch.filesystem_generation = generation;
        launch.terminal = Some(crate::Size::new(24, 80).unwrap());
        launch.extensions = request.extensions;
        launch.authorities = request.authorities;

        let spec = hl_engine::MachineSpec::from(super::Spec::try_from(&launch).unwrap());
        hl_engine::Engine::new().validate(&spec).unwrap();
        let authorities = Engine::authorities(launch.authorities).unwrap();
        let granted = authorities.provider(&provider).unwrap();
        assert!(granted.handles.is_some());
        assert!(granted.memory.is_some());
        assert!(hl_engine::Engine::new()
            .capabilities()
            .control
            .operations
            .contains(&hl_engine::spec::ControlOperation::Signal));
    }
}
