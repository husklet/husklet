use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Docker health-check wire representation.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct Healthcheck {
    #[serde(default)]
    pub test: Vec<String>,
    #[serde(default)]
    pub interval: i64,
    #[serde(default)]
    pub timeout: i64,
    #[serde(default)]
    pub retries: i64,
    #[serde(default)]
    pub start_period: i64,
    #[serde(default)]
    pub start_interval: i64,
}

#[cfg(feature = "runtime")]
impl Healthcheck {
    pub(crate) fn policy(self) -> Result<Option<hl_container::Healthcheck>, String> {
        use hl_container::{Check, Healthcheck as Policy, Process};
        let Some(kind) = self.test.first().map(String::as_str) else {
            return Ok(None);
        };
        let command = match kind {
            "NONE" if self.test.len() == 1 => return Ok(None),
            "CMD" if self.test.len() > 1 => {
                Check::Command(Process::new(&self.test[1]).args(self.test[2..].iter().cloned()))
            }
            "CMD-SHELL" if self.test.len() > 1 => Check::Shell(self.test[1..].join(" ")),
            _ => {
                return Err("Healthcheck.Test must be NONE, CMD, or CMD-SHELL with a command".into());
            }
        };
        let duration = |name: &str, value: i64, default| match value.cmp(&0) {
            std::cmp::Ordering::Less => Err(format!("Healthcheck.{name} must be nonnegative")),
            std::cmp::Ordering::Equal => Ok(default),
            std::cmp::Ordering::Greater => Ok(std::time::Duration::from_nanos(
                u64::try_from(value).expect("positive i64 always fits u64"),
            )),
        };
        let retries = match self.retries {
            value if value < 0 => return Err("Healthcheck.Retries must be nonnegative".into()),
            0 => 3,
            value => u32::try_from(value).map_err(|_| "Healthcheck.Retries exceeds u32".to_owned())?,
        };
        Ok(Some(
            Policy::new(command)
                .interval(duration("Interval", self.interval, std::time::Duration::from_secs(30))?)
                .timeout(duration("Timeout", self.timeout, std::time::Duration::from_secs(30))?)
                .retries(retries)
                .start_period(duration("StartPeriod", self.start_period, std::time::Duration::ZERO)?)
                .start_interval(duration(
                    "StartInterval",
                    self.start_interval,
                    std::time::Duration::from_secs(5),
                )?),
        ))
    }
}

/// Docker automatic-restart wire representation.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct RestartPolicy {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub maximum_retry_count: i64,
}

#[cfg(feature = "runtime")]
impl RestartPolicy {
    pub(crate) fn policy(&self) -> Result<hl_container::RestartPolicy, String> {
        let maximum = match self.maximum_retry_count {
            value if value < 0 => {
                return Err("RestartPolicy.MaximumRetryCount must be nonnegative".into());
            }
            0 => None,
            value => Some(u32::try_from(value).map_err(|_| "RestartPolicy.MaximumRetryCount exceeds u32".to_owned())?),
        };
        match self.name.as_str() {
            "" | "no" if maximum.is_none() => Ok(hl_container::RestartPolicy::Never),
            "always" if maximum.is_none() => Ok(hl_container::RestartPolicy::Always),
            "unless-stopped" if maximum.is_none() => Ok(hl_container::RestartPolicy::UnlessStopped),
            "on-failure" => Ok(hl_container::RestartPolicy::OnFailure { maximum }),
            "" | "no" | "always" | "unless-stopped" => Err("MaximumRetryCount is only valid for on-failure".into()),
            name => Err(format!("unsupported restart policy {name:?}")),
        }
    }
}

#[cfg(feature = "runtime")]
impl From<hl_container::RestartPolicy> for RestartPolicy {
    fn from(value: hl_container::RestartPolicy) -> Self {
        match value {
            hl_container::RestartPolicy::Never => Self {
                name: "no".into(),
                maximum_retry_count: 0,
            },
            hl_container::RestartPolicy::Always => Self {
                name: "always".into(),
                maximum_retry_count: 0,
            },
            hl_container::RestartPolicy::UnlessStopped => Self {
                name: "unless-stopped".into(),
                maximum_retry_count: 0,
            },
            hl_container::RestartPolicy::OnFailure { maximum } => Self {
                name: "on-failure".into(),
                maximum_retry_count: maximum.map_or(0, i64::from),
            },
        }
    }
}

/// Runtime-effective subset of Docker's container-update request.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct Update {
    pub memory: Option<i64>,
    pub pids_limit: Option<i64>,
    pub nano_cpus: Option<i64>,
    pub restart_policy: Option<RestartPolicy>,
    #[serde(flatten, default)]
    pub unsupported: BTreeMap<String, serde_json::Value>,
}

#[cfg(feature = "runtime")]
impl Update {
    pub(crate) fn settings(&self) -> Result<hl_container::Update, String> {
        if let Some(name) = CompatibilityFields::from(&self.unsupported).first_meaningful() {
            return Err(format!("unsupported container update field {name}"));
        }
        let restart = self.restart_policy.as_ref().map(RestartPolicy::policy).transpose()?;
        Ok(hl_container::Update {
            memory_bytes: self.memory_bytes()?,
            process_count: self.process_count()?,
            cpu_count: self.cpu_count()?,
            restart,
        })
    }

    fn memory_bytes(&self) -> Result<Option<u64>, String> {
        let Some(value) = self.memory else {
            return Ok(None);
        };
        u64::try_from(value)
            .map(Some)
            .map_err(|_| "Memory must be nonnegative".to_owned())
    }

    fn process_count(&self) -> Result<Option<u32>, String> {
        let Some(value) = self.pids_limit else {
            return Ok(None);
        };
        match value {
            -1 | 0 => Ok(Some(0)),
            value if value > 0 => u32::try_from(value)
                .map(Some)
                .map_err(|_| "PidsLimit exceeds u32".to_owned()),
            _ => Err("PidsLimit must be -1, 0, or positive".into()),
        }
    }

    fn cpu_count(&self) -> Result<Option<u32>, String> {
        let Some(value) = self.nano_cpus else {
            return Ok(None);
        };
        let value = u64::try_from(value).map_err(|_| "NanoCpus must be nonnegative".to_owned())?;
        u32::try_from(value.div_ceil(1_000_000_000))
            .map(Some)
            .map_err(|_| "NanoCpus exceeds u32 CPUs".to_owned())
    }
}

/// Borrowed view of unknown Docker fields retained for compatibility validation.
pub struct CompatibilityFields<'a>(&'a BTreeMap<String, serde_json::Value>);

impl<'a> From<&'a BTreeMap<String, serde_json::Value>> for CompatibilityFields<'a> {
    fn from(fields: &'a BTreeMap<String, serde_json::Value>) -> Self {
        Self(fields)
    }
}

impl<'a> CompatibilityFields<'a> {
    #[must_use]
    pub fn first_meaningful(&self) -> Option<&'a str> {
        self.0
            .iter()
            .find(|(_, value)| !match value {
                serde_json::Value::Null => true,
                serde_json::Value::Bool(value) => !value,
                serde_json::Value::Number(value) => value.as_i64() == Some(0),
                serde_json::Value::String(value) => value.is_empty(),
                serde_json::Value::Array(values) => values.is_empty(),
                serde_json::Value::Object(values) => values.is_empty(),
            })
            .map(|(name, _)| name.as_str())
    }
}

/// Docker container-update acknowledgement.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct UpdateResult {
    pub warnings: Vec<String>,
}

#[cfg(all(test, feature = "runtime"))]
mod tests {
    use super::{Healthcheck, RestartPolicy, Update};

    #[test]
    fn docker_healthcheck_maps_commands_defaults_and_validation() {
        let shell = Healthcheck {
            test: vec!["CMD-SHELL".into(), "test -f /ready".into()],
            ..Default::default()
        }
        .policy()
        .unwrap()
        .unwrap();
        assert!(matches!(shell.command, hl_container::Check::Shell(ref value) if value == "test -f /ready"));
        assert_eq!(shell.interval, std::time::Duration::from_secs(30));
        assert_eq!(shell.timeout, std::time::Duration::from_secs(30));
        assert_eq!(shell.retries, 3);

        let command = Healthcheck {
            test: vec!["CMD".into(), "/bin/check".into(), "--quiet".into()],
            interval: 500_000_000,
            timeout: 2_000_000_000,
            retries: 5,
            start_period: 1_000_000_000,
            start_interval: 100_000_000,
        }
        .policy()
        .unwrap()
        .unwrap();
        assert!(matches!(command.command, hl_container::Check::Command(ref process)
            if process.program == "/bin/check" && process.args == ["--quiet"]));
        assert_eq!(command.interval, std::time::Duration::from_millis(500));
        assert_eq!(command.start_interval, std::time::Duration::from_millis(100));
        assert!(
            Healthcheck {
                test: vec!["CMD".into()],
                ..Default::default()
            }
            .policy()
            .is_err()
        );
    }

    #[test]
    fn docker_restart_policy_maps_limits_without_silent_ignores() {
        assert_eq!(
            RestartPolicy {
                name: "unless-stopped".into(),
                maximum_retry_count: 0,
            }
            .policy()
            .unwrap(),
            hl_container::RestartPolicy::UnlessStopped
        );
        assert_eq!(
            RestartPolicy {
                name: "on-failure".into(),
                maximum_retry_count: 4,
            }
            .policy()
            .unwrap(),
            hl_container::RestartPolicy::OnFailure { maximum: Some(4) }
        );
        assert!(
            RestartPolicy {
                name: "always".into(),
                maximum_retry_count: 1,
            }
            .policy()
            .is_err()
        );
    }

    #[test]
    fn update_accepts_only_harmless_unknown_defaults() {
        let mut update = Update::default();
        update.unsupported.insert("CpuShares".into(), serde_json::json!(0));
        update
            .unsupported
            .insert("BlkioWeightDevice".into(), serde_json::json!([]));
        assert!(update.settings().is_ok());

        update.unsupported.insert("CpuShares".into(), serde_json::json!(512));
        assert_eq!(
            update.settings().unwrap_err(),
            "unsupported container update field CpuShares"
        );
    }
}
