mod inspect;
mod start;

pub(super) use inspect::inspect;
pub(super) use start::start;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use hl_container::{ExecSpec, ExitStatus, Streams};
use serde::Deserialize;

use super::DockerState;
use super::console::{DetachKeys, Resize};
use super::error::{ApiError, ApiResult};
use crate::api::{ExecConfig, ExecCreated, Wait};

#[hl_design::adapter]
pub(super) async fn create(
    State(state): State<DockerState>,
    Path(container): Path<String>,
    Json(config): Json<ExecConfig>,
) -> ApiResult<(StatusCode, Json<ExecCreated>)> {
    DetachKeys::parse(&config.detach_keys)?;
    config.validate().map_err(|message| {
        let status = if message.contains("not implemented") || message.starts_with("unsupported") {
            StatusCode::NOT_IMPLEMENTED
        } else {
            StatusCode::BAD_REQUEST
        };
        ApiError::new(status, message)
    })?;
    let parent = state
        .containers
        .inspect(&container)
        .await
        .map_err(ApiError::container)?;
    let process = config
        .process(&parent.spec.process)
        .map_err(|message| ApiError::new(StatusCode::BAD_REQUEST, message))?;
    let spec = ExecSpec::new(process)
        .streams(Streams {
            stdin: config.attach.stdin,
            stdout: config.attach.stdout,
            stderr: config.attach.stderr,
        })
        .privileged(config.privileged)
        .detach_keys(config.detach_keys)
        .user(config.user);
    let exec = state
        .containers
        .executions()
        .create(&container, spec)
        .await
        .map_err(ApiError::container)?;
    Ok((
        StatusCode::CREATED,
        Json(ExecCreated {
            id: exec.id.to_string(),
        }),
    ))
}

#[hl_design::adapter]
pub(super) async fn wait(State(state): State<DockerState>, Path(id): Path<String>) -> ApiResult<Json<Wait>> {
    let exec_id = id
        .parse()
        .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, format!("no such exec: {id}")))?;
    let result = state
        .containers
        .executions()
        .wait(&exec_id)
        .await
        .map_err(ApiError::container)?;
    let status_code = match result {
        ExitStatus::Code(code) => code,
        ExitStatus::Signal(signal) => 128 + signal,
        ExitStatus::Fault { status, .. } => status,
    };
    Ok(Json(Wait {
        status_code: i64::from(status_code),
    }))
}

#[hl_design::adapter]
pub(super) async fn remove(State(state): State<DockerState>, Path(id): Path<String>) -> ApiResult<StatusCode> {
    let exec_id = id
        .parse()
        .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, format!("no such exec: {id}")))?;
    state
        .containers
        .executions()
        .remove(&exec_id)
        .await
        .map_err(ApiError::container)?;
    Ok(StatusCode::NO_CONTENT)
}

#[hl_design::adapter]
pub(super) async fn resize(
    State(state): State<DockerState>,
    Path(id): Path<String>,
    Query(query): Query<Resize>,
) -> ApiResult<StatusCode> {
    let exec_id = id
        .parse()
        .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, format!("no such exec: {id}")))?;
    state
        .containers
        .executions()
        .resize(&exec_id, query.size()?)
        .await
        .map_err(ApiError::container)?;
    Ok(StatusCode::OK)
}

#[derive(Deserialize)]
pub(super) struct SignalQuery {
    #[serde(default = "default_signal")]
    signal: String,
}

fn default_signal() -> String {
    "KILL".into()
}

#[hl_design::adapter]
pub(super) async fn signal(
    State(state): State<DockerState>,
    Path(id): Path<String>,
    Query(query): Query<SignalQuery>,
) -> ApiResult<StatusCode> {
    let exec_id = id
        .parse()
        .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, format!("no such exec: {id}")))?;
    let requested = if query.signal.is_empty() { "KILL" } else { &query.signal };
    let signal = requested
        .parse::<super::container::DockerSignal>()
        .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, format!("unsupported signal {}", query.signal)))?
        .into();
    state
        .containers
        .executions()
        .signal(&exec_id, signal)
        .await
        .map_err(ApiError::container)?;
    Ok(StatusCode::NO_CONTENT)
}
