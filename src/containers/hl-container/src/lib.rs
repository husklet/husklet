//! Embeddable headless Linux container lifecycle.
#![forbid(unsafe_code)]

mod config;
mod console;
mod containers;
pub mod device;
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
mod volumes;

pub use config::{Config, Persistence};
pub use console::{Input, Session};
pub use containers::{
    Builder, CommitMetadata, Containers, FilesystemUsage, LifecycleAction, LifecycleEvent,
    LifecycleEvents,
};
pub use device::{Authority, Device, DeviceContext, DeviceRequest, Devices, HandleAuthority};
pub use error::{Error, Result};
pub use executions::Executions;
pub use filesystem::{Change, ChangeKind, Changes, Filesystem, Limits, Stat};
pub use model::{
    Access, BindPropagation, Check, Checkpoint, Console, Container, ContainerId, ContainerSpec,
    ContainerState, Endpoint, EndpointSpec, Entry, Exec, ExecId, ExecSpec, ExecState, ExitStatus,
    Guest, Health, HealthStatus, Healthcheck, Isolation, Logs, Mount, MountSource, Network,
    NetworkDriver, NetworkId, NetworkMode, NetworkSpec, Port, Probe, Process, Protocol, Prune,
    Publication, RemovalPolicy, Resources, Restart, RestartPolicy, Rootfs, Sandbox, Signal, Size,
    Stream, Streams, Subnet, Update, Volume, VolumeKind, VolumeSource, VolumeSpec, WaitCondition,
};
pub(crate) use model::{JournalId, LogChunk};
pub use networks::Networks;
pub use volumes::Volumes;
