use super::*;

#[derive(Deserialize)]
pub(in super::super) struct KillQuery {
    #[serde(default = "default_signal")]
    signal: String,
}

fn default_signal() -> String {
    "KILL".into()
}

impl KillQuery {
    fn signal(&self) -> ApiResult<Signal> {
        let value = if self.signal.is_empty() { "KILL" } else { &self.signal };
        value.parse::<DockerSignal>().map(Signal::from).map_err(|_| {
            ApiError::container(ContainerError::InvalidSpec(format!(
                "unsupported signal {}",
                self.signal
            )))
        })
    }
}

pub(in super::super) async fn kill(
    State(state): State<DockerState>,
    Path(id): Path<String>,
    Query(query): Query<KillQuery>,
) -> ApiResult<StatusCode> {
    let signal = query.signal()?;
    state
        .containers
        .signal(&id, signal)
        .await
        .map_err(ApiError::container)?;
    Ok(StatusCode::NO_CONTENT)
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
mod tests {
    use super::*;

    #[test]
    fn default_signal() {
        let omitted: KillQuery = serde_json::from_value(serde_json::json!({})).unwrap();
        let empty: KillQuery = serde_json::from_value(serde_json::json!({ "signal": "" })).unwrap();

        assert_eq!(omitted.signal().unwrap(), Signal::Kill);
        assert_eq!(empty.signal().unwrap(), Signal::Kill);
    }

    #[test]
    fn supported_forms() {
        for (value, expected) in [("9", Signal::Kill), ("sigterm", Signal::Terminate)] {
            let query = KillQuery { signal: value.into() };
            assert_eq!(query.signal().unwrap(), expected);
        }
        let whitespace = KillQuery {
            signal: " SIGKILL ".into(),
        };
        assert_eq!(whitespace.signal().unwrap_err().status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn invalid_signal() {
        for value in ["SIGBOGUS", "0"] {
            let query = KillQuery { signal: value.into() };
            assert_eq!(query.signal().unwrap_err().status, StatusCode::BAD_REQUEST);
        }
    }
}
