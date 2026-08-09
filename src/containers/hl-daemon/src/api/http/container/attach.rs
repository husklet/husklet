use super::*;
use axum::extract::OriginalUri;

#[derive(Default, Deserialize)]
pub(in super::super) struct AttachQuery {
    #[serde(default, rename = "detachKeys")]
    detach_keys: String,
    logs: Option<String>,
    stream: Option<String>,
    stdin: Option<String>,
    stdout: Option<String>,
    stderr: Option<String>,
}

impl AttachQuery {
    fn validate_detach_keys(&self) -> ApiResult<()> {
        super::super::console::DetachKeys::parse(&self.detach_keys).map(|_| ())
    }

    fn output(&self) -> (bool, bool) {
        (Self::flag(self.logs.as_deref()), Self::flag(self.stream.as_deref()))
    }

    fn streams(&self) -> Streams {
        Streams {
            stdin: Self::flag(self.stdin.as_deref()),
            stdout: Self::flag(self.stdout.as_deref()),
            stderr: Self::flag(self.stderr.as_deref()),
        }
    }

    fn flag(value: Option<&str>) -> bool {
        crate::api::http::query::parse_flag(value.unwrap_or_default())
    }

    fn content_type(uri: &axum::http::Uri, terminal: bool) -> &'static str {
        crate::api::http::query::stream_content_type(uri, terminal)
    }
}

pub(in super::super) async fn attach(
    State(state): State<DockerState>,
    Path(id): Path<String>,
    Query(query): Query<AttachQuery>,
    OriginalUri(uri): OriginalUri,
    request: Request,
) -> ApiResult<Response> {
    query.validate_detach_keys()?;
    let (logs, stream) = query.output();
    let container = state.containers.inspect(&id).await.map_err(ApiError::container)?;
    let terminal = container.spec.process.console.terminal.is_some();
    let session = if logs || !stream {
        state.containers.follow(&id).await
    } else {
        state.containers.attach(&id).await
    }
    .map_err(ApiError::container)?;
    Ok(
        Connection::new(hyper::upgrade::on(request), session, query.streams(), terminal)
            .content_type(AttachQuery::content_type(&uri, terminal))
            .detach_keys(&query.detach_keys)?
            .output(logs, stream)
            .spawn(),
    )
}

pub(in super::super) async fn websocket(
    State(state): State<DockerState>,
    Path(id): Path<String>,
    Query(query): Query<AttachQuery>,
) -> ApiResult<StatusCode> {
    query.validate_detach_keys()?;
    state.containers.inspect(&id).await.map_err(ApiError::container)?;
    Err(ApiError::new(
        StatusCode::NOT_IMPLEMENTED,
        "WebSocket container attach is not implemented; use the Docker raw-stream attach endpoint",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_contract() {
        let query: AttachQuery = serde_json::from_value(serde_json::json!({
            "detachKeys": "ctrl-x,x",
            "logs": "true",
            "stream": "1",
            "stdin": "1"
        }))
        .unwrap();
        assert_eq!(query.detach_keys, "ctrl-x,x");
        query.validate_detach_keys().unwrap();
        assert_eq!(query.output(), (true, true));
        let streams = query.streams();
        assert!(streams.stdin);
        assert!(!streams.stdout);
        assert!(!streams.stderr);

        let missing: AttachQuery = serde_json::from_value(serde_json::json!({ "stdout": "1" })).unwrap();
        assert_eq!(missing.detach_keys, "");
        missing.validate_detach_keys().unwrap();
        assert_eq!(missing.output(), (false, false));

        let invalid: AttachQuery = serde_json::from_value(serde_json::json!({ "detachKeys": "ctrl-!" })).unwrap();
        assert_eq!(
            invalid.validate_detach_keys().unwrap_err().status,
            StatusCode::BAD_REQUEST
        );

        for value in ["", "0", "no", "false", "none", " FALSE "] {
            assert!(!AttachQuery::flag(Some(value)), "accepted false value {value:?}");
        }
        assert!(AttachQuery::flag(Some(" yes ")));
        assert!(AttachQuery::flag(Some("one")));
    }

    #[test]
    fn media_type() {
        for path in [
            "/containers/id/attach",
            "/v1.42/containers/id/attach",
            "/v1.43/containers/id/attach",
        ] {
            let uri = path.parse().unwrap();
            assert_eq!(
                AttachQuery::content_type(&uri, false),
                "application/vnd.docker.multiplexed-stream"
            );
            assert_eq!(
                AttachQuery::content_type(&uri, true),
                "application/vnd.docker.raw-stream"
            );
        }
        let legacy = "/v1.41/containers/id/attach".parse().unwrap();
        assert_eq!(
            AttachQuery::content_type(&legacy, false),
            "application/vnd.docker.raw-stream"
        );
    }
}
