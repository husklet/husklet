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
    pub streams: Streams,
    pub privileged: bool,
    pub detach_keys: String,
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

    /// Resolve Docker's `User` field against the container's account databases.
    ///
    /// Container create and the image builder resolve the same way, so `docker exec -u root`
    /// and `docker run -u root` agree on the identity they select.
    pub(crate) fn apply_user(&mut self, rootfs: &std::path::Path) -> crate::Result<()> {
        if self.user.is_empty() {
            return Ok(());
        }
        let (uid, gid) = Process::resolve_user(&self.user, rootfs)?;
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
    pub checkpoint: Option<crate::Checkpoint>,
}

impl Exec {
    pub(crate) fn new(container: ContainerId, spec: ExecSpec) -> Self {
        Self {
            id: ExecId::new(),
            container,
            spec,
            state: ExecState::Created,
            created_at_ms: now_ms(),
            checkpoint: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `docker exec -u root` must work, so a named user resolves against the container's own
    /// account databases exactly as container create does.
    #[test]
    fn exec_user_resolves_names_and_numeric_identities_against_the_container_root() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("etc")).unwrap();
        std::fs::write(
            root.path().join("etc/passwd"),
            "root:x:0:0:root:/root:/bin/sh\nubuntu:x:1000:1000::/home/ubuntu:/bin/bash\n",
        )
        .unwrap();
        std::fs::write(root.path().join("etc/group"), "root:x:0:\nstaff:x:50:\n").unwrap();
        let resolve = |value: &str| {
            let mut spec = ExecSpec::new(Process::new("/usr/bin/id")).user(value);
            spec.apply_user(root.path())
                .map(|()| (spec.process.uid, spec.process.gid))
        };

        assert_eq!(resolve("root").unwrap(), (Some(0), Some(0)));
        assert_eq!(resolve("ubuntu").unwrap(), (Some(1000), Some(1000)));
        assert_eq!(resolve("ubuntu:staff").unwrap(), (Some(1000), Some(50)));
        assert_eq!(resolve("1000:1001").unwrap(), (Some(1000), Some(1001)));
        // Docker takes a numeric id verbatim, with no passwd entry required.
        assert_eq!(resolve("4242").unwrap(), (Some(4242), Some(4242)));
        // An empty value inherits the container's configured identity.
        assert_eq!(resolve("").unwrap(), (None, None));

        for rejected in ["absent", "root:absent", "-1", ":"] {
            assert!(
                matches!(resolve(rejected), Err(crate::Error::InvalidSpec(_))),
                "{rejected}"
            );
        }
    }

    /// A scratch image without account databases is a bad request, not a missing resource: the
    /// daemon turns `Error::Io(NotFound)` into HTTP 404.
    #[test]
    fn a_named_user_without_account_databases_is_reported_as_an_invalid_spec() {
        let root = tempfile::tempdir().unwrap();
        let mut spec = ExecSpec::new(Process::new("/usr/bin/id")).user("root");

        assert!(matches!(
            spec.apply_user(root.path()),
            Err(crate::Error::InvalidSpec(_))
        ));
    }
}
