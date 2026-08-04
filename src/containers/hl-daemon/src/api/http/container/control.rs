use super::*;

#[derive(Default, Deserialize)]
pub(in super::super) struct AttachQuery {
    stdin: Option<String>,
    stdout: Option<String>,
    stderr: Option<String>,
}

pub(in super::super) async fn attach(
    State(state): State<DockerState>,
    Path(id): Path<String>,
    Query(query): Query<AttachQuery>,
    request: Request,
) -> ApiResult<Response> {
    let stdin = bool::from(query.stdin.as_deref().unwrap_or_default().parse::<Flag>()?);
    let stdout = bool::from(query.stdout.as_deref().unwrap_or_default().parse::<Flag>()?);
    let stderr = bool::from(query.stderr.as_deref().unwrap_or_default().parse::<Flag>()?);
    if !stdin && !stdout && !stderr {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "attach requires at least one of stdin, stdout, or stderr",
        ));
    }

    let container = state.containers.inspect(&id).await.map_err(ApiError::container)?;
    let terminal = container.spec.process.console.terminal.is_some();
    let session = state.containers.attach(&id).await.map_err(ApiError::container)?;
    Ok(Connection::new(
        hyper::upgrade::on(request),
        session,
        Streams { stdin, stdout, stderr },
        terminal,
    )
    .spawn())
}

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
    #[serde(default, rename = "t")]
    seconds: Option<u64>,
}

impl TimeoutQuery {
    fn duration(&self, configured_seconds: u64) -> ApiResult<std::time::Duration> {
        const MAXIMUM_SECONDS: u64 = 86_400;
        let seconds = self.seconds.unwrap_or(configured_seconds);
        if seconds > MAXIMUM_SECONDS {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("stop timeout must not exceed {MAXIMUM_SECONDS} seconds"),
            ));
        }
        Ok(std::time::Duration::from_secs(seconds))
    }
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
pub(in super::super) struct KillQuery {
    #[serde(default = "default_signal")]
    signal: String,
}

fn default_signal() -> String {
    "KILL".into()
}

pub(in super::super) async fn kill(
    State(state): State<DockerState>,
    Path(id): Path<String>,
    Query(query): Query<KillQuery>,
) -> ApiResult<StatusCode> {
    let signal = query
        .signal
        .parse::<DockerSignal>()
        .map_err(|_| {
            ApiError::container(ContainerError::InvalidSpec(format!(
                "unsupported signal {}",
                query.signal
            )))
        })?
        .into();
    state
        .containers
        .signal(&id, signal)
        .await
        .map_err(ApiError::container)?;
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
    state
        .containers
        .rename(&id, query.name)
        .await
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(ApiError::container)
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
    #[serde(default = "default_checkpoint_timeout_ms")]
    timeout_ms: u64,
}

const fn default_checkpoint_timeout_ms() -> u64 {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DockerSignal(Signal);

impl FromStr for DockerSignal {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.to_ascii_uppercase();
        match value.strip_prefix("SIG").unwrap_or(&value) {
            "1" | "HUP" => Ok(Self(Signal::Hangup)),
            "2" | "INT" => Ok(Self(Signal::Interrupt)),
            "3" | "QUIT" => Ok(Self(Signal::Quit)),
            "9" | "KILL" => Ok(Self(Signal::Kill)),
            "10" | "USR1" => Ok(Self(Signal::User1)),
            "12" | "USR2" => Ok(Self(Signal::User2)),
            "15" | "TERM" => Ok(Self(Signal::Terminate)),
            _ => Err(value),
        }
    }
}

impl From<DockerSignal> for Signal {
    fn from(value: DockerSignal) -> Self {
        value.0
    }
}

#[cfg(test)]
mod stop_timeout_tests {
    use super::TimeoutQuery;

    #[test]
    fn query_timeout_overrides_durable_default() {
        assert_eq!(TimeoutQuery { seconds: None }.duration(23).unwrap().as_secs(), 23);
        assert_eq!(TimeoutQuery { seconds: Some(0) }.duration(23).unwrap().as_secs(), 0);
        assert_eq!(TimeoutQuery { seconds: Some(7) }.duration(23).unwrap().as_secs(), 7);
        assert!(TimeoutQuery { seconds: Some(86_401) }.duration(23).is_err());
    }
}
