mod attachment;
mod catalog;
mod checkpoint;
mod control;
mod exec;
mod filesystem;
mod health;
mod launch;
mod removal;
mod restart;
mod rollback;

use super::{NetworkConfig, ProcessConfig, Running, Runtime};
use crate::console::Io;
use crate::storage::{Containers as ContainerStorage, Execs as ExecStorage, Logs as LogStorage, Storage};
use crate::{
    Check, Container, ContainerId, ContainerSpec, ContainerState, Error, Exec, ExecId, ExecSpec, ExecState, ExitStatus,
    Healthcheck, JournalId, Probe, Result, Rootfs, Signal, WaitCondition, model::now_ms,
};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex, Notify};

struct Run {
    process: Arc<dyn Running>,
    health: tokio::sync::watch::Sender<bool>,
    output_complete: tokio::sync::watch::Receiver<bool>,
}

struct OutputOwner {
    abort: tokio::task::AbortHandle,
}

pub(crate) struct Service {
    containers: Arc<dyn ContainerStorage>,
    execs: Arc<dyn ExecStorage>,
    logs: Arc<dyn LogStorage>,
    runtime: Arc<dyn Runtime>,
    operations: Arc<Mutex<()>>,
    waiters: Mutex<HashMap<ContainerId, Arc<Notify>>>,
    exits: Mutex<HashMap<String, ExitStatus>>,
    failures: Mutex<HashMap<JournalId, String>>,
    live: Mutex<HashMap<ContainerId, Run>>,
    launch_cleanups: Mutex<HashMap<ContainerId, tokio::task::AbortHandle>>,
    launch_cleanup_failures: Mutex<HashMap<ContainerId, String>>,
    restarts: Mutex<HashMap<ContainerId, tokio::sync::watch::Sender<bool>>>,
    exec_live: Mutex<HashMap<ExecId, Arc<dyn Running>>>,
    exec_output_complete: Mutex<HashMap<ExecId, tokio::sync::watch::Receiver<bool>>>,
    output_owners: Mutex<HashMap<JournalId, Arc<OutputOwner>>>,
    exec_cleanups: Mutex<HashMap<ExecId, tokio::task::AbortHandle>>,
    exec_cleanup_failures: Mutex<HashMap<ExecId, String>>,
    exec_waiters: Mutex<HashMap<ExecId, Arc<Notify>>>,
    io: Mutex<HashMap<JournalId, Arc<Io>>>,
    next_io_generation: AtomicU64,
    #[cfg(test)]
    checkpoint_all_gate: Mutex<CheckpointAllGate>,
    #[cfg(test)]
    exec_start_attempts: AtomicU64,
    last_created_ms: AtomicU64,
    rootfs: Option<hl_images::rootfs::Roots>,
    images: Option<hl_images::Images>,
    volumes: crate::Volumes,
    networks: crate::Networks,
    identity: crate::identity::Identity,
    translation_cache: Option<std::path::PathBuf>,
    translation_cache_observability: bool,
    translation_symbols: Option<std::path::PathBuf>,
    events: std::sync::RwLock<Vec<Arc<dyn crate::LifecycleEvents>>>,
    event_history: std::sync::Mutex<Vec<crate::LifecycleEvent>>,
    checkpoints: Arc<dyn crate::CheckpointImages>,
}

#[cfg(test)]
#[derive(Default)]
struct CheckpointAllGate {
    ready: Option<tokio::sync::oneshot::Sender<()>>,
    release: Option<tokio::sync::oneshot::Receiver<()>>,
}

pub(crate) struct Dependencies<S> {
    pub(crate) storage: Arc<S>,
    pub(crate) runtime: Arc<dyn Runtime>,
    pub(crate) rootfs: Option<hl_images::rootfs::Roots>,
    pub(crate) images: Option<hl_images::Images>,
    pub(crate) volumes: crate::Volumes,
    pub(crate) networks: crate::Networks,
    pub(crate) runtime_root: std::path::PathBuf,
    pub(crate) translation_cache: Option<std::path::PathBuf>,
    pub(crate) translation_cache_observability: bool,
    pub(crate) translation_symbols: Option<std::path::PathBuf>,
    pub(crate) checkpoints: Arc<dyn crate::CheckpointImages>,
}

impl Service {
    pub(crate) fn validate_overlay(&self, overlay: &super::OverlayConfig) -> bool {
        self.runtime.validate_overlay(overlay)
    }
    pub(crate) fn new<S: Storage + 'static>(dependencies: Dependencies<S>) -> Self {
        let Dependencies {
            storage,
            runtime,
            rootfs,
            images,
            volumes,
            networks,
            runtime_root,
            translation_cache,
            translation_cache_observability,
            translation_symbols,
            checkpoints,
        } = dependencies;
        let operations = volumes.operation();
        let containers: Arc<dyn ContainerStorage> = storage.clone();
        let execs: Arc<dyn ExecStorage> = storage.clone();
        let logs: Arc<dyn LogStorage> = storage;
        Self {
            containers,
            execs,
            logs,
            runtime,
            operations,
            waiters: Mutex::new(HashMap::new()),
            exits: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
            live: Mutex::new(HashMap::new()),
            launch_cleanups: Mutex::new(HashMap::new()),
            launch_cleanup_failures: Mutex::new(HashMap::new()),
            restarts: Mutex::new(HashMap::new()),
            exec_live: Mutex::new(HashMap::new()),
            exec_output_complete: Mutex::new(HashMap::new()),
            output_owners: Mutex::new(HashMap::new()),
            exec_cleanups: Mutex::new(HashMap::new()),
            exec_cleanup_failures: Mutex::new(HashMap::new()),
            exec_waiters: Mutex::new(HashMap::new()),
            io: Mutex::new(HashMap::new()),
            next_io_generation: AtomicU64::new(1),
            #[cfg(test)]
            checkpoint_all_gate: Mutex::new(CheckpointAllGate::default()),
            #[cfg(test)]
            exec_start_attempts: AtomicU64::new(0),
            last_created_ms: AtomicU64::new(0),
            rootfs,
            images,
            volumes,
            networks,
            identity: crate::identity::Identity::new(runtime_root.clone()),
            translation_cache,
            translation_cache_observability,
            translation_symbols,
            events: std::sync::RwLock::new(Vec::new()),
            event_history: std::sync::Mutex::new(Vec::new()),
            checkpoints,
        }
    }

    pub(crate) fn observe(&self, events: Arc<dyn crate::LifecycleEvents>) {
        if let Ok(history) = self.event_history.lock() {
            for event in history.iter().cloned() {
                events.emit(event);
            }
            if let Ok(mut sinks) = self.events.write() {
                sinks.push(events);
            }
        }
    }

    fn emit(&self, action: crate::LifecycleAction, container: &Container) {
        let event = crate::LifecycleEvent {
            action,
            container: container.clone(),
        };
        if let Ok(mut history) = self.event_history.lock() {
            if history.len() == 4096 {
                history.remove(0);
            }
            history.push(event.clone());
        }
        if let Ok(sinks) = self.events.read() {
            for sink in sinks.iter() {
                sink.emit(event.clone());
            }
        }
    }

    fn next_io_generation(&self) -> Result<u64> {
        self.next_io_generation
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| value.checked_add(1))
            .map_err(|_| Error::Runtime("process I/O generation space is exhausted".into()))
    }

    #[cfg(test)]
    pub(crate) fn exhaust_io_generations(&self) {
        self.next_io_generation.store(u64::MAX, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) async fn gate_checkpoint_all(
        &self,
    ) -> (tokio::sync::oneshot::Receiver<()>, tokio::sync::oneshot::Sender<()>) {
        let (ready, ready_rx) = tokio::sync::oneshot::channel();
        let (release, release_rx) = tokio::sync::oneshot::channel();
        *self.checkpoint_all_gate.lock().await = CheckpointAllGate {
            ready: Some(ready),
            release: Some(release_rx),
        };
        (ready_rx, release)
    }

    #[cfg(test)]
    async fn wait_checkpoint_all_gate(&self) {
        let (ready, release) = {
            let mut gate = self.checkpoint_all_gate.lock().await;
            (gate.ready.take(), gate.release.take())
        };
        if let (Some(ready), Some(release)) = (ready, release) {
            let _ = ready.send(());
            let _ = release.await;
        }
    }

    #[cfg(test)]
    pub(crate) fn exec_start_attempts(&self) -> u64 {
        self.exec_start_attempts.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) async fn checkpoint_order(&self) -> Result<Vec<ContainerId>> {
        self.containers
            .list()
            .await
            .map(|containers| containers.into_iter().map(|container| container.id).collect())
    }
}
