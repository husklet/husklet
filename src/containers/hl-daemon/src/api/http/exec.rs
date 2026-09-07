mod attach;
mod inspect;
mod start;

pub(super) use attach::attach;
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
use crate::api::{ExecConfig, ExecCreated, ExecLifetime, ExecNetwork, Wait};

#[derive(Deserialize)]
pub(super) struct ListQuery {
    limit: Option<u16>,
}

pub(super) async fn list(
    State(state): State<DockerState>,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<crate::api::ExecCatalogue>> {
    let limit = usize::from(query.limit.unwrap_or(1024).min(1024));
    let mut records = state
        .containers
        .executions()
        .list()
        .await
        .map_err(ApiError::container)?;
    let truncated = records.len() > limit;
    records.truncate(limit);
    let executions = records
        .into_iter()
        .map(inspect::model)
        .collect::<ApiResult<Vec<_>>>()?
        .into_iter()
        .map(|Json(value)| value)
        .collect();
    Ok(Json(crate::api::ExecCatalogue { executions, truncated }))
}

pub(super) async fn logs(
    State(state): State<DockerState>,
    Path(id): Path<String>,
) -> ApiResult<Json<crate::api::ExecOutput>> {
    let exec_id = id
        .parse()
        .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, format!("no such exec: {id}")))?;
    let logs = state
        .containers
        .executions()
        .logs(&exec_id)
        .await
        .map_err(ApiError::container)?;
    Ok(Json(crate::api::ExecOutput {
        stdout: logs.stdout,
        stderr: logs.stderr,
    }))
}

#[derive(Deserialize)]
pub(super) struct OutputQuery {
    after: u64,
    limit: u16,
}

fn finish_output_page(
    after: u64,
    next: u64,
    more: bool,
    running: bool,
    tail: u64,
) -> Result<(bool, bool), &'static str> {
    if after > tail {
        return Err("exec output cursor is past the durable tail");
    }
    if next < after || next > tail {
        return Err("exec output page cursor is outside the durable journal range");
    }
    let more = more || tail > next;
    Ok((more, !running && !more))
}

pub(super) async fn output(
    State(state): State<DockerState>,
    Path(id): Path<String>,
    Query(query): Query<OutputQuery>,
) -> ApiResult<Json<crate::api::ExecOutputPage>> {
    if query.limit == 0 || query.limit > 16 {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "exec output limit must be between 1 and 16"));
    }
    let exec_id = id
        .parse()
        .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, format!("no such exec: {id}")))?;
    let mut entries = state
        .containers
        .executions()
        .output_after(&exec_id, query.after, usize::from(query.limit) + 1)
        .await
        .map_err(ApiError::container)?;
    let more = entries.len() > usize::from(query.limit);
    entries.truncate(usize::from(query.limit));
    let next = entries.last().map_or(query.after, |entry| entry.sequence);
    let gap = entries
        .first()
        .is_some_and(|entry| query.after.checked_add(1) != Some(entry.sequence));
    let running = state
        .containers
        .executions()
        .inspect(&exec_id)
        .await
        .map_err(ApiError::container)?
        .state
        .is_active();
    let tail = state
        .containers
        .executions()
        .output_cursor(&exec_id)
        .await
        .map_err(ApiError::container)?;
    let (more, eof) = finish_output_page(query.after, next, more, running, tail)
        .map_err(|detail| ApiError::new(StatusCode::BAD_REQUEST, detail))?;
    Ok(Json(crate::api::ExecOutputPage {
        entries: entries
            .into_iter()
            .map(|entry| crate::api::ExecOutputEntry {
                sequence: entry.sequence,
                timestamp_ms: entry.timestamp_ms,
                stream: entry.stream,
                bytes: entry.bytes,
            })
            .collect(),
        next,
        more,
        eof,
        gap,
    }))
}

#[cfg(test)]
mod output_tests {
    use super::finish_output_page;

    #[test]
    fn stopped_tail_reconciliation_exposes_a_final_append_and_then_eof() {
        assert_eq!(finish_output_page(4, 4, false, false, 5), Ok((true, false)));
        assert_eq!(finish_output_page(4, 5, false, false, 5), Ok((false, true)));
        assert_eq!(finish_output_page(4, 4, false, true, 5), Ok((true, false)));
    }

    #[test]
    fn stopped_tail_reconciliation_rejects_a_future_cursor() {
        assert_eq!(
            finish_output_page(6, 6, false, false, 5),
            Err("exec output cursor is past the durable tail")
        );
        assert_eq!(
            finish_output_page(6, 6, false, true, 5),
            Err("exec output cursor is past the durable tail")
        );
        assert_eq!(
            finish_output_page(4, 6, false, false, 5),
            Err("exec output page cursor is outside the durable journal range")
        );
    }
}

#[hl_design::adapter]
pub(super) async fn create(
    State(state): State<DockerState>,
    Path(container): Path<String>,
    Query(query): Query<GenerationQuery>,
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
    let parent = if let Some(generation) = query.generation {
        state.containers.inspect_if_generation(&container, query.container_id.as_deref().unwrap_or(&container), generation).await
    } else { state.containers.inspect(&container).await }.map_err(ApiError::container)?;
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
        .user(config.user)
        .lifetime(match config.lifetime {
            ExecLifetime::Persisted => hl_container::ExecLifetime::Persisted,
            ExecLifetime::Live => hl_container::ExecLifetime::Live,
            ExecLifetime::Ephemeral => hl_container::ExecLifetime::Ephemeral,
        })
        .network(match config.network {
            ExecNetwork::Container => hl_container::ExecNetwork::Container,
            ExecNetwork::Isolated => hl_container::ExecNetwork::Isolated,
        });
    let spec = if config.native {
        spec.execution(hl_container::Execution::native(false))
    } else {
        spec
    };
    let executions = state.containers.executions();
    let exec = if let Some(generation) = query.generation {
        executions.create_if_generation(&container, query.container_id.as_deref().unwrap_or(&container), generation, spec).await
    } else { executions.create(&container, spec).await }.map_err(ApiError::container)?;
    Ok((
        StatusCode::CREATED,
        Json(ExecCreated {
            id: exec.id.to_string(),
        }),
    ))
}

#[derive(Default, Deserialize)]
pub(super) struct GenerationQuery { generation: Option<u64>, container_id: Option<String> }

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
