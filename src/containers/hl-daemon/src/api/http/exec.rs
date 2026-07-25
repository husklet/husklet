use axum::body::{to_bytes, Body};
use axum::extract::{Path, Query, Request, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::Json;
use hl_container::{ExecSpec, ExecState, ExitStatus, Streams};
use serde::Deserialize;

use super::console::{Connection, DetachKeys, Resize};
use super::error::{ApiError, ApiResult};
use super::DockerState;
use crate::api::{ExecConfig, ExecCreated, ExecInspect, ExecOpen, ExecProcess, ExecStart, Wait};

const BODY_LIMIT: usize = 64 * 1024;

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
pub(super) async fn start(
    State(state): State<DockerState>,
    Path(id): Path<String>,
    mut request: Request,
) -> ApiResult<Response> {
    let id = id
        .parse()
        .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, format!("no such exec: {id}")))?;
    let upgrade = hyper::upgrade::on(&mut request);
    let bytes = to_bytes(request.into_body(), BODY_LIMIT)
        .await
        .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error.to_string()))?;
    let start = if bytes.is_empty() {
        ExecStart::default()
    } else {
        serde_json::from_slice(&bytes)
            .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error.to_string()))?
    };
    let size = start.size().map_err(|message| {
        let status = if message.contains("not implemented") || message.starts_with("unsupported") {
            StatusCode::NOT_IMPLEMENTED
        } else {
            StatusCode::BAD_REQUEST
        };
        ApiError::new(status, message)
    })?;
    let exec = state
        .containers
        .executions()
        .inspect(&id)
        .await
        .map_err(ApiError::container)?;
    if start.tty != exec.spec.process.console.terminal.is_some() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "exec start Tty must match exec create Tty",
        ));
    }
    let executions = state.containers.executions();
    let session = match size {
        Some(size) => executions.start_at(&id, size).await,
        None => executions.start(&id).await,
    }
    .map_err(ApiError::container)?;
    if start.detach {
        return Ok(Response::new(Body::empty()));
    }
    let streams = exec.spec.streams;
    let mut connection = Connection::new(
        upgrade,
        session,
        streams,
        exec.spec.process.console.terminal.is_some(),
    )
    .detach_keys(&exec.spec.detach_keys)?;
    if start.kill_on_disconnect {
        connection = connection.kill_on_disconnect(executions, id);
    }
    Ok(connection.spawn())
}

#[hl_design::adapter]
pub(super) async fn inspect(
    State(state): State<DockerState>,
    Path(id): Path<String>,
) -> ApiResult<Json<ExecInspect>> {
    let exec_id = id
        .parse()
        .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, format!("no such exec: {id}")))?;
    let exec = state
        .containers
        .executions()
        .inspect(&exec_id)
        .await
        .map_err(ApiError::container)?;
    let (running, exit_code, pid) = match exec.state {
        ExecState::Created => (false, 0, 0),
        ExecState::Running { process_id, .. } => (
            true,
            0,
            i64::try_from(process_id).map_err(|_| {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "exec PID exceeds i64")
            })?,
        ),
        ExecState::Exited { result, .. } => {
            let code = match result {
                ExitStatus::Code(code) => code,
                ExitStatus::Signal(signal) => 128 + signal,
                ExitStatus::Fault { status, .. } => status,
            };
            (false, i64::from(code), 0)
        }
    };
    let process = exec.spec.process;
    Ok(Json(ExecInspect {
        id: exec.id.to_string(),
        container_id: exec.container.to_string(),
        running,
        exit_code,
        pid,
        can_remove: !running,
        detach_keys: exec.spec.detach_keys,
        open: ExecOpen {
            stdin: exec.spec.streams.stdin,
            stdout: exec.spec.streams.stdout,
            stderr: exec.spec.streams.stderr,
        },
        process: ExecProcess {
            arguments: process.args,
            entrypoint: process.program,
            privileged: exec.spec.privileged,
            tty: process.console.terminal.is_some(),
            user: exec.spec.user,
        },
    }))
}

#[hl_design::adapter]
pub(super) async fn wait(
    State(state): State<DockerState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Wait>> {
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
pub(super) async fn remove(
    State(state): State<DockerState>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
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
    let signal = query
        .signal
        .parse::<super::container::DockerSignal>()
        .map_err(|_| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("unsupported signal {}", query.signal),
            )
        })?
        .into();
    state
        .containers
        .executions()
        .signal(&exec_id, signal)
        .await
        .map_err(ApiError::container)?;
    Ok(StatusCode::NO_CONTENT)
}
