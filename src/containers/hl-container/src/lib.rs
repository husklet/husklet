//! Embeddable headless Linux container lifecycle.
#![forbid(unsafe_code)]

mod checkpoint;
mod config;
mod console;
mod containers;
mod engine;
mod error;
mod executions;
pub mod filesystem;
mod generation;
mod identity;
mod model;
mod networks;
mod service;
mod storage;
mod volume_size;
mod volumes;

pub use checkpoint::{CheckpointError, CheckpointImage, CheckpointImages};
pub use config::{Config, Persistence};
pub use console::{Input, Session};
pub use containers::{
    Builder, CommitMetadata, Containers, FilesystemUsage, LifecycleAction, LifecycleEvent, LifecycleEvents,
};
pub use error::{Error, Result};
pub use executions::Executions;
pub use filesystem::{Change, ChangeKind, Changes, Extraction, Filesystem, Limits, Stat};
pub use model::{
    Access, BindPropagation, Check, Checkpoint, Console, Container, ContainerId, ContainerSpec, ContainerState,
    Endpoint, EndpointSpec, Entry, Environment, EnvironmentRecord, Exec, ExecId, ExecSpec, ExecState, Execution,
    ExitStatus, FaultCause, Guest, Health, HealthStatus, Healthcheck, Isolation, Logs, Mount, MountSource, Network,
    NetworkDriver, NetworkId, NetworkMode, NetworkSpec, Port, Probe, Process, Protocol, Prune, Publication,
    RemovalPolicy, Resolver, Resources, Restart, RestartPolicy, Rootfs, Sandbox, SeccompBaseline, Signal, Size, Stream,
    Streams, Subnet, Update, Volume, VolumeKind, VolumeSource, VolumeSpec, WaitCondition,
};
pub(crate) use model::{JournalId, LogChunk};
pub use networks::Networks;
pub use volumes::Volumes;
