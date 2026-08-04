use axum::body::{Body, to_bytes};
use axum::extract::{OriginalUri, Path, Request, State};
use axum::http::StatusCode;
use axum::response::Response;

use super::super::DockerState;
use super::super::console::Connection;
use super::super::error::{ApiError, ApiResult};
use crate::api::ExecStart;

const BODY_LIMIT: usize = 64 * 1024;

#[hl_design::adapter]
pub(in crate::api::http) async fn start(
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
    let start = if bytes.is_empty() {
        ExecStart::default()
    } else {
        serde_json::from_slice(&bytes).map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error.to_string()))?
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
    let terminal = exec.spec.process.console.terminal.is_some();
    let mut connection = Connection::new(upgrade, session, streams, terminal)
        .content_type(start.content_type(&uri))
        .detach_keys(&exec.spec.detach_keys)?;
    if start.kill_on_disconnect {
        connection = connection.kill_on_disconnect(executions, id);
    }
    Ok(connection.spawn())
}

impl ExecStart {
    fn content_type(&self, uri: &axum::http::Uri) -> &'static str {
        let supports_multiplexed_type = uri
            .path()
            .strip_prefix("/v1.")
            .and_then(|path| path.split('/').next())
            .and_then(|minor| minor.parse::<u16>().ok())
            .is_none_or(|minor| minor >= 42);
        if supports_multiplexed_type && !self.tty {
            "application/vnd.docker.multiplexed-stream"
        } else {
            "application/vnd.docker.raw-stream"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ExecStart;

    #[test]
    fn media() {
        let pipe = ExecStart::default();
        let terminal = ExecStart {
            tty: true,
            ..ExecStart::default()
        };
        for path in ["/exec/id/start", "/v1.42/exec/id/start", "/v1.43/exec/id/start"] {
            let uri = path.parse().unwrap();
            assert_eq!(pipe.content_type(&uri), "application/vnd.docker.multiplexed-stream");
            assert_eq!(terminal.content_type(&uri), "application/vnd.docker.raw-stream");
        }
        let legacy = "/v1.41/exec/id/start".parse().unwrap();
        assert_eq!(pipe.content_type(&legacy), "application/vnd.docker.raw-stream");
    }
}
