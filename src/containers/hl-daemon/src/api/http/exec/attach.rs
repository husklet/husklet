use axum::body::to_bytes;
use axum::extract::{OriginalUri, Path, Request, State};
use axum::http::StatusCode;
use axum::response::Response;

use super::super::DockerState;
use super::super::console::Connection;
use super::super::error::{ApiError, ApiResult};
use crate::api::ExecAttach;

const BODY_LIMIT: usize = 64 * 1024;

#[hl_design::adapter]
pub(in crate::api::http) async fn attach(
    State(state): State<DockerState>,
    Path(id): Path<String>,
    OriginalUri(uri): OriginalUri,
    mut request: Request,
) -> ApiResult<Response> {
    let id = id
        .parse()
        .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, format!("no such exec: {id}")))?;
    let upgrade = hyper::upgrade::on(&mut request);
    let bytes = to_bytes(request.into_body(), BODY_LIMIT)
        .await
        .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error.to_string()))?;
    let attach = if bytes.is_empty() {
        ExecAttach::default()
    } else {
        serde_json::from_slice(&bytes).map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error.to_string()))?
    };
    let size = attach
        .size()
        .map_err(|message| ApiError::new(StatusCode::BAD_REQUEST, message))?;
    let executions = state.containers.executions();
    let exec = executions.inspect(&id).await.map_err(ApiError::container)?;
    if attach.tty != exec.spec.process.console.terminal.is_some() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "exec attach Tty must match exec create Tty",
        ));
    }
    let session = executions.attach(&id, size).await.map_err(ApiError::container)?;
    let streams = exec.spec.streams;
    let terminal = exec.spec.process.console.terminal.is_some();
    let mut connection = Connection::new(upgrade, session, streams, terminal)
        .content_type(crate::api::http::query::stream_content_type(&uri, terminal))
        .detach_keys(&exec.spec.detach_keys)?;
    if attach.kill_on_disconnect {
        connection = connection.kill_on_disconnect(executions, id);
    }
    Ok(connection.spawn())
}

#[cfg(test)]
mod tests {
    use super::ExecAttach;

    #[test]
    fn attach_size_requires_a_terminal() {
        let attach = ExecAttach {
            console_size: Some([24, 80]),
            ..ExecAttach::default()
        };
        assert!(attach.size().unwrap_err().contains("Tty=true"));
        assert!(ExecAttach { tty: true, ..attach }.size().unwrap().is_some());
    }
}
