use super::*;

#[derive(Default, Deserialize)]
pub(in super::super) struct LogsQuery {
    pub(super) stdout: Option<String>,
    pub(super) stderr: Option<String>,
    pub(super) follow: Option<String>,
    pub(super) since: Option<String>,
    pub(super) until: Option<String>,
    pub(super) timestamps: Option<String>,
    pub(super) tail: Option<String>,
    pub(super) details: Option<String>,
    #[serde(flatten)]
    pub(super) unsupported: std::collections::BTreeMap<String, String>,
}

pub(in super::super) async fn logs(
    State(state): State<DockerState>,
    Path(id): Path<String>,
    Query(query): Query<LogsQuery>,
) -> ApiResult<Response> {
    let options = LogOptions::try_from(query)?;
    if !options.streams.stdout && !options.streams.stderr {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "logs require stdout=true or stderr=true",
        ));
    }
    let container = state.containers.inspect(&id).await.map_err(ApiError::container)?;
    let terminal = container.spec.process.console.terminal.is_some();
    let mut session = state.containers.follow(&id).await.map_err(ApiError::container)?;
    let history = options.replay(session.history().await.map_err(ApiError::container)?);
    let (sender, receiver) = tokio::sync::mpsc::channel(16);
    tokio::spawn(async move {
        let mut encoder = crate::api::model::LogEncoder::new(options.timestamps, terminal);
        for entry in history {
            if sender.send(encoder.frame(&entry)).await.is_err() {
                return;
            }
        }
        if !options.follow || options.until_ms.is_some_and(|until| until <= now_ms()) {
            return;
        }
        loop {
            let entry = if let Some(until) = options.until_ms {
                let remaining = std::time::Duration::from_millis(until.saturating_sub(now_ms()));
                tokio::select! {
                    () = sender.closed() => return,
                    () = tokio::time::sleep(remaining) => return,
                    result = session.next() => result,
                }
            } else {
                tokio::select! {
                    () = sender.closed() => return,
                    result = session.next() => result,
                }
            };
            let Ok(Some(entry)) = entry else { return };
            if options.accepts(&entry) && sender.send(encoder.frame(&entry)).await.is_err() {
                return;
            }
        }
    });
    let body = Body::from_stream(futures_util::stream::unfold(receiver, |mut receiver| async move {
        receiver
            .recv()
            .await
            .map(|bytes| (Ok::<_, std::convert::Infallible>(bytes), receiver))
    }));
    Ok((
        [(axum::http::header::CONTENT_TYPE, "application/vnd.docker.raw-stream")],
        body,
    )
        .into_response())
}

impl TryFrom<LogsQuery> for LogOptions {
    type Error = ApiError;

    fn try_from(query: LogsQuery) -> ApiResult<Self> {
        if let Some(name) = query.unsupported.keys().next() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("unsupported logs option {name:?}"),
            ));
        }
        if bool::from(query.details.as_deref().unwrap_or_default().parse::<Flag>()?) {
            return Err(ApiError::new(
                StatusCode::NOT_IMPLEMENTED,
                "log details require container attribute logging",
            ));
        }
        let tail = match query.tail.as_deref().unwrap_or("all") {
            "" | "all" => None,
            value => match value.parse::<i64>() {
                Ok(value) if value < 0 => None,
                Ok(value) => Some(
                    usize::try_from(value)
                        .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "log tail exceeds host range"))?,
                ),
                Err(_) => None,
            },
        };
        Ok(Self {
            follow: bool::from(query.follow.as_deref().unwrap_or_default().parse::<Flag>()?),
            streams: LogStreams {
                stdout: bool::from(query.stdout.as_deref().unwrap_or_default().parse::<Flag>()?),
                stderr: bool::from(query.stderr.as_deref().unwrap_or_default().parse::<Flag>()?),
            },
            since_ms: query
                .since
                .as_deref()
                .map(str::parse::<LogTime>)
                .transpose()?
                .map(u64::from),
            until_ms: query
                .until
                .as_deref()
                .map(str::parse::<LogTime>)
                .transpose()?
                .map(u64::from),
            timestamps: bool::from(query.timestamps.as_deref().unwrap_or_default().parse::<Flag>()?),
            tail,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LogTime(u64);

impl LogTime {
    fn from_numeric(value: &str) -> Option<Self> {
        let (seconds, fraction) = value.split_once('.').map_or((value, ""), |parts| parts);
        if seconds.is_empty()
            || !seconds.bytes().all(|byte| byte.is_ascii_digit())
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        let seconds = seconds.parse::<u64>().ok()?;
        let mut milliseconds = fraction
            .bytes()
            .take(3)
            .fold(0_u64, |value, digit| value * 10 + u64::from(digit - b'0'));
        for _ in fraction.len().min(3)..3 {
            milliseconds *= 10;
        }
        seconds.checked_mul(1_000)?.checked_add(milliseconds).map(Self)
    }
}

impl FromStr for LogTime {
    type Err = ApiError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if let Some(time) = Self::from_numeric(value) {
            return Ok(time);
        }
        let timestamp = chrono::DateTime::parse_from_rfc3339(value)
            .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, format!("invalid log time {value:?}")))?
            .timestamp_millis();
        u64::try_from(timestamp)
            .map(Self)
            .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "log time predates Unix epoch"))
    }
}

impl From<LogTime> for u64 {
    fn from(value: LogTime) -> Self {
        value.0
    }
}

fn now_ms() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Flag(bool);

impl FromStr for Flag {
    type Err = ApiError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "1" | "true" | "True" | "TRUE" => Ok(Self(true)),
            "0" | "false" | "False" | "FALSE" | "" => Ok(Self(false)),
            value => Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("invalid boolean {value:?}"),
            )),
        }
    }
}

impl From<Flag> for bool {
    fn from(value: Flag) -> Self {
        value.0
    }
}
