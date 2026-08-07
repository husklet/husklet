//! Docker-compatible wire types and HTTP implementation.

use serde::Deserialize as _;

#[hl_design::classify(domain = "serde")]
fn null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de> + Default,
{
    Option::<T>::deserialize(deserializer).map(Option::unwrap_or_default)
}

mod event;
mod filter;
mod model;
mod network;
mod port;
mod volume;

pub use event::{Actor, Event, EventFilter, EventQuery};
pub use model::{
    Attachment, Authentication, BindOptions, BindReadOnly, BlockIo, BuildPrune, Change, ChangeKind, CommitOptions,
    CompatibilityFields, Container, ContainerConfig, ContainerCreation, ContainerLogs, ContainerMetadata,
    ContainerPrune, ContainerState, Cpu, CpuUsage, CreateContainer, Credentials, DiskUsage, Distribution, DockerError,
    DockerMount, DriverConfig, EndpointSettings, EndpointsConfig, EnvError, EnvVar, EnvVars, ExecConfig, ExecCreated,
    ExecInspect, ExecOpen, ExecProcess, ExecStart, HealthLog, HealthState, Healthcheck, HostConfig, ImageCommit,
    ImageConfig, ImageDelete, ImageHistory, ImageLoad, ImagePrune, ImageSummary, InspectContainer, InspectHostConfig,
    InspectImage, LogOptions, LogProtocolError, LogStreams, Memory, MountPoint, NetworkSettings, NetworkingConfig,
    PathStat, Pids, Plugin, ProgressDetail, PullProgress, PushAux, PushProgress, RestartPolicy, Search, Stats,
    SystemInfo, SystemPrune, Throttling, Top, Update, UpdateResult, UsageData, Version, VolumeOptions, VolumeUsage,
    Wait,
};
pub use network::{
    ConfigFrom, EndpointConfig, EndpointIpam, Ipam, IpamConfig, Network, NetworkConnect, NetworkContainer,
    NetworkCreate, NetworkCreated, NetworkDisconnect, NetworkPrune,
};
pub use port::{ExposedPorts, PortBinding, PortBindings, PortSummary};
pub use volume::{Volume, VolumeCreate, VolumeList, VolumePrune};

#[cfg(feature = "runtime")]
mod http;

pub use filter::List;
#[cfg(feature = "runtime")]
pub(crate) use http::router;
