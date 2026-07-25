use super::{ContainerSpec, Health, MountSource, Restart};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

/// Durable native process-tree checkpoint associated with a stopped container.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Checkpoint {
    #[serde(alias = "directory")]
    pub namespace: String,
    pub created_at_ms: u64,
}

/// Stable, opaque container identity.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ContainerId(String);

impl ContainerId {
    pub(crate) fn new() -> Self {
        Self(format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        ))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Stable identity used for engine-owned per-container namespaces.
    ///
    /// Docker exposes a 256-bit container identifier, while the engine launch
    /// ABI deliberately bounds namespace names. The leading 128 bits retain
    /// UUID-strength uniqueness and leave room for a domain prefix without
    /// leaking that ABI limit into container creation, exec, and healthchecks.
    pub(crate) fn namespace(&self) -> String {
        let identity = &self.0[..self.0.len().min(32)];
        format!("c-{identity}")
    }
}

impl fmt::Display for ContainerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for ContainerId {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let compact = value.replace('-', "");
        if !matches!(compact.len(), 32 | 64)
            || !compact
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("invalid container id");
        }
        Ok(Self(compact))
    }
}

/// Terminal process result.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ExitStatus {
    Code(i32),
    Signal(i32),
    Fault { status: i32, detail: u64 },
}

/// Persisted lifecycle state.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ContainerState {
    Created,
    Running {
        process_id: u64,
        started_at_ms: u64,
    },
    Paused {
        process_id: u64,
        started_at_ms: u64,
        paused_at_ms: u64,
    },
    Restarting {
        result: ExitStatus,
        finished_at_ms: u64,
        ready_at_ms: u64,
    },
    Exited {
        result: ExitStatus,
        finished_at_ms: u64,
    },
}

impl ContainerState {
    #[must_use]
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Running { .. } | Self::Paused { .. } | Self::Restarting { .. }
        )
    }

    #[must_use]
    pub fn is_paused(&self) -> bool {
        matches!(self, Self::Paused { .. })
    }
}

/// Persisted container record returned by headless operations.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Container {
    pub id: ContainerId,
    pub spec: ContainerSpec,
    pub state: ContainerState,
    pub created_at_ms: u64,
    pub generation: u64,
    pub restart: Restart,
    pub health: Option<Health>,
    pub checkpoint: Option<Checkpoint>,
}

impl Container {
    pub(crate) fn uses_volume(&self, name: &str) -> bool {
        self.spec.mounts.iter().any(|mount| {
            matches!(
                &mount.source,
                MountSource::Volume(value) | MountSource::Anonymous(value) if value == name
            )
        })
    }

    pub(crate) fn hostname(&self) -> String {
        self.spec
            .hostname
            .clone()
            .unwrap_or_else(|| self.id.as_str()[..self.id.as_str().len().min(12)].to_owned())
    }

    pub(crate) fn require_exec(&self) -> crate::Result<()> {
        if matches!(self.state, ContainerState::Running { .. }) {
            return Ok(());
        }
        Err(crate::Error::InvalidState {
            id: self.id.clone(),
            actual: self.state.clone(),
            expected: "running and not paused",
        })
    }
}

pub(crate) fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Process;
    use std::str::FromStr;

    #[test]
    fn engine_namespace_is_stable_and_bounded_for_docker_ids() {
        let id = ContainerId::from_str(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .unwrap();

        assert_eq!(id.namespace(), "c-0123456789abcdef0123456789abcdef");
        assert!(id.namespace().len() <= 39);
    }

    #[test]
    fn volume_usage_includes_named_and_anonymous_mounts() {
        let mut spec = ContainerSpec::from_directory("/rootfs", Process::new("/bin/true"));
        spec.mounts = vec![
            crate::Mount::volume_read_write("named", "/named"),
            crate::Mount {
                source: MountSource::Anonymous("private".into()),
                target: "/private".into(),
                access: crate::Access::ReadWrite,
                populate: false,
                subpath: None,
                propagation: crate::BindPropagation::RecursivePrivate,
                recursive: true,
            },
            crate::Mount::read_write("/host", "/bind"),
        ];
        let container = Container {
            id: ContainerId::new(),
            spec,
            state: ContainerState::Created,
            created_at_ms: 1,
            generation: 0,
            restart: Restart::default(),
            health: None,
            checkpoint: None,
        };
        assert!(container.uses_volume("named"));
        assert!(container.uses_volume("private"));
        assert!(!container.uses_volume("/host"));
        assert!(!container.uses_volume("missing"));
    }

    #[test]
    fn new_id_is_64_hex_and_unique() {
        let first = ContainerId::new();
        let second = ContainerId::new();
        assert_eq!(first.as_str().len(), 64);
        assert!(first
            .as_str()
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
        assert_ne!(first, second);
    }

    #[test]
    fn new_id_bulk_are_all_64_lowercase_hex_and_collision_free() {
        let ids = (0..200).map(|_| ContainerId::new()).collect::<Vec<_>>();
        assert!(ids.iter().all(|id| id.as_str().len() == 64
            && id
                .as_str()
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))));
        assert_eq!(
            ids.iter().collect::<std::collections::BTreeSet<_>>().len(),
            ids.len()
        );
    }

    #[test]
    fn new_id_short_id_prefix_is_hex() {
        let id = ContainerId::new();
        assert!(id.as_str()[..12]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
    }

    #[test]
    fn new_id_ignores_image_arg_for_shape() {
        let empty = ContainerId::new();
        let image_independent = ContainerId::new();
        assert_eq!(empty.as_str().len(), 64);
        assert_eq!(image_independent.as_str().len(), 64);
        assert_ne!(empty, image_independent);
    }
}
