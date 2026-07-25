use super::ExitStatus;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Automatic restart policy attached to a container specification.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum RestartPolicy {
    #[default]
    Never,
    Always,
    UnlessStopped,
    OnFailure {
        maximum: Option<u32>,
    },
}

impl RestartPolicy {
    pub(crate) fn validate(self) -> Result<()> {
        if matches!(self, Self::OnFailure { maximum: Some(0) }) {
            return Err(Error::InvalidSpec(
                "restart attempt limit must be greater than zero".into(),
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn allows(self, result: ExitStatus, restart: &Restart) -> bool {
        if restart.manually_stopped {
            return false;
        }
        match self {
            Self::Never => false,
            Self::Always | Self::UnlessStopped => true,
            Self::OnFailure { maximum } => {
                !matches!(result, ExitStatus::Code(0))
                    && maximum.is_none_or(|maximum| restart.count < maximum)
            }
        }
    }

    #[must_use]
    pub fn delay(self, restart: &Restart) -> Duration {
        let shift = restart.count.min(7);
        Duration::from_millis((100u64 << shift).min(10_000))
    }
}

/// Durable automatic-restart bookkeeping.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Restart {
    pub count: u32,
    pub manually_stopped: bool,
}

impl Restart {
    pub fn completed_run(&mut self, elapsed: Duration) {
        if elapsed >= Duration::from_secs(10) {
            self.count = 0;
        }
    }

    pub(crate) fn completed_between(&mut self, started_at_ms: u64, finished_at_ms: u64) {
        self.completed_run(Duration::from_millis(
            finished_at_ms.saturating_sub(started_at_ms),
        ));
    }
    pub fn manual(&mut self) {
        self.manually_stopped = true;
    }

    pub fn automatic(&mut self) {
        self.count = self.count.saturating_add(1);
    }

    pub fn started(&mut self, explicit: bool) {
        if explicit {
            self.count = 0;
            self.manually_stopped = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policies_distinguish_clean_failure_limit_and_manual_stop() {
        let mut restart = Restart::default();
        assert!(!RestartPolicy::Never.allows(ExitStatus::Code(1), &restart));
        assert!(RestartPolicy::Always.allows(ExitStatus::Code(0), &restart));
        assert!(RestartPolicy::UnlessStopped.allows(ExitStatus::Signal(9), &restart));
        let policy = RestartPolicy::OnFailure { maximum: Some(2) };
        assert!(!policy.allows(ExitStatus::Code(0), &restart));
        assert!(policy.allows(ExitStatus::Code(1), &restart));
        restart.automatic();
        assert!(policy.allows(
            ExitStatus::Fault {
                status: -1,
                detail: 0
            },
            &restart
        ));
        restart.automatic();
        assert!(!policy.allows(ExitStatus::Code(1), &restart));
        restart.manual();
        assert!(!RestartPolicy::Always.allows(ExitStatus::Code(1), &restart));
    }

    #[test]
    fn explicit_start_resets_manual_state_and_attempts() {
        let mut restart = Restart {
            count: 4,
            manually_stopped: true,
        };
        restart.started(false);
        assert_eq!(restart.count, 4);
        assert!(restart.manually_stopped);
        restart.started(true);
        assert_eq!(restart, Restart::default());
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let policy = RestartPolicy::Always;
        let mut restart = Restart::default();
        let mut delays = Vec::new();
        for _ in 0..9 {
            delays.push(policy.delay(&restart));
            restart.automatic();
        }
        assert_eq!(delays[0], Duration::from_millis(100));
        assert_eq!(delays[1], Duration::from_millis(200));
        assert_eq!(delays[6], Duration::from_millis(6400));
        assert_eq!(delays[7], Duration::from_secs(10));
        assert_eq!(delays[8], Duration::from_secs(10));
        assert!(RestartPolicy::OnFailure { maximum: Some(0) }
            .validate()
            .is_err());
    }

    #[test]
    fn ten_second_run_resets_backoff_without_clearing_manual_stop() {
        let mut restart = Restart {
            count: 7,
            manually_stopped: true,
        };
        restart.completed_run(Duration::from_secs(9));
        assert_eq!(restart.count, 7);
        restart.completed_run(Duration::from_secs(10));
        assert_eq!(restart.count, 0);
        assert!(restart.manually_stopped);
    }
}
