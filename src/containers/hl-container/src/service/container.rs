mod attachment;
mod catalog;
mod control;
mod exec;
mod filesystem;
mod health;
mod launch;
mod removal;
mod restart;

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
    restarts: Mutex<HashMap<ContainerId, tokio::sync::watch::Sender<bool>>>,
    exec_live: Mutex<HashMap<ExecId, Arc<dyn Running>>>,
    exec_waiters: Mutex<HashMap<ExecId, Arc<Notify>>>,
    io: Mutex<HashMap<JournalId, Arc<Io>>>,
    last_created_ms: AtomicU64,
    rootfs: Option<hl_images::rootfs::Roots>,
    images: Option<hl_images::Images>,
    volumes: crate::Volumes,
    networks: crate::Networks,
    identity: crate::identity::Identity,
    translation_cache: Option<std::path::PathBuf>,
    events: std::sync::RwLock<Vec<Arc<dyn crate::LifecycleEvents>>>,
    event_history: std::sync::Mutex<Vec<crate::LifecycleEvent>>,
    checkpoints: Arc<dyn crate::CheckpointImages>,
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
            restarts: Mutex::new(HashMap::new()),
            exec_live: Mutex::new(HashMap::new()),
            exec_waiters: Mutex::new(HashMap::new()),
            io: Mutex::new(HashMap::new()),
            last_created_ms: AtomicU64::new(0),
            rootfs,
            images,
            volumes,
            networks,
            identity: crate::identity::Identity::new(runtime_root.clone()),
            translation_cache,
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
}
