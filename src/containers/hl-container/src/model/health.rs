use super::{ExitStatus, Process};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::time::Duration;

const OUTPUT_LIMIT: usize = 4096;
const HISTORY_LIMIT: usize = 5;

/// Process executed to determine whether a container is healthy.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Check {
    Command(Process),
    Shell(String),
}

impl Check {
    fn validate(&self) -> Result<()> {
        match self {
            Self::Command(process) if process.program.is_empty() => {
                Err(Error::InvalidSpec("health command program must not be empty".into()))
            }
            Self::Command(process) if process.console.terminal.is_some() => {
                Err(Error::InvalidSpec("health commands cannot allocate a terminal".into()))
            }
            Self::Shell(command) if command.is_empty() => {
                Err(Error::InvalidSpec("health shell command must not be empty".into()))
            }
            _ => Ok(()),
        }
    }
}

/// Immutable health-check policy attached to a container specification.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Healthcheck {
    pub command: Check,
    pub interval: Duration,
    pub timeout: Duration,
    pub retries: u32,
    pub start_period: Duration,
    #[serde(default = "default_start_interval")]
    pub start_interval: Duration,
}

fn default_start_interval() -> Duration {
    Duration::from_secs(5)
}

impl Healthcheck {
    #[must_use]
    pub fn new(command: Check) -> Self {
        Self {
            command,
            interval: Duration::from_secs(30),
            timeout: Duration::from_secs(30),
            retries: 3,
            start_period: Duration::ZERO,
            start_interval: default_start_interval(),
        }
    }

    #[must_use]
    pub const fn interval(mut self, value: Duration) -> Self {
        self.interval = value;
        self
    }

    #[must_use]
    pub const fn timeout(mut self, value: Duration) -> Self {
        self.timeout = value;
        self
    }

    #[must_use]
    pub const fn retries(mut self, value: u32) -> Self {
        self.retries = value;
        self
    }

    #[must_use]
    pub const fn start_period(mut self, value: Duration) -> Self {
        self.start_period = value;
        self
    }

    #[must_use]
    pub const fn start_interval(mut self, value: Duration) -> Self {
        self.start_interval = value;
        self
    }

    pub(crate) fn validate(&self) -> Result<()> {
        self.command.validate()?;
        if self.interval.is_zero() {
            return Err(Error::InvalidSpec("health interval must be greater than zero".into()));
        }
        if self.timeout.is_zero() {
            return Err(Error::InvalidSpec("health timeout must be greater than zero".into()));
        }
        if self.start_interval.is_zero() {
            return Err(Error::InvalidSpec(
                "health start interval must be greater than zero".into(),
            ));
        }
        if self.retries == 0 {
            return Err(Error::InvalidSpec("health retries must be greater than zero".into()));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    Starting,
    Healthy,
    Unhealthy,
}

/// One completed health-check execution.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Probe {
    pub started_at_ms: u64,
    pub finished_at_ms: u64,
    pub result: ExitStatus,
    pub output: String,
}

impl Probe {
    #[must_use]
    pub fn new(started_at_ms: u64, finished_at_ms: u64, result: ExitStatus, output: impl Into<String>) -> Self {
        let output = output.into();
        let output = output
            .chars()
            .scan(0usize, |bytes, character| {
                *bytes += character.len_utf8();
                (*bytes <= OUTPUT_LIMIT).then_some(character)
            })
            .collect();
        Self {
            started_at_ms,
            finished_at_ms,
            result,
            output,
        }
    }

    #[must_use]
    pub const fn success(&self) -> bool {
        matches!(self.result, ExitStatus::Code(0))
    }
}

/// Durable health state and bounded probe history.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Health {
    pub status: HealthStatus,
    pub failures: u32,
    pub probes: VecDeque<Probe>,
}

impl Health {
    #[must_use]
    pub fn starting() -> Self {
        Self {
            status: HealthStatus::Starting,
            failures: 0,
            probes: VecDeque::new(),
        }
    }

    pub fn record(&mut self, probe: Probe, check: &Healthcheck, elapsed: Duration) {
        if probe.success() {
            self.status = HealthStatus::Healthy;
            self.failures = 0;
        } else if elapsed >= check.start_period {
            self.failures = self.failures.saturating_add(1);
            if self.failures >= check.retries {
                self.status = HealthStatus::Unhealthy;
            }
        }
        self.probes.push_back(probe);
        while self.probes.len() > HISTORY_LIMIT {
            self.probes.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check() -> Healthcheck {
        Healthcheck::new(Check::Shell("true".into()))
            .retries(3)
            .start_period(Duration::from_secs(10))
    }

    fn probe(code: i32, output: impl Into<String>) -> Probe {
        Probe::new(1, 2, ExitStatus::Code(code), output)
    }

    #[test]
    fn grace_failures_do_not_count_and_success_recovers() {
        let check = check();
        let mut health = Health::starting();
        health.record(probe(1, "failure"), &check, Duration::from_secs(9));
        assert_eq!(health.status, HealthStatus::Starting);
        assert_eq!(health.failures, 0);
        for _ in 0..3 {
            health.record(probe(1, "failure"), &check, Duration::from_secs(10));
        }
        assert_eq!(health.status, HealthStatus::Unhealthy);
        assert_eq!(health.failures, 3);
        health.record(probe(0, "recovered"), &check, Duration::from_secs(11));
        assert_eq!(health.status, HealthStatus::Healthy);
        assert_eq!(health.failures, 0);
    }

    #[test]
    fn history_and_output_are_bounded_without_splitting_utf8() {
        let check = check();
        let mut health = Health::starting();
        for index in 0..7 {
            health.record(probe(0, format!("probe-{index}")), &check, Duration::ZERO);
        }
        assert_eq!(health.probes.len(), 5);
        assert_eq!(health.probes.front().unwrap().output, "probe-2");
        let output = format!("{}tail", "é".repeat(3000));
        let probe = probe(1, output);
        assert!(probe.output.len() <= OUTPUT_LIMIT);
        assert!(probe.output.is_char_boundary(probe.output.len()));
    }

    #[test]
    fn policy_rejects_empty_or_zero_configuration() {
        assert!(Healthcheck::new(Check::Shell(String::new())).validate().is_err());
        assert!(
            Healthcheck::new(Check::Shell("true".into()))
                .interval(Duration::ZERO)
                .validate()
                .is_err()
        );
        assert!(
            Healthcheck::new(Check::Shell("true".into()))
                .timeout(Duration::ZERO)
                .validate()
                .is_err()
        );
        assert!(
            Healthcheck::new(Check::Shell("true".into()))
                .start_interval(Duration::ZERO)
                .validate()
                .is_err()
        );
        assert!(
            Healthcheck::new(Check::Shell("true".into()))
                .retries(0)
                .validate()
                .is_err()
        );
    }
}
