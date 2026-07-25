use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use hl_container::{
    Console, ContainerSpec, Error as ContainerError, ExitStatus, Isolation, Mount, NetworkDriver,
    NetworkSpec, Resources, Signal, Streams, Subnet,
};
use hl_images::{Reference, RuntimeOverrides};
use http_body_util::BodyExt as _;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::{Seek as _, SeekFrom};
use std::str::FromStr;
use tokio::io::{AsyncSeekExt as _, AsyncWriteExt as _};

use super::console::{Connection, Resize};
use super::error::{ApiError, ApiResult};
use super::DockerState;
use crate::api::{
    Change, Container, ContainerCreation, ContainerPrune, CreateContainer, EndpointConfig, EnvVars,
    HostConfig, InspectContainer, LogOptions, LogStreams, MountPoint, NetworkingConfig, PathStat,
    Update, UpdateResult, Wait,
};

const ARCHIVE_LIMIT: u64 = 8 * 1024 * 1024 * 1024;
const DEFAULT_NETWORK: &str = "bridge";

mod archive;
mod control;
mod create;
mod host;
mod inspect;
mod lifecycle;
mod list;
mod logs;
mod mount;

pub(super) use control::DockerSignal;
use host::HostSettings;
use list::NetworkPlan;
use logs::Flag;
use mount::{LegacyBind, Target};

pub(super) use archive::{archive, export, extract, stat};
pub(super) use control::{
    attach, checkpoint, kill, pause, rename, resize, restart, start, stop, unpause,
};
pub(super) use create::create;
pub(super) use inspect::{changes, inspect, update};
pub(super) use lifecycle::{remove, wait};
pub(super) use list::{list, prune, PruneQuery};
pub(super) use logs::logs;

#[cfg(test)]
use archive::ArchiveQuery;
#[cfg(test)]
use logs::LogsQuery;

#[cfg(test)]
mod tests;
