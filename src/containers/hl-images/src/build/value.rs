use std::collections::BTreeMap;

use super::instruction::{Assignments, Words, WorkingDirectory};
use super::{Error, History, Result};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Command {
    Exec(Vec<String>),
    Shell(String),
}

impl std::str::FromStr for Command {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        let value = value.trim_start();
        let json_form = value.starts_with('[') && !value.as_bytes().get(1).is_some_and(u8::is_ascii_whitespace);
        if json_form {
            return serde_json::from_str(value)
                .map(Self::Exec)
                .map_err(|error| Error::MalformedOci(format!("invalid command: {error}")));
        }
        Ok(Self::Shell(value.into()))
    }
}

impl From<Command> for Vec<String> {
    fn from(command: Command) -> Self {
        match command {
            Command::Exec(arguments) => arguments,
            Command::Shell(command) => vec!["/bin/sh".into(), "-c".into(), command],
        }
    }
}

/// Ordered Docker commit configuration changes.
pub struct Changes<'a> {
    values: &'a [String],
}

impl<'a> Changes<'a> {
    #[must_use]
    pub fn new(values: &'a [String]) -> Self {
        Self { values }
    }

    /// Apply changes to inherited image metadata.
    ///
    /// # Errors
    /// Returns a validation error for malformed or unsupported instructions.
    pub fn apply(&self, metadata: &mut crate::Metadata) -> Result<()> {
        for change in self.values {
            let (name, value) = change
                .trim()
                .split_once(char::is_whitespace)
                .ok_or_else(|| Error::MalformedOci(format!("commit change {change:?} has no value")))?;
            let value = value.trim();
            let name = name.to_ascii_uppercase();
            match name.as_str() {
                "CMD" => metadata.runtime.command = value.parse::<Command>()?.into(),
                "ENTRYPOINT" => metadata.runtime.entrypoint = value.parse::<Command>()?.into(),
                "ENV" => metadata.runtime.environment.extend(Assignments::new(value).parse()?),
                "LABEL" => metadata.labels.extend(Assignments::new(value).parse()?),
                "WORKDIR" => {
                    metadata.runtime.working_directory =
                        WorkingDirectory::new(&metadata.runtime.working_directory).resolve(value)?;
                }
                "USER" => {
                    if value.is_empty() {
                        return Err(Error::MalformedOci("USER must not be empty".into()));
                    }
                    metadata.runtime.user = value.into();
                }
                "EXPOSE" => {
                    metadata
                        .exposed_ports
                        .extend(Words::new(value).parse().into_iter().map(|port| {
                            if port.contains('/') {
                                port
                            } else {
                                format!("{port}/tcp")
                            }
                        }));
                }
                "VOLUME" => {
                    let volumes = if value.starts_with('[') {
                        serde_json::from_str(value)
                            .map_err(|error| Error::MalformedOci(format!("invalid VOLUME: {error}")))?
                    } else {
                        Words::new(value).parse()
                    };
                    metadata.volumes.extend(volumes);
                }
                "ONBUILD" => {
                    let nested = value.split_whitespace().next().unwrap_or_default().to_ascii_uppercase();
                    if value.is_empty() || matches!(nested.as_str(), "FROM" | "ONBUILD") {
                        return Err(Error::MalformedOci("invalid ONBUILD trigger".into()));
                    }
                    metadata.onbuild.push(value.into());
                }
                "STOPSIGNAL" if !value.is_empty() && !value.contains(char::is_whitespace) => {
                    metadata.stop_signal = Some(value.into());
                }
                "STOPSIGNAL" => return Err(Error::MalformedOci("invalid STOPSIGNAL".into())),
                _ => return Err(Error::MalformedOci(format!("unsupported commit change {name}"))),
            }
            metadata.history.push(History::change(change));
        }
        metadata.runtime.validate()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Healthcheck {
    test: Vec<String>,
    interval: Option<u64>,
    timeout: Option<u64>,
    start_period: Option<u64>,
    start_interval: Option<u64>,
    retries: Option<u64>,
}

impl std::str::FromStr for Healthcheck {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        let mut value = value.trim();
        let mut health = Self {
            test: Vec::new(),
            interval: None,
            timeout: None,
            start_period: None,
            start_interval: None,
            retries: None,
        };
        while let Some(option) = value.strip_prefix("--") {
            let end = option
                .find(char::is_whitespace)
                .ok_or_else(|| Error::MalformedOci("HEALTHCHECK option requires a command".into()))?;
            let (option, rest) = option.split_at(end);
            let (name, setting) = option
                .split_once('=')
                .ok_or_else(|| Error::MalformedOci("HEALTHCHECK option requires '='".into()))?;
            match name {
                "interval" => health.interval = Some(setting.parse::<Duration>()?.into()),
                "timeout" => health.timeout = Some(setting.parse::<Duration>()?.into()),
                "start-period" => health.start_period = Some(setting.parse::<Duration>()?.into()),
                "start-interval" => {
                    health.start_interval = Some(setting.parse::<Duration>()?.into());
                }
                "retries" => {
                    health.retries = Some(
                        setting
                            .parse::<u64>()
                            .map_err(|_| Error::MalformedOci("invalid HEALTHCHECK retries".into()))?,
                    );
                }
                _ => return Err(Error::MalformedOci("unknown HEALTHCHECK option".into())),
            }
            value = rest.trim_start();
        }
        if value.eq_ignore_ascii_case("NONE") {
            if health.interval.is_some()
                || health.timeout.is_some()
                || health.start_period.is_some()
                || health.start_interval.is_some()
                || health.retries.is_some()
            {
                return Err(Error::MalformedOci("HEALTHCHECK NONE does not accept options".into()));
            }
            health.test = vec!["NONE".into()];
            return Ok(health);
        }
        let command = value
            .strip_prefix("CMD ")
            .ok_or_else(|| Error::MalformedOci("HEALTHCHECK requires CMD or NONE".into()))?;
        health.test = if command.trim_start().starts_with('[') {
            let mut values = vec!["CMD".to_owned()];
            values.extend(
                serde_json::from_str::<Vec<String>>(command)
                    .map_err(|error| Error::MalformedOci(format!("invalid HEALTHCHECK CMD: {error}")))?,
            );
            values
        } else {
            vec!["CMD-SHELL".into(), command.into()]
        };
        Ok(health)
    }
}

impl From<Healthcheck> for serde_json::Value {
    fn from(health: Healthcheck) -> Self {
        let mut value = serde_json::Map::new();
        value.insert("Test".into(), health.test.into());
        for (name, setting) in [
            ("Interval", health.interval),
            ("Timeout", health.timeout),
            ("StartPeriod", health.start_period),
            ("StartInterval", health.start_interval),
            ("Retries", health.retries),
        ] {
            if let Some(setting) = setting {
                value.insert(name.into(), setting.into());
            }
        }
        value.into()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Duration(u64);

impl std::str::FromStr for Duration {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        let split = value
            .find(|character: char| !character.is_ascii_digit() && character != '.')
            .ok_or_else(|| Error::MalformedOci("HEALTHCHECK duration requires a unit".into()))?;
        let number: f64 = value[..split]
            .parse()
            .map_err(|_| Error::MalformedOci("invalid HEALTHCHECK duration".into()))?;
        let seconds = match &value[split..] {
            "ns" => 1e-9,
            "us" | "µs" => 1e-6,
            "ms" => 1e-3,
            "s" => 1.0,
            "m" => 60.0,
            "h" => 3_600.0,
            _ => return Err(Error::MalformedOci("invalid HEALTHCHECK duration unit".into())),
        };
        std::time::Duration::try_from_secs_f64(number * seconds)
            .ok()
            .and_then(|duration| u64::try_from(duration.as_nanos()).ok())
            .map(Self)
            .ok_or_else(|| Error::MalformedOci("invalid HEALTHCHECK duration".into()))
    }
}

impl From<Duration> for u64 {
    fn from(duration: Duration) -> Self {
        duration.0
    }
}

pub(super) struct Environment<'a> {
    values: &'a BTreeMap<String, String>,
}

impl<'a> Environment<'a> {
    pub(super) fn new(values: &'a BTreeMap<String, String>) -> Self {
        Self { values }
    }

    pub(super) fn expand(&self, value: &str) -> Result<String> {
        let mut output = String::with_capacity(value.len());
        let mut chars = value.chars().peekable();
        while let Some(character) = chars.next() {
            if character != '$' {
                output.push(character);
                continue;
            }
            let braced = chars.peek() == Some(&'{');
            if braced {
                chars.next();
            }
            let mut name = String::new();
            while chars
                .peek()
                .is_some_and(|character| character.is_ascii_alphanumeric() || *character == '_')
            {
                name.push(chars.next().expect("peeked character"));
            }
            if braced {
                let mut operator = String::new();
                if matches!(chars.peek(), Some(':' | '-' | '+')) {
                    operator.push(chars.next().expect("peeked operator"));
                    if operator == ":" && matches!(chars.peek(), Some('-' | '+')) {
                        operator.push(chars.next().expect("peeked operator"));
                    }
                }
                let mut alternate = String::new();
                while chars.peek().is_some_and(|character| *character != '}') {
                    alternate.push(chars.next().expect("peeked character"));
                }
                if chars.peek() != Some(&'}') {
                    return Err(Error::MalformedOci("unterminated variable substitution".into()));
                }
                chars.next();
                let value = self.values.get(&name).map(String::as_str);
                let set = value.is_some();
                let nonempty = value.is_some_and(|value| !value.is_empty());
                let replacement = match (operator.as_str(), set, nonempty) {
                    ("-", false, _) | (":-", _, false) | ("+", true, _) | (":+", _, true) => self.expand(&alternate)?,
                    ("" | "-" | ":-", _, _) => value.unwrap_or_default().to_owned(),
                    _ => String::new(),
                };
                output.push_str(&replacement);
            } else {
                output.push_str(self.values.get(&name).map_or("", String::as_str));
            }
        }
        Ok(output)
    }
}
