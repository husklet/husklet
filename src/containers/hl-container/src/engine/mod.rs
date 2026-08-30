use crate::{
    Error, Result,
    service::{OverlayConfig, ProcessConfig, Running, Runtime},
};
use async_trait::async_trait;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex as StdMutex},
};

const CHECKPOINT_OBJECT: &str = "rust/image";
const CHECKPOINT_MANIFEST_MAGIC: &[u8; 8] = b"HLRUST01";

mod checkpoint;
mod member;
mod process;
mod spec;
mod stream;
use checkpoint::CheckpointTransport;
use member::MemberSession;
use process::Process;
use spec::Spec;
use stream::{OutputChannel, TerminalChannel};

/// Every domain freeze channel this runtime currently coordinates, keyed by the
/// process domain identity the guest processes are launched into.
type DomainChannels = StdMutex<HashMap<[u64; 2], hl_engine::composition::CheckpointChannel>>;

#[derive(Default)]
pub(crate) struct Engine {
    domains: Arc<DomainChannels>,
}

/// Keeps a coordinator's freeze channel published for its domain's exec sessions
/// and withdraws it when the coordinator process is released, so a session started
/// after the coordinator is gone cannot join a dead broker.
pub(super) struct DomainChannelEntry {
    domains: Arc<DomainChannels>,
    identity: [u64; 2],
}

impl Drop for DomainChannelEntry {
    fn drop(&mut self) {
        if let Ok(mut domains) = self.domains.lock() {
            domains.remove(&self.identity);
        }
    }
}

#[async_trait]
impl Runtime for Engine {
    fn validate_overlay(&self, overlay: &OverlayConfig) -> bool {
        overlay.lower.is_dir() && overlay.upper.is_dir() && overlay.work.is_dir()
    }

    async fn start(&self, mut config: ProcessConfig) -> Result<Arc<dyn Running>> {
        if !config.rootfs.is_dir() {
            return Err(Error::InvalidSpec(format!(
                "rootfs does not exist or is not a directory: {}",
                config.rootfs.display()
            )));
        }
        let spec = Spec::try_from(&config)?;
        let (sender, receiver) = crate::service::log_channel();
        let streams = match config.terminal {
            Some(size) => {
                let port = Arc::new(TerminalChannel::new(config.input.take(), sender));
                let terminal = hl_engine::composition::Terminal::new(port, size.rows(), size.columns())
                    .map_err(|_| Error::Runtime("terminal construction failed".into()))?;
                hl_engine::composition::StandardStreams::default().with_terminal(terminal)
            }
            None => hl_engine::composition::StandardStreams::default()
                .with_output(Arc::new(OutputChannel::new(config.input.take(), sender))),
        };
        let identity = spec.domain.identity();
        let role = config.checkpoint.take();
        // A member joins the coordinator's broker and trigger. Resolving the channel
        // before construction keeps the refusal on the launch boundary: a session that
        // cannot reach its domain's freeze must not start armed on a channel of its own.
        let member = match &role {
            Some(crate::service::CheckpointRole::DomainMember) => Some(
                self.domains
                    .lock()
                    .map_err(|_| Error::Runtime("checkpoint domain registry is poisoned".into()))?
                    .get(&identity)
                    .cloned()
                    .ok_or_else(|| Error::Runtime("process domain has no checkpoint coordinator to join".into()))?,
            ),
            _ => None,
        };
        let checkpoint = match role {
            Some(crate::service::CheckpointRole::Coordinator(checkpoint)) => {
                Some(Arc::new(CheckpointTransport::new(checkpoint.image)))
            }
            Some(crate::service::CheckpointRole::DomainMember) | None => None,
        };
        let engine = Arc::new(
            match (checkpoint, member) {
                (Some(transport), _) => hl_engine::runtime::Engine::with_checkpoint(
                    spec.isa,
                    spec.plan,
                    streams,
                    transport.clone(),
                    transport,
                ),
                (None, Some(channel)) => {
                    hl_engine::runtime::Engine::with_checkpoint_channel(spec.isa, spec.plan, streams, channel)
                }
                (None, None) => hl_engine::runtime::Engine::with_streams(spec.isa, spec.plan, streams),
            }
            .map_err(|error| Error::Runtime(format!("engine construction: {error:?}")))?,
        );
        let domain_channel = engine.checkpoint_channel().map(|channel| {
            if let Ok(mut domains) = self.domains.lock() {
                domains.insert(identity, channel);
            }
            DomainChannelEntry {
                domains: Arc::clone(&self.domains),
                identity,
            }
        });
        // Before `start`, because start IS the restore: a member asks for its terminal from inside its own
        // descriptor restore, so a registration made afterwards answers nothing. Registering by value is
        // deliberate -- the slave descriptor is moved into the engine here, so a launch that dropped this
        // step would not compile rather than quietly restore a tree with no per-member I/O.
        let members = config
            .member_terminals
            .drain(..)
            .map(|member| MemberSession::open(&engine, member).map(Arc::new))
            .collect::<Result<Vec<_>>>()?;
        engine
            .start()
            .map_err(|error| Error::Runtime(format!("engine start: {error:?}")))?;

        Ok(Arc::new(Process {
            id: Process::next_id(),
            child: StdMutex::new(Some(engine)),
            logs: StdMutex::new(Some(receiver)),
            domain: spec.domain,
            _domain_channel: domain_channel,
            members,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::{CheckpointTransport, Spec};
    use crate::CheckpointImage as _;
    use crate::service::{NetworkConfig, ProcessConfig};
    use hl_engine::composition::{CheckpointSink as _, CheckpointSource as _};
    use std::collections::BTreeMap;
    use std::num::NonZeroU64;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Image(Mutex<BTreeMap<String, Vec<u8>>>);

    impl crate::CheckpointImage for Image {
        fn begin_until(&self, _: std::time::Instant) -> Result<NonZeroU64, crate::CheckpointError> {
            Ok(NonZeroU64::MIN)
        }

        fn put_until(
            &self,
            _: NonZeroU64,
            name: &str,
            bytes: &[u8],
            deadline: std::time::Instant,
        ) -> Result<(), crate::CheckpointError> {
            (std::time::Instant::now() < deadline)
                .then_some(())
                .ok_or_else(|| crate::CheckpointError::new("deadline exceeded"))?;
            self.0.lock().unwrap().insert(name.to_owned(), bytes.to_vec());
            Ok(())
        }

        fn get(&self, name: &str) -> Result<Vec<u8>, crate::CheckpointError> {
            self.0
                .lock()
                .unwrap()
                .get(name)
                .cloned()
                .ok_or_else(|| crate::CheckpointError::new("missing object"))
        }

        fn list(&self) -> Result<Vec<String>, crate::CheckpointError> {
            Ok(self.0.lock().unwrap().keys().cloned().collect())
        }

        fn abort_until(&self, _: NonZeroU64, _: std::time::Instant) -> Result<(), crate::CheckpointError> {
            Ok(())
        }

        fn get_until(&self, name: &str, deadline: std::time::Instant) -> Result<Vec<u8>, crate::CheckpointError> {
            (std::time::Instant::now() < deadline)
                .then_some(())
                .ok_or_else(|| crate::CheckpointError::new("deadline exceeded"))?;
            self.get(name)
        }

        fn list_until(&self, deadline: std::time::Instant) -> Result<Vec<String>, crate::CheckpointError> {
            (std::time::Instant::now() < deadline)
                .then_some(())
                .ok_or_else(|| crate::CheckpointError::new("deadline exceeded"))?;
            self.list()
        }

        fn commit_until(
            &self,
            transaction: NonZeroU64,
            manifest: &[u8],
            deadline: std::time::Instant,
        ) -> Result<(), crate::CheckpointError> {
            (std::time::Instant::now() < deadline)
                .then_some(())
                .ok_or_else(|| crate::CheckpointError::new("deadline exceeded"))?;
            self.put_until(transaction, "MANIFEST", manifest, deadline)
        }
    }

    #[test]
    fn checkpoint_transport_publishes_then_reads_exact_image() {
        let image = Arc::new(Image::default());
        let transport = CheckpointTransport::new(image.clone());
        transport.replace(b"rust-checkpoint").unwrap();
        assert_eq!(transport.read(64).unwrap(), b"rust-checkpoint");
        assert_eq!(image.list().unwrap(), ["MANIFEST", "rust/image"]);
        assert!(transport.read(4).is_err());
    }

    #[test]
    fn checkpoint_transport_rejects_uncommitted_or_torn_images() {
        let image = Arc::new(Image::default());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let transaction = image.begin_until(deadline).unwrap();
        image
            .put_until(transaction, "rust/image", b"partial", deadline)
            .unwrap();
        let transport = CheckpointTransport::new(image.clone());
        assert!(transport.read(64).is_err());
        image
            .put_until(transaction, "MANIFEST", b"not-a-rust-manifest", deadline)
            .unwrap();
        assert!(transport.read(64).is_err());
    }

    struct BusyImage;

    impl crate::CheckpointImage for BusyImage {
        fn begin_until(&self, _: std::time::Instant) -> Result<NonZeroU64, crate::CheckpointError> {
            Err(crate::CheckpointError::busy())
        }
        fn put_until(
            &self,
            _: NonZeroU64,
            _: &str,
            _: &[u8],
            _: std::time::Instant,
        ) -> Result<(), crate::CheckpointError> {
            unreachable!()
        }
        fn abort_until(&self, _: NonZeroU64, _: std::time::Instant) -> Result<(), crate::CheckpointError> {
            unreachable!()
        }
        fn get(&self, _: &str) -> Result<Vec<u8>, crate::CheckpointError> {
            unreachable!()
        }
        fn get_until(&self, _: &str, _: std::time::Instant) -> Result<Vec<u8>, crate::CheckpointError> {
            unreachable!()
        }
        fn list(&self) -> Result<Vec<String>, crate::CheckpointError> {
            unreachable!()
        }
        fn list_until(&self, _: std::time::Instant) -> Result<Vec<String>, crate::CheckpointError> {
            unreachable!()
        }
        fn commit_until(&self, _: NonZeroU64, _: &[u8], _: std::time::Instant) -> Result<(), crate::CheckpointError> {
            unreachable!()
        }
    }

    #[test]
    fn checkpoint_transport_preserves_transaction_busy() {
        let transport = CheckpointTransport::new(Arc::new(BusyImage));
        assert_eq!(
            transport.begin_until(std::time::Instant::now() + std::time::Duration::from_secs(1)),
            Err(hl_engine::composition::CompositionError::TransactionBusy)
        );
    }

    fn launch() -> ProcessConfig {
        ProcessConfig {
            member_terminals: Vec::new(),
            network_namespace: "container-test".to_owned(),
            rootfs: "/rootfs".into(),
            overlay: None,
            executable_digest_authority: None,
            owners: Vec::new(),
            filesystem_generation: "/generation".into(),
            translation_cache: None,
            translation_cache_observability: false,
            translation_symbols: None,
            checkpoint: None,
            guest: crate::Guest::Aarch64,
            execution: crate::Execution::default(),
            process: crate::Process::new("/bin/true"),
            hostname: None,
            mounts: Vec::new(),
            resources: crate::Resources::default(),
            isolation: crate::Isolation::default(),
            network_mode: crate::NetworkMode::Automatic,
            networks: Vec::new(),
            publish: Vec::new(),
            input: None,
            terminal: None,
            domain: None,
            domain_owner: true,
        }
    }

    fn bridge(name: &str, address: &str, prefix: u8) -> NetworkConfig {
        NetworkConfig {
            namespace: "container-test".to_owned(),
            bridge: Some(name.to_owned()),
            address: Some(address.parse().unwrap()),
            prefix: Some(prefix),
            gateway: Some("172.28.0.1".parse().unwrap()),
            name: name.to_owned(),
            driver: crate::NetworkDriver::Bridge,
            endpoints: Vec::new(),
        }
    }

    #[test]
    fn resolved_container_plan_uses_the_production_engine() {
        let launch = launch();
        let spec = Spec::try_from(&launch).unwrap();
        assert_eq!(spec.plan.rootfs.as_deref(), Some(b"/rootfs".as_slice()));
        assert_eq!(spec.plan.arguments[0], b"/bin/true");
        assert_eq!(spec.plan.options.get("HL_NETNS"), Some("container-test"));
    }

    #[test]
    fn checkpoint_transport_arms_capture_and_requested_restore() {
        for restore in [false, true] {
            let mut launch = launch();
            launch.checkpoint = Some(crate::service::CheckpointRole::Coordinator(
                crate::service::CheckpointConfig {
                    image: Arc::new(Image::default()),
                    restore,
                },
            ));
            let spec = Spec::try_from(&launch).unwrap();
            assert_eq!(spec.plan.options.get("HL_CHECKPOINT"), Some("1"));
            assert_eq!(spec.plan.options.get("HL_RESTORE"), restore.then_some("1"));
        }
    }

    #[test]
    fn file_mounts_use_the_volume_protocol() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("guest-program");
        std::fs::write(&source, b"program").unwrap();
        let mut launch = launch();
        launch.mounts.push(crate::model::ResolvedMount {
            source: source.clone(),
            target: "/bin/guest-program".into(),
            access: crate::Access::ReadOnly,
        });

        let spec = Spec::try_from(&launch).unwrap();

        assert_eq!(
            spec.plan.options.get("HL_VOLUMES"),
            Some(format!("ro:/bin/guest-program:{}", source.display()).as_str())
        );
        assert_eq!(spec.plan.options.get("HL_NAME_BINDS"), None);
    }

    #[test]
    fn native_execution_selects_only_the_retained_c_diagnostics_option() {
        // The engine has no native-execution switch: `src/runtime/hl-native/src/native/engine/options.c`
        // is the authoritative option registry and defines neither `HL_NATIVE_EXECUTION` nor
        // `HL_NATIVE_DIAGNOSTICS`. `Execution::Native` therefore carries exactly one launch effect,
        // the retained-C diagnostics request.
        let mut launch = launch();
        launch.execution = crate::Execution::native(true);
        let spec = Spec::try_from(&launch).unwrap();
        assert_eq!(spec.plan.options.get("HL_NATIVE_EXECUTION"), None);
        assert_eq!(spec.plan.options.get("HL_NATIVE_DIAGNOSTICS"), None);
        assert_eq!(spec.plan.options.get("HL_C_DIAGNOSTICS"), Some("1"));
    }

    #[test]
    fn native_execution_without_diagnostics_selects_no_launch_option() {
        let mut launch = launch();
        launch.execution = crate::Execution::native(false);
        let spec = Spec::try_from(&launch).unwrap();
        assert_eq!(spec.plan.options.get("HL_NATIVE_EXECUTION"), None);
        assert_eq!(spec.plan.options.get("HL_NATIVE_DIAGNOSTICS"), None);
        assert_eq!(spec.plan.options.get("HL_C_DIAGNOSTICS"), None);
    }

    #[test]
    fn every_execution_mode_leaves_product_backend_unselected() {
        for execution in [crate::Execution::default(), crate::Execution::native(false)] {
            let mut launch = launch();
            launch.execution = execution;
            let spec = Spec::try_from(&launch).unwrap();
            assert_eq!(spec.plan.options.get("HL_EXECUTION_BACKEND"), None);
        }
    }

    #[test]
    fn filesystem_generation_reaches_the_typed_engine_policy() {
        use std::os::unix::ffi::OsStrExt as _;

        let launch = launch();
        let spec = Spec::try_from(&launch).unwrap();
        assert_eq!(
            spec.plan.box_policy.filesystem_generation.as_deref(),
            Some(launch.filesystem_generation.as_os_str().as_bytes())
        );
        assert_eq!(spec.plan.options.get("HL_FSGEN_FILE"), Some("/generation"));
    }

    #[test]
    fn translation_cache_reaches_options_and_typed_engine_policy() {
        let mut launch = launch();
        launch.translation_cache = Some("/translation-cache".into());
        let spec = Spec::try_from(&launch).unwrap();
        assert_eq!(
            spec.plan.box_policy.translation_cache.as_deref(),
            Some(b"/translation-cache".as_slice())
        );
        assert_eq!(spec.plan.options.get("HL_PCACHE"), Some("1"));
        assert_eq!(spec.plan.options.get("HL_PCACHE_DIR"), Some("/translation-cache"));
    }

    #[test]
    fn snapshot_executable_digest_reaches_only_internal_typed_policy() {
        let root = tempfile::tempdir().unwrap();
        let snapshots = hl_images::snapshot::Snapshots::open(root.path().join("snapshots")).unwrap();
        let lower_id = hl_images::snapshot::Id::new("chain-test").unwrap();
        let draft = snapshots.prepare(lower_id.clone(), None).unwrap();
        std::fs::create_dir(draft.path().join("bin")).unwrap();
        std::fs::copy(std::env::current_exe().unwrap(), draft.path().join("bin/tool")).unwrap();
        draft.commit(lower_id.clone()).unwrap();
        let roots =
            hl_images::rootfs::Roots::new(snapshots, hl_images::Leases::open(root.path().join("leases")).unwrap());
        let authority = roots.executable_digest_authority(&lower_id);
        let mut launch = launch();
        launch.guest = crate::Guest::X86_64;
        launch.process = crate::Process::new("/bin/tool");
        launch.rootfs = root.path().join("upper");
        std::fs::create_dir(&launch.rootfs).unwrap();
        launch.overlay = Some(crate::service::OverlayConfig {
            lower: root.path().join("snapshots/committed/chain-test"),
            upper: launch.rootfs.clone(),
            work: root.path().join("work"),
        });
        launch.executable_digest_authority = Some(authority);

        let spec = Spec::try_from(&launch).unwrap();
        assert!(!spec.plan.box_policy.executable_digests.is_empty());
        assert_eq!(spec.plan.box_policy.executable_digests[0].guest_path, b"/bin/tool");
        assert!(spec.plan.options.get("HL_PCACHE_EXEC_AUTHORITY").is_some());
    }

    #[test]
    fn overlay_and_ownership_reach_the_typed_engine_policy() {
        let mut launch = launch();
        launch.overlay = Some(crate::service::OverlayConfig {
            lower: "/lower".into(),
            upper: "/upper".into(),
            work: "/work".into(),
        });
        launch.owners = vec![("bin/tool".into(), 123, 456)];
        let spec = Spec::try_from(&launch).unwrap();
        assert_eq!(spec.plan.box_policy.lower_layers.as_deref(), Some(b"/lower".as_slice()));
        assert_eq!(
            spec.plan.box_policy.file_owners.as_deref(),
            Some(b"bin/tool\t123\t456".as_slice())
        );
    }

    #[test]
    fn network_interfaces_preserve_attachment_order_and_prefixes() {
        let mut launch = launch();
        launch.networks = vec![bridge("front", "172.29.0.2", 24), bridge("back", "10.7.0.9", 19)];

        let spec = Spec::try_from(&launch).unwrap();

        assert_eq!(
            spec.plan.options.get("HL_NETIFS"),
            Some("front=172.29.0.2/24\nback=10.7.0.9/19")
        );
        assert_eq!(spec.plan.options.get("HL_NETBR"), None);
        assert_eq!(spec.plan.options.get("HL_IP"), None);
        assert_eq!(spec.plan.box_policy.network_mode, 0);
        assert_eq!(
            spec.plan.box_policy.network_namespace.as_deref(),
            Some(b"container-test".as_slice())
        );
        assert_eq!(spec.plan.box_policy.network_interfaces.len(), 2);
        assert_eq!(spec.plan.box_policy.network_interfaces[0].bridge, b"front");
        assert_eq!(
            spec.plan.box_policy.network_interfaces[0].address_ipv4_be,
            u32::from_le_bytes([172, 29, 0, 2])
        );
        assert_eq!(
            spec.plan.box_policy.network_interfaces[0].gateway_ipv4_be,
            u32::from_le_bytes([172, 28, 0, 1])
        );
        assert_eq!(spec.plan.box_policy.network_interfaces[1].bridge, b"back");
    }

    #[test]
    fn network_interfaces_are_bounded() {
        let mut launch = launch();
        launch.networks = (0..9)
            .map(|index| bridge(&format!("net{index}"), &format!("10.0.0.{}", index + 1), 24))
            .collect();

        let error = Spec::try_from(&launch).err().unwrap();
        assert!(error.to_string().contains("at most 8 virtual network interfaces"));
    }

    #[test]
    fn network_interface_fields_are_validated() {
        let mut launch = launch();
        launch.networks = vec![bridge("bad=bridge", "10.0.0.2", 24)];
        assert!(Spec::try_from(&launch).is_err());

        launch.networks = vec![bridge(&"x".repeat(41), "10.0.0.2", 24)];
        assert!(Spec::try_from(&launch).is_err());

        launch.networks = vec![bridge("valid", "10.0.0.2", 33)];
        assert!(Spec::try_from(&launch).is_err());

        launch.networks = vec![bridge("valid", "0.0.0.0", 24)];
        assert!(Spec::try_from(&launch).is_err());

        let mut incomplete = bridge("valid", "10.0.0.2", 24);
        incomplete.prefix = None;
        launch.networks = vec![incomplete];
        assert!(Spec::try_from(&launch).is_err());
        let mut missing_gateway = bridge("valid", "10.0.0.2", 24);
        missing_gateway.gateway = None;
        launch.networks = vec![missing_gateway];
        assert!(Spec::try_from(&launch).is_err());
    }

    #[test]
    fn host_network_and_publications_reach_the_typed_policy_without_legacy_parsing() {
        let mut launch = launch();
        launch.network_mode = crate::NetworkMode::Host;
        launch.publish = vec![crate::Publication::tcp("127.0.0.1".parse().unwrap(), 18080, 8080).unwrap()];
        let spec = Spec::try_from(&launch).unwrap();
        assert_eq!(spec.plan.box_policy.network_mode, 2);
        assert_eq!(spec.plan.box_policy.network_namespace, None);
        assert_eq!(spec.plan.box_policy.publish.len(), 1);
        assert_eq!(
            spec.plan.box_policy.publish[0].host_ipv4_be,
            u32::from_le_bytes([127, 0, 0, 1])
        );
        assert_eq!(
            (
                spec.plan.box_policy.publish[0].host_port,
                spec.plan.box_policy.publish[0].guest_port
            ),
            (18080, 8080)
        );
    }
}
