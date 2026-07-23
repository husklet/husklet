pub(super) use super::super::builder::build_with;
pub(super) use super::super::*;
pub(super) use crate::{
    error::Result,
    service::Runtime,
    storage::{Disk, Memory},
    Container, ContainerSpec, ExitStatus, Signal,
};

#[derive(Default)]
pub(super) struct Recorded(pub(super) std::sync::Mutex<Vec<LifecycleAction>>);

impl LifecycleEvents for Recorded {
    fn emit(&self, event: LifecycleEvent) {
        self.0.lock().unwrap().push(event.action);
    }
}

pub(super) use crate::{
    service::{NetworkConfig, ProcessConfig, Running},
    storage::Containers as _,
    Access, ContainerState, Error, Guest, Isolation, Process,
};
use async_trait::async_trait;
pub(super) use std::{
    collections::BTreeSet,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

pub(super) fn stored_container(id: &str, name: &str) -> Container {
    Container {
        id: id.parse().unwrap(),
        spec: spec(name),
        state: ContainerState::Created,
        created_at_ms: 1,
        generation: 0,
        restart: crate::Restart::default(),
        health: None,
        checkpoint: None,
    }
}

pub(super) async fn resolving_store(values: &[(&str, &str)]) -> Containers {
    let storage = Arc::new(Memory::default());
    for (id, name) in values {
        storage.insert(&stored_container(id, name)).await.unwrap();
    }
    test_containers(storage, Arc::new(FakeRuntime::new(ExitStatus::Code(0))))
        .await
        .unwrap()
}

pub(super) const RESOLVE_A: &str = "aaaaaaaa-0000-4000-8000-000000000001";
pub(super) const RESOLVE_B: &str = "bbbbbbbb-0000-4000-8000-000000000002";
pub(super) const RESOLVE_AMBIGUOUS: &str = "aaaaaaaa-1111-4000-8000-000000000003";

pub(super) type RecordedMounts =
    Arc<std::sync::Mutex<Vec<Vec<(std::path::PathBuf, std::path::PathBuf, Access)>>>>;

type CheckpointLaunch = (Option<std::path::PathBuf>, Option<std::path::PathBuf>);

pub(super) struct FakeRuntime {
    pub(super) next: AtomicU64,
    pub(super) fail: AtomicBool,
    pub(super) fail_wait: AtomicBool,
    pub(super) hold_logs: AtomicBool,
    pub(super) delay: Duration,
    pub(super) result: ExitStatus,
    pub(super) signals: Arc<std::sync::Mutex<Vec<Signal>>>,
    pub(super) suspensions: Arc<std::sync::Mutex<Vec<bool>>>,
    pub(super) mounts: RecordedMounts,
    pub(super) networks: Arc<std::sync::Mutex<Vec<Vec<NetworkConfig>>>>,
    pub(super) programs: Arc<std::sync::Mutex<Vec<Process>>>,
    pub(super) extensions: Arc<std::sync::Mutex<Vec<Vec<hl_engine::extension::ExtensionSpec>>>>,
    pub(super) resources: Arc<std::sync::Mutex<Vec<crate::Resources>>>,
    pub(super) isolations: Arc<std::sync::Mutex<Vec<Isolation>>>,
    pub(super) publishes: Arc<std::sync::Mutex<Vec<Vec<crate::Publication>>>>,
    pub(super) terminals: Arc<std::sync::Mutex<Vec<Option<crate::Size>>>>,
    pub(super) checkpoints: Arc<std::sync::Mutex<Vec<CheckpointLaunch>>>,
    pub(super) resizes: Arc<std::sync::Mutex<Vec<crate::Size>>>,
    pub(super) health: std::sync::Mutex<Option<(Duration, std::collections::VecDeque<ExitStatus>)>>,
}

impl FakeRuntime {
    pub(super) fn new(result: ExitStatus) -> Self {
        Self {
            next: AtomicU64::new(40),
            fail: AtomicBool::new(false),
            fail_wait: AtomicBool::new(false),
            hold_logs: AtomicBool::new(false),
            delay: Duration::from_millis(10),
            result,
            signals: Arc::new(std::sync::Mutex::new(Vec::new())),
            suspensions: Arc::new(std::sync::Mutex::new(Vec::new())),
            mounts: Arc::new(std::sync::Mutex::new(Vec::new())),
            networks: Arc::new(std::sync::Mutex::new(Vec::new())),
            programs: Arc::new(std::sync::Mutex::new(Vec::new())),
            extensions: Arc::new(std::sync::Mutex::new(Vec::new())),
            resources: Arc::new(std::sync::Mutex::new(Vec::new())),
            isolations: Arc::new(std::sync::Mutex::new(Vec::new())),
            publishes: Arc::new(std::sync::Mutex::new(Vec::new())),
            terminals: Arc::new(std::sync::Mutex::new(Vec::new())),
            checkpoints: Arc::new(std::sync::Mutex::new(Vec::new())),
            resizes: Arc::new(std::sync::Mutex::new(Vec::new())),
            health: std::sync::Mutex::new(None),
        }
    }
}

struct FakeProcess {
    id: u64,
    delay: Duration,
    result: ExitStatus,
    fail_wait: bool,
    signals: Arc<std::sync::Mutex<Vec<Signal>>>,
    suspensions: Arc<std::sync::Mutex<Vec<bool>>>,
    resizes: Arc<std::sync::Mutex<Vec<crate::Size>>>,
    logs: std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<crate::LogChunk>>>,
    _log_owner: Option<tokio::sync::mpsc::UnboundedSender<crate::LogChunk>>,
    checkpoint_directory: Option<std::path::PathBuf>,
}

#[async_trait]
impl Running for FakeProcess {
    fn id(&self) -> u64 {
        self.id
    }
    fn domain(&self) -> Option<hl_engine::Domain> {
        None
    }
    async fn wait(self: Arc<Self>) -> Result<ExitStatus> {
        tokio::time::sleep(self.delay).await;
        if self.fail_wait {
            return Err(Error::Runtime("injected wait failure".into()));
        }
        Ok(self.result)
    }
    async fn signal(&self, signal: Signal) -> Result<()> {
        self.signals.lock().unwrap().push(signal);
        Ok(())
    }
    async fn pause(&self) -> Result<()> {
        self.suspensions.lock().unwrap().push(true);
        Ok(())
    }
    async fn resume(&self) -> Result<()> {
        self.suspensions.lock().unwrap().push(false);
        Ok(())
    }
    async fn checkpoint(&self, _timeout: Duration) -> Result<std::path::PathBuf> {
        self.checkpoint_directory
            .clone()
            .ok_or_else(|| Error::Runtime("process was not armed for checkpoint".into()))
    }
    async fn resize(&self, size: crate::Size) -> Result<()> {
        self.resizes.lock().unwrap().push(size);
        Ok(())
    }
    fn take_logs(&self) -> Option<tokio::sync::mpsc::UnboundedReceiver<crate::LogChunk>> {
        self.logs.lock().unwrap().take()
    }
}

#[async_trait]
impl Runtime for FakeRuntime {
    async fn start(&self, launch: ProcessConfig) -> Result<Arc<dyn Running>> {
        assert!(launch.rootfs.is_absolute());
        let is_health = launch.process.program == "/health"
            || launch
                .process
                .args
                .iter()
                .any(|value| value == "__health__");
        self.programs.lock().unwrap().push(launch.process.clone());
        self.extensions.lock().unwrap().push(launch.extensions);
        self.resources.lock().unwrap().push(launch.resources);
        self.isolations.lock().unwrap().push(launch.isolation);
        self.publishes.lock().unwrap().push(launch.publish);
        self.terminals.lock().unwrap().push(launch.terminal);
        self.checkpoints.lock().unwrap().push((
            launch.checkpoint_directory.clone(),
            launch.restore_directory.clone(),
        ));
        self.mounts.lock().unwrap().push(
            launch
                .mounts
                .iter()
                .map(|mount| (mount.source.clone(), mount.target.clone(), mount.access))
                .collect(),
        );
        self.networks.lock().unwrap().push(launch.networks);
        if self.fail.load(Ordering::SeqCst) {
            return Err(Error::Runtime("injected launch failure".into()));
        }
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        sender
            .send(crate::LogChunk {
                stream: crate::Stream::Stdout,
                bytes: b"fake-out\n".to_vec(),
            })
            .unwrap();
        sender
            .send(crate::LogChunk {
                stream: crate::Stream::Stderr,
                bytes: b"fake-err\n".to_vec(),
            })
            .unwrap();
        let log_owner = self.hold_logs.load(Ordering::SeqCst).then_some(sender);
        let (delay, result) = if is_health {
            let mut health = self.health.lock().unwrap();
            let (delay, results) = health.as_mut().expect("health runtime is configured");
            (*delay, results.pop_front().unwrap_or(self.result))
        } else {
            (self.delay, self.result)
        };
        Ok(Arc::new(FakeProcess {
            id: self.next.fetch_add(1, Ordering::SeqCst),
            delay,
            result,
            fail_wait: self.fail_wait.load(Ordering::SeqCst),
            signals: Arc::clone(&self.signals),
            suspensions: Arc::clone(&self.suspensions),
            resizes: Arc::clone(&self.resizes),
            logs: std::sync::Mutex::new(Some(receiver)),
            _log_owner: log_owner,
            checkpoint_directory: launch.checkpoint_directory,
        }))
    }
}

pub(super) fn spec(name: &str) -> ContainerSpec {
    ContainerSpec::from_directory(
        "/rootfs",
        Process::new("/bin/sh").args(["-c", "exit 7"]).env("A", "B"),
    )
    .name(name)
    .guest(Guest::Aarch64)
}

pub(super) async fn service(runtime: Arc<FakeRuntime>) -> Containers {
    test_containers(Arc::new(Memory::default()), runtime)
        .await
        .unwrap()
}

pub(super) struct Clock;

impl crate::Device for Clock {
    fn name(&self) -> &'static str {
        "clock"
    }

    fn request(&self, _context: crate::DeviceContext<'_>) -> Result<crate::DeviceRequest> {
        Ok(crate::DeviceRequest {
            environment: std::collections::BTreeMap::from([(
                "CLOCK_PROVIDER".to_owned(),
                "host".to_owned(),
            )]),
            extensions: vec![hl_engine::extension::ExtensionSpec {
                provider: hl_engine::extension::ProviderId::new("test.clock").unwrap(),
                version: hl_engine::spec::Version::new(1, 0),
                required: false,
                required_features: BTreeSet::default(),
                optional_features: BTreeSet::default(),
                config: hl_engine::extension::ExtensionConfig::empty("test.clock/v1"),
                namespace: Vec::new(),
                services: Vec::new(),
                memory: Vec::new(),
                environment: Vec::new(),
            }],
            ..Default::default()
        })
    }
}
