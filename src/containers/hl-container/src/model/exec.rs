use super::{now_ms, ContainerId, ExitStatus, Process};
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// Stable, opaque identity for an additional process in a container.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ExecId(String);

impl ExecId {
    pub(crate) fn new() -> Self {
        Self(uuid::Uuid::new_v4().simple().to_string())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ExecId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for ExecId {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        uuid::Uuid::parse_str(value).map_err(|_| "invalid exec id")?;
        Ok(Self(value.replace('-', "")))
    }
}

/// Streams selected when an execution is created.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Streams {
    pub stdin: bool,
    pub stdout: bool,
    pub stderr: bool,
}

impl Default for Streams {
    fn default() -> Self {
        Self {
            stdin: false,
            stdout: true,
            stderr: true,
        }
    }
}

/// Immutable definition of an additional process in an existing container.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExecSpec {
    pub process: Process,
    #[serde(default)]
    pub streams: Streams,
    #[serde(default)]
    pub privileged: bool,
    #[serde(default)]
    pub detach_keys: String,
    #[serde(default)]
    pub user: String,
}

impl ExecSpec {
    #[must_use]
    pub fn new(process: Process) -> Self {
        Self {
            process,
            streams: Streams::default(),
            privileged: false,
            detach_keys: String::new(),
            user: String::new(),
        }
    }

    #[must_use]
    pub const fn streams(mut self, value: Streams) -> Self {
        self.streams = value;
        self
    }

    #[must_use]
    pub const fn privileged(mut self, value: bool) -> Self {
        self.privileged = value;
        self
    }

    #[must_use]
    pub fn detach_keys(mut self, value: impl Into<String>) -> Self {
        self.detach_keys = value.into();
        self
    }

    #[must_use]
    pub fn user(mut self, value: impl Into<String>) -> Self {
        self.user = value.into();
        self
    }

    pub(crate) fn apply_user(&mut self) -> crate::Result<()> {
        if self.user.is_empty() {
            return Ok(());
        }
        let (uid, gid) = self
            .user
            .split_once(':')
            .map_or((&*self.user, &*self.user), |values| values);
        let uid = uid.parse().map_err(|_| {
            crate::Error::InvalidSpec("exec user must be a numeric UID or UID:GID".into())
        })?;
        let gid = gid.parse().map_err(|_| {
            crate::Error::InvalidSpec("exec user must be a numeric UID or UID:GID".into())
        })?;
        self.process.uid = Some(uid);
        self.process.gid = Some(gid);
        Ok(())
    }
}

/// Persisted execution lifecycle state.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ExecState {
    Created,
    Running {
        process_id: u64,
        started_at_ms: u64,
    },
    Exited {
        result: ExitStatus,
        finished_at_ms: u64,
    },
}

impl ExecState {
    #[must_use]
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Running { .. })
    }
}

/// Persisted execution record returned by inspect operations.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Exec {
    pub id: ExecId,
    pub container: ContainerId,
    pub spec: ExecSpec,
    pub state: ExecState,
    pub created_at_ms: u64,
}

impl Exec {
    pub(crate) fn new(container: ContainerId, spec: ExecSpec) -> Self {
        Self {
            id: ExecId::new(),
            container,
            spec,
            state: ExecState::Created,
            created_at_ms: now_ms(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_user_updates_effective_process_identity() {
        let mut spec = ExecSpec::new(Process::new("/usr/bin/id")).user("1000:1001");
        spec.apply_user().unwrap();
        assert_eq!(
            (spec.process.uid, spec.process.gid),
            (Some(1000), Some(1001))
        );

        let mut invalid = ExecSpec::new(Process::new("/usr/bin/id")).user("named");
        assert!(invalid.apply_user().is_err());
    }
}
