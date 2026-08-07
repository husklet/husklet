use super::*;

#[hl_design::adapter]
pub(in super::super) async fn start(State(state): State<DockerState>, Path(id): Path<String>) -> ApiResult<StatusCode> {
    let container = state.containers.inspect(&id).await.map_err(ApiError::container)?;
    state.containers.start(&id).await.map_err(ApiError::container)?;
    state.events.volumes("mount", &container);
    Ok(StatusCode::NO_CONTENT)
}

pub(in super::super) async fn resize(
    State(state): State<DockerState>,
    Path(id): Path<String>,
    Query(query): Query<Resize>,
) -> ApiResult<StatusCode> {
    state
        .containers
        .resize(&id, query.size()?)
        .await
        .map_err(ApiError::container)?;
    Ok(StatusCode::OK)
}

#[derive(Default, Deserialize)]
pub(in super::super) struct TimeoutQuery {
    #[serde(default, rename = "t", deserialize_with = "crate::api::http::query::optional_u64")]
    seconds: Option<u64>,
    #[serde(default)]
    signal: Option<String>,
}

impl TimeoutQuery {
    // ApiResult keeps this uniform with the other query accessors.
    #[allow(clippy::unnecessary_wraps)]
    fn duration(&self, configured_seconds: u64) -> ApiResult<std::time::Duration> {
        let seconds = self.seconds.unwrap_or(configured_seconds);
        Ok(std::time::Duration::from_secs(seconds))
    }

    fn validate_signal(&self, configured: Signal) -> ApiResult<()> {
        let Some(value) = self.signal.as_deref().filter(|value| !value.is_empty()) else {
            return Ok(());
        };
        let requested = value
            .parse::<DockerSignal>()
            .map(Signal::from)
            .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, format!("unsupported stop signal {value}")))?;
        if requested != configured {
            return Err(ApiError::new(
                StatusCode::NOT_IMPLEMENTED,
                "custom stop signal override is not implemented",
            ));
        }
        Ok(())
    }
}

#[derive(Default, Deserialize)]
pub(in super::super) struct LegacyTimeoutQuery {
    #[serde(default, rename = "t", deserialize_with = "crate::api::http::query::optional_u64")]
    seconds: Option<u64>,
}

pub(in super::super) async fn legacy_stop(
    State(state): State<DockerState>,
    Path(id): Path<String>,
    Query(query): Query<LegacyTimeoutQuery>,
) -> ApiResult<StatusCode> {
    stop(
        State(state),
        Path(id),
        Query(TimeoutQuery {
            seconds: query.seconds,
            signal: None,
        }),
    )
    .await
}

pub(in super::super) async fn legacy_restart(
    State(state): State<DockerState>,
    Path(id): Path<String>,
    Query(query): Query<LegacyTimeoutQuery>,
) -> ApiResult<StatusCode> {
    restart(
        State(state),
        Path(id),
        Query(TimeoutQuery {
            seconds: query.seconds,
            signal: None,
        }),
    )
    .await
}

pub(in super::super) async fn stop(
    State(state): State<DockerState>,
    Path(id): Path<String>,
    Query(query): Query<TimeoutQuery>,
) -> ApiResult<StatusCode> {
    let container = state.containers.inspect(&id).await.map_err(ApiError::container)?;
    if !container.state.is_active() {
        return Ok(StatusCode::NO_CONTENT);
    }
    query.validate_signal(container.spec.stop_signal)?;
    match state
        .containers
        .stop(&id, query.duration(container.spec.stop_timeout_seconds)?)
        .await
    {
        Ok(_) | Err(ContainerError::InvalidState { .. }) => Ok(StatusCode::NO_CONTENT),
        Err(error) => Err(ApiError::container(error)),
    }
}

pub(in super::super) async fn restart(
    State(state): State<DockerState>,
    Path(id): Path<String>,
    Query(query): Query<TimeoutQuery>,
) -> ApiResult<StatusCode> {
    let container = state.containers.inspect(&id).await.map_err(ApiError::container)?;
    if container.state.is_active() {
        query.validate_signal(container.spec.stop_signal)?;
        match state
            .containers
            .stop(&id, query.duration(container.spec.stop_timeout_seconds)?)
            .await
        {
            Ok(_) | Err(ContainerError::InvalidState { .. }) => {}
            Err(error) => return Err(ApiError::container(error)),
        }
    }
    state.containers.start(&id).await.map_err(ApiError::container)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub(in super::super) struct RenameQuery {
    name: String,
}

pub(in super::super) async fn rename(
    State(state): State<DockerState>,
    Path(id): Path<String>,
    Query(query): Query<RenameQuery>,
) -> ApiResult<StatusCode> {
    let container = state
        .containers
        .rename(&id, query.name)
        .await
        .map_err(ApiError::container)?;
    state.events.container("rename", &container);
    Ok(StatusCode::NO_CONTENT)
}

#[hl_design::adapter]
pub(in super::super) async fn pause(State(state): State<DockerState>, Path(id): Path<String>) -> ApiResult<StatusCode> {
    state
        .containers
        .pause(&id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(ApiError::container)
}

#[hl_design::adapter]
pub(in super::super) async fn unpause(
    State(state): State<DockerState>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    state
        .containers
        .unpause(&id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(ApiError::container)
}

#[derive(Deserialize)]
pub(in super::super) struct CheckpointQuery {
    #[serde(default = "checkpoint_timeout")]
    timeout_ms: u64,
}

const fn checkpoint_timeout() -> u64 {
    30_000
}

#[hl_design::adapter]
pub(in super::super) async fn checkpoint(
    State(state): State<DockerState>,
    Path(id): Path<String>,
    Query(query): Query<CheckpointQuery>,
) -> ApiResult<StatusCode> {
    if query.timeout_ms == 0 || query.timeout_ms > 300_000 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "checkpoint timeout_ms must be between 1 and 300000",
        ));
    }
    state
        .containers
        .checkpoint(&id, std::time::Duration::from_millis(query.timeout_ms))
        .await
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(ApiError::container)
}

#[cfg(test)]
mod stop_timeout_tests {
    use super::*;

    #[test]
    fn timeout_contract() {
        assert_eq!(
            TimeoutQuery {
                seconds: None,
                signal: None,
            }
            .duration(23)
            .unwrap()
            .as_secs(),
            23
        );
        assert_eq!(
            TimeoutQuery {
                seconds: Some(0),
                signal: None,
            }
            .duration(23)
            .unwrap()
            .as_secs(),
            0
        );
        assert_eq!(
            TimeoutQuery {
                seconds: Some(7),
                signal: None,
            }
            .duration(23)
            .unwrap()
            .as_secs(),
            7
        );
        assert_eq!(
            TimeoutQuery {
                seconds: Some(86_401),
                signal: None,
            }
            .duration(23)
            .unwrap()
            .as_secs(),
            86_401
        );
    }

    #[test]
    fn empty_timeout_is_omitted_for_legacy_and_current_routes() {
        for path in [
            "/containers/id/stop",
            "/containers/id/stop?t=",
            "/v1.43/containers/id/restart?t=",
        ] {
            let uri = path.parse().unwrap();
            let Query(query) = Query::<TimeoutQuery>::try_from_uri(&uri).unwrap();
            assert_eq!(query.duration(23).unwrap().as_secs(), 23, "{path}");
        }
        let uri = "/v1.41/containers/id/stop?t=".parse().unwrap();
        let Query(query) = Query::<LegacyTimeoutQuery>::try_from_uri(&uri).unwrap();
        assert_eq!(query.seconds, None);
        for path in [
            "/containers/id/stop?t=-1",
            "/containers/id/stop?t=invalid",
            "/containers/id/stop?t=18446744073709551616",
        ] {
            let uri = path.parse().unwrap();
            assert!(Query::<TimeoutQuery>::try_from_uri(&uri).is_err(), "{path}");
        }
    }

    #[test]
    fn stop_signal_contract() {
        for signal in [None, Some(""), Some("TERM"), Some("SIGTERM"), Some("15")] {
            let query = TimeoutQuery {
                seconds: None,
                signal: signal.map(str::to_owned),
            };
            query.validate_signal(Signal::Terminate).unwrap();
        }

        let malformed = TimeoutQuery {
            seconds: None,
            signal: Some("SIGBOGUS".into()),
        };
        assert_eq!(
            malformed.validate_signal(Signal::Terminate).unwrap_err().status,
            StatusCode::BAD_REQUEST
        );

        let unsupported = TimeoutQuery {
            seconds: None,
            signal: Some("KILL".into()),
        };
        assert_eq!(
            unsupported.validate_signal(Signal::Terminate).unwrap_err().status,
            StatusCode::NOT_IMPLEMENTED
        );
    }

    #[test]
    fn legacy_timeout_ignores_newer_signal_query() {
        let uri = "/v1.41/containers/id/stop?t=7&signal=KILL".parse().unwrap();
        let Query(query) = Query::<LegacyTimeoutQuery>::try_from_uri(&uri).unwrap();
        assert_eq!(query.seconds, Some(7));
    }
}
