use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use hl_container::{
    Console, ContainerSpec, Error as ContainerError, ExitStatus, Isolation, Mount, NetworkDriver, NetworkSpec,
    Resources, Signal, Streams, Subnet,
};
use hl_images::{Reference, RuntimeOverrides};
use http_body_util::BodyExt as _;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::{Seek as _, SeekFrom};
use std::str::FromStr;
use tokio::io::{AsyncSeekExt as _, AsyncWriteExt as _};

use super::DockerState;
use super::console::{Connection, Resize};
use super::error::{ApiError, ApiResult};
use crate::api::{
    Change, Container, ContainerCreation, ContainerPrune, CreateContainer, EndpointConfig, EnvVars, HostConfig,
    InspectContainer, LogOptions, LogStreams, MountPoint, NetworkingConfig, PathStat, Update, UpdateResult, Wait,
};

const ARCHIVE_LIMIT: u64 = 8 * 1024 * 1024 * 1024;
const DEFAULT_NETWORK: &str = "bridge";

mod archive;
mod attach;
mod control;
mod create;
mod host;
mod inspect;
mod kill;
mod lifecycle;
mod list;
mod logs;
mod mount;

use host::HostSettings;
pub(super) use kill::DockerSignal;
use list::NetworkPlan;
use logs::Flag;
use mount::{LegacyBind, Target};

pub(super) use archive::{archive, extract, stat};
pub(super) use attach::attach;
pub(super) use control::{checkpoint, pause, rename, resize, restart, start, stop, unpause};
pub(super) use create::create;
pub(super) use inspect::{changes, inspect, update};
pub(super) use kill::kill;
pub(super) use lifecycle::{remove, wait};
pub(super) use list::{PruneQuery, list, prune};
pub(super) use logs::logs;

#[hl_design::adapter]
pub(super) async fn export(state: State<DockerState>, id: Path<String>) -> ApiResult<Response> {
    let mut response = archive::export(state, id).await?;
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/octet-stream"),
    );
    response.headers_mut().remove("X-Docker-Container-Path-Stat");
    Ok(response)
}

#[cfg(test)]
use archive::ArchiveQuery;
#[cfg(test)]
use logs::LogsQuery;

#[cfg(test)]
mod tests;
