pub(super) use super::super::builder::build_with;
pub(super) use super::super::*;
pub(super) use crate::{
    Container, ContainerSpec, ExitStatus, Signal,
    error::Result,
    service::Runtime,
    storage::{Disk, Memory},
};

#[derive(Default)]
pub(super) struct Recorded(pub(super) std::sync::Mutex<Vec<LifecycleAction>>);

impl LifecycleEvents for Recorded {
    fn emit(&self, event: LifecycleEvent) {
        self.0.lock().unwrap().push(event.action);
    }
}

pub(super) use crate::{
    Access, ContainerState, Error, Guest, Isolation, Process,
    service::{NetworkConfig, ProcessConfig, Running},
    storage::Containers as _,
};
use async_trait::async_trait;
pub(super) use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
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
        runtime_diagnostic: None,
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

pub(super) type RecordedMounts = Arc<std::sync::Mutex<Vec<Vec<(std::path::PathBuf, std::path::PathBuf, Access)>>>>;
pub(super) type RecordedInputs = Arc<std::sync::Mutex<Vec<(u64, Vec<u8>)>>>;

type CheckpointLaunch = Option<bool>;

struct DelayedLog {
    initial: Vec<u8>,
    late: Vec<u8>,
    ready: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}

struct DelayedProcessLog {
    late: Vec<u8>,
    ready: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
    sender: crate::service::LogSender,
}

pub(super) struct FakeRuntime {
    pub(super) next: AtomicU64,
    pub(super) fail: AtomicBool,
    pub(super) launch_failures: Arc<std::sync::Mutex<std::collections::BTreeMap<String, String>>>,
    pub(super) fail_wait: AtomicBool,
    pub(super) fail_signal: AtomicBool,
    pub(super) blocking_wait: AtomicBool,
    pub(super) panic_wait: AtomicBool,
    pub(super) fail_checkpoint: Arc<AtomicU64>,
    pub(super) checkpoint_exits: Arc<AtomicU64>,
    pub(super) hold_logs: AtomicBool,
    pub(super) checkpointable: AtomicBool,
    /// One entry per launch: `Some(true)` coordinator, `Some(false)` domain member.
    pub(super) checkpoint_roles: Arc<std::sync::Mutex<Vec<Option<bool>>>>,
    pub(super) delay: Duration,
    pub(super) restore_delay: Option<Duration>,
    pub(super) result: ExitStatus,
    pub(super) signals: Arc<std::sync::Mutex<Vec<Signal>>>,
    pub(super) waits: Arc<AtomicU64>,
    pub(super) suspensions: Arc<std::sync::Mutex<Vec<bool>>>,
    pub(super) mounts: RecordedMounts,
    pub(super) networks: Arc<std::sync::Mutex<Vec<Vec<NetworkConfig>>>>,
    pub(super) programs: Arc<std::sync::Mutex<Vec<Process>>>,
    pub(super) resources: Arc<std::sync::Mutex<Vec<crate::Resources>>>,
    pub(super) isolations: Arc<std::sync::Mutex<Vec<Isolation>>>,
    pub(super) publishes: Arc<std::sync::Mutex<Vec<Vec<crate::Publication>>>>,
    pub(super) terminals: Arc<std::sync::Mutex<Vec<Option<crate::Size>>>>,
    pub(super) checkpoints: Arc<std::sync::Mutex<Vec<CheckpointLaunch>>>,
    pub(super) domains: Arc<std::sync::Mutex<Vec<(hl_engine::Domain, bool)>>>,
    pub(super) inputs: RecordedInputs,
    pub(super) domain_reads: Arc<AtomicU64>,
    pub(super) resizes: Arc<std::sync::Mutex<Vec<crate::Size>>>,
    pub(super) health: std::sync::Mutex<Option<(Duration, std::collections::VecDeque<ExitStatus>)>>,
    delayed_logs: std::sync::Mutex<std::collections::VecDeque<DelayedLog>>,
}

impl FakeRuntime {
    pub(super) fn new(result: ExitStatus) -> Self {
        Self {
            next: AtomicU64::new(40),
            fail: AtomicBool::new(false),
            launch_failures: Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new())),
            fail_wait: AtomicBool::new(false),
            fail_signal: AtomicBool::new(false),
            blocking_wait: AtomicBool::new(false),
            panic_wait: AtomicBool::new(false),
            fail_checkpoint: Arc::new(AtomicU64::new(0)),
            checkpoint_exits: Arc::new(AtomicU64::new(0)),
            hold_logs: AtomicBool::new(false),
            checkpointable: AtomicBool::new(true),
            checkpoint_roles: Arc::new(std::sync::Mutex::new(Vec::new())),
            delay: Duration::from_millis(10),
            restore_delay: None,
            result,
            signals: Arc::new(std::sync::Mutex::new(Vec::new())),
            waits: Arc::new(AtomicU64::new(0)),
            suspensions: Arc::new(std::sync::Mutex::new(Vec::new())),
            mounts: Arc::new(std::sync::Mutex::new(Vec::new())),
            networks: Arc::new(std::sync::Mutex::new(Vec::new())),
            programs: Arc::new(std::sync::Mutex::new(Vec::new())),
            resources: Arc::new(std::sync::Mutex::new(Vec::new())),
            isolations: Arc::new(std::sync::Mutex::new(Vec::new())),
            publishes: Arc::new(std::sync::Mutex::new(Vec::new())),
            terminals: Arc::new(std::sync::Mutex::new(Vec::new())),
            checkpoints: Arc::new(std::sync::Mutex::new(Vec::new())),
            domains: Arc::new(std::sync::Mutex::new(Vec::new())),
            inputs: Arc::new(std::sync::Mutex::new(Vec::new())),
            domain_reads: Arc::new(AtomicU64::new(0)),
            resizes: Arc::new(std::sync::Mutex::new(Vec::new())),
            health: std::sync::Mutex::new(None),
            delayed_logs: std::sync::Mutex::new(std::collections::VecDeque::new()),
        }
    }

    pub(super) fn delay_next_log(
        &self,
        initial: impl Into<Vec<u8>>,
        late: impl Into<Vec<u8>>,
    ) -> (tokio::sync::oneshot::Receiver<()>, tokio::sync::oneshot::Sender<()>) {
        let (ready, ready_rx) = tokio::sync::oneshot::channel();
        let (release, release_rx) = tokio::sync::oneshot::channel();
        self.delayed_logs.lock().unwrap().push_back(DelayedLog {
            initial: initial.into(),
            late: late.into(),
            ready,
            release: release_rx,
        });
        (ready_rx, release)
    }
}

struct FakeProcess {
    id: u64,
    delay: Duration,
    result: ExitStatus,
    fail_wait: bool,
    fail_signal: bool,
    blocking_wait: bool,
    panic_wait: bool,
    signals: Arc<std::sync::Mutex<Vec<Signal>>>,
    waits: Arc<AtomicU64>,
    suspensions: Arc<std::sync::Mutex<Vec<bool>>>,
    resizes: Arc<std::sync::Mutex<Vec<crate::Size>>>,
    logs: std::sync::Mutex<Option<crate::service::LogReceiver>>,
    _log_owner: Option<crate::service::LogSender>,
    checkpoint_armed: bool,
    checkpoint_failure: Arc<AtomicU64>,
    checkpoint_exit: tokio::sync::watch::Sender<bool>,
    checkpoint_exits: Arc<AtomicU64>,
    delayed_log: std::sync::Mutex<Option<DelayedProcessLog>>,
    domain: hl_engine::Domain,
    domain_reads: Arc<AtomicU64>,
}

#[async_trait]
impl Running for FakeProcess {
    fn id(&self) -> u64 {
        self.id
    }
    fn domain(&self) -> hl_engine::Domain {
        self.domain_reads.fetch_add(1, Ordering::SeqCst);
        self.domain
    }
    fn checkpointable(&self) -> bool {
        self.checkpoint_armed
    }
    async fn wait(self: Arc<Self>) -> Result<ExitStatus> {
        self.waits.fetch_add(1, Ordering::SeqCst);
        let delayed = self.delayed_log.lock().unwrap().take();
        if let Some(delayed) = delayed {
            let _ = delayed.ready.send(());
            delayed
                .release
                .await
                .map_err(|_| Error::Runtime("delayed log release was dropped".into()))?;
            delayed
                .sender
                .send(crate::LogChunk {
                    stream: crate::Stream::Stdout,
                    bytes: delayed.late,
                })
                .await
                .map_err(|_| Error::Runtime("delayed log receiver closed".into()))?;
        }
        if self.blocking_wait {
            let delay = self.delay;
            tokio::task::spawn_blocking(move || std::thread::sleep(delay))
                .await
                .unwrap();
        } else {
            let mut checkpoint_exit = self.checkpoint_exit.subscribe();
            if !*checkpoint_exit.borrow() {
                tokio::select! {
                    () = tokio::time::sleep(self.delay) => {}
                    changed = checkpoint_exit.changed() => {
                        assert!(changed.is_ok() && *checkpoint_exit.borrow(), "checkpoint exit channel closed");
                    }
                }
            }
        }
        assert!(!self.panic_wait, "injected wait panic");
        if self.fail_wait {
            return Err(Error::Runtime("injected wait failure".into()));
        }
        Ok(self.result)
    }
    async fn signal(&self, signal: Signal) -> Result<()> {
        self.signals.lock().unwrap().push(signal);
        if self.fail_signal {
            return Err(Error::Runtime("injected signal failure".into()));
        }
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
    async fn checkpoint(&self, _timeout: Duration) -> Result<()> {
        if self.checkpoint_failure.load(Ordering::SeqCst) == self.id {
            return Err(Error::Runtime("injected checkpoint failure".into()));
        }
        if self.checkpoint_armed {
            self.checkpoint_exit.send_replace(true);
            self.checkpoint_exits.fetch_add(1, Ordering::SeqCst);
            Ok(())
        } else {
            Err(Error::Runtime("process was not armed for checkpoint".into()))
        }
    }
    async fn resize(&self, size: crate::Size) -> Result<()> {
        self.resizes.lock().unwrap().push(size);
        Ok(())
    }
    fn take_logs(&self) -> Option<crate::service::LogReceiver> {
        self.logs.lock().unwrap().take()
    }
}

#[async_trait]
impl Runtime for FakeRuntime {
    async fn start(&self, launch: ProcessConfig) -> Result<Arc<dyn Running>> {
        assert!(launch.rootfs.is_absolute());
        let restoring = matches!(
            launch.checkpoint,
            Some(crate::service::CheckpointRole::Coordinator(ref checkpoint)) if checkpoint.restore
        );
        let domain = launch.domain.unwrap_or(
            hl_engine::Domain::new().map_err(|error| Error::Runtime(format!("domain allocation failed: {error}")))?,
        );
        let is_health =
            launch.process.program == "/health" || launch.process.args.iter().any(|value| value == "__health__");
        self.programs.lock().unwrap().push(launch.process.clone());
        self.resources.lock().unwrap().push(launch.resources);
        self.isolations.lock().unwrap().push(launch.isolation);
        self.publishes.lock().unwrap().push(launch.publish);
        self.terminals.lock().unwrap().push(launch.terminal);
        self.checkpoints
            .lock()
            .unwrap()
            .push(match launch.checkpoint {
                Some(crate::service::CheckpointRole::Coordinator(ref checkpoint)) => Some(checkpoint.restore),
                Some(crate::service::CheckpointRole::DomainMember) | None => None,
            });
        self.domains.lock().unwrap().push((domain, launch.domain_owner));
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
        if let Some(error) = self
            .launch_failures
            .lock()
            .unwrap()
            .get(&launch.process.program)
            .cloned()
        {
            return Err(Error::Runtime(error));
        }
        let (sender, receiver) = crate::service::log_channel();
        let delayed = self.delayed_logs.lock().unwrap().pop_front();
        let delayed_log = if let Some(delayed) = delayed {
            sender
                .try_send(crate::LogChunk {
                    stream: crate::Stream::Stdout,
                    bytes: delayed.initial,
                })
                .unwrap();
            Some(DelayedProcessLog {
                late: delayed.late,
                ready: delayed.ready,
                release: delayed.release,
                sender: sender.clone(),
            })
        } else {
            sender
                .try_send(crate::LogChunk {
                    stream: crate::Stream::Stdout,
                    bytes: b"fake-out\n".to_vec(),
                })
                .unwrap();
            sender
                .try_send(crate::LogChunk {
                    stream: crate::Stream::Stderr,
                    bytes: b"fake-err\n".to_vec(),
                })
                .unwrap();
            None
        };
        let log_owner = self.hold_logs.load(Ordering::SeqCst).then_some(sender);
        let (delay, result) = if is_health {
            let mut health = self.health.lock().unwrap();
            let (delay, results) = health.as_mut().expect("health runtime is configured");
            (*delay, results.pop_front().unwrap_or(self.result))
        } else {
            (
                if restoring {
                    self.restore_delay.unwrap_or(self.delay)
                } else {
                    self.delay
                },
                self.result,
            )
        };
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let (checkpoint_exit, _) = tokio::sync::watch::channel(false);
        if let Some(mut input) = launch.input {
            let received = Arc::clone(&self.inputs);
            tokio::spawn(async move {
                while let Some(bytes) = input.recv().await {
                    received.lock().unwrap().push((id, bytes));
                }
            });
        }
        self.checkpoint_roles
            .lock()
            .unwrap()
            .push(launch.checkpoint.as_ref().map(|role| {
                matches!(role, crate::service::CheckpointRole::Coordinator(_))
            }));
        Ok(Arc::new(FakeProcess {
            id,
            delay,
            result,
            fail_wait: self.fail_wait.load(Ordering::SeqCst),
            fail_signal: self.fail_signal.load(Ordering::SeqCst),
            blocking_wait: self.blocking_wait.load(Ordering::SeqCst),
            panic_wait: self.panic_wait.load(Ordering::SeqCst),
            signals: Arc::clone(&self.signals),
            waits: Arc::clone(&self.waits),
            suspensions: Arc::clone(&self.suspensions),
            resizes: Arc::clone(&self.resizes),
            logs: std::sync::Mutex::new(Some(receiver)),
            _log_owner: log_owner,
            checkpoint_armed: matches!(launch.checkpoint, Some(crate::service::CheckpointRole::Coordinator(_)))
                && self.checkpointable.load(Ordering::SeqCst),
            checkpoint_failure: Arc::clone(&self.fail_checkpoint),
            checkpoint_exit,
            checkpoint_exits: Arc::clone(&self.checkpoint_exits),
            delayed_log: std::sync::Mutex::new(delayed_log),
            domain,
            domain_reads: Arc::clone(&self.domain_reads),
        }))
    }
}

pub(super) fn spec(name: &str) -> ContainerSpec {
    ContainerSpec::from_directory("/rootfs", Process::new("/bin/sh").args(["-c", "exit 7"]).env("A", "B"))
        .name(name)
        .guest(Guest::Aarch64)
}

pub(super) async fn service(runtime: Arc<FakeRuntime>) -> Containers {
    test_containers(Arc::new(Memory::default()), runtime).await.unwrap()
}
