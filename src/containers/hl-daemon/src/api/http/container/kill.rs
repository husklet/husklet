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

/// Docker's Linux signal table: 1..=31 named, 32 and 33 reserved by the C library and
/// therefore unnamed and unaccepted, 34..=64 named relative to `SIGRTMIN`/`SIGRTMAX`.
const NAMES: [&str; 31] = [
    "HUP", "INT", "QUIT", "ILL", "TRAP", "ABRT", "BUS", "FPE", "KILL", "USR1", "SEGV", "USR2", "PIPE", "ALRM", "TERM",
    "STKFLT", "CHLD", "CONT", "STOP", "TSTP", "TTIN", "TTOU", "URG", "XCPU", "XFSZ", "VTALRM", "PROF", "WINCH", "IO",
    "PWR", "SYS",
];
const ALIASES: [(&str, u8); 3] = [("IOT", 6), ("CLD", 17), ("POLL", 29)];
const REALTIME_MINIMUM: u8 = 34;

impl DockerSignal {
    /// Renders the canonical Docker name, matching `kill -l` on a glibc host.
    pub(crate) fn name(signal: Signal) -> String {
        let number = signal.get();
        match number {
            1..=31 => format!("SIG{}", NAMES[number as usize - 1]),
            REALTIME_MINIMUM => "SIGRTMIN".to_owned(),
            Signal::MAXIMUM => "SIGRTMAX".to_owned(),
            35..=49 => format!("SIGRTMIN+{}", number - REALTIME_MINIMUM),
            50..=63 => format!("SIGRTMAX-{}", Signal::MAXIMUM - number),
            _ => format!("SIG{number}"),
        }
    }

    /// Resolves an `RTMIN`/`RTMAX` name. Docker names each real-time signal exactly once,
    /// counting up from `RTMIN` through `RTMIN+15` and down from `RTMAX` to `RTMAX-14`.
    fn realtime(value: &str) -> Option<u8> {
        let (base, limit, sign, anchor) = if let Some(rest) = value.strip_prefix("RTMIN") {
            (REALTIME_MINIMUM, 15, b'+', rest)
        } else {
            (Signal::MAXIMUM, 14, b'-', value.strip_prefix("RTMAX")?)
        };
        if anchor.is_empty() {
            return Some(base);
        }
        // Docker's table has no "+0"/"-0" entry: the base signal is named RTMIN/RTMAX only.
        let offset = anchor
            .get(1..)?
            .parse::<u8>()
            .ok()
            .filter(|offset| (1..=limit).contains(offset))?;
        (anchor.as_bytes()[0] == sign).then_some(())?;
        if sign == b'+' {
            base.checked_add(offset)
        } else {
            base.checked_sub(offset)
        }
    }
}

impl FromStr for DockerSignal {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.to_ascii_uppercase();
        // Docker parses a decimal number before consulting the name table, so "09" and
        // "+9" are signal nine while "0x9" is a name that does not exist.
        let number = if let Ok(number) = value.parse::<i64>() {
            u8::try_from(number).ok().filter(|_| number > 0)
        } else {
            let name = value.strip_prefix("SIG").unwrap_or(&value);
            NAMES
                .iter()
                .position(|known| *known == name)
                .and_then(|index| u8::try_from(index + 1).ok())
                .or_else(|| {
                    ALIASES
                        .iter()
                        .find(|(alias, _)| *alias == name)
                        .map(|(_, number)| *number)
                })
                .or_else(|| Self::realtime(name))
        };
        number
            .filter(|number| *number < 32 || *number >= REALTIME_MINIMUM)
            .and_then(Signal::new)
            .map(Self)
            .ok_or(value)
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

        assert_eq!(omitted.signal().unwrap(), Signal::KILL);
        assert_eq!(empty.signal().unwrap(), Signal::KILL);
    }

    #[test]
    fn supported_forms() {
        for (value, expected) in [("9", Signal::KILL), ("sigterm", Signal::TERMINATE)] {
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
