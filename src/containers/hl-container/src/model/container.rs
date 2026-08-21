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

/// `detail` is only a faulting program counter, and the reasons that carry no
/// instruction report zero, so the cause has to travel beside it.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FaultCause {
    #[default]
    Unknown,
    Fetch,
    Memory,
    Decode,
    Unsupported,
    Frozen,
    CacheEpoch,
    Protocol,
    NativeFatal,
}

/// Terminal process result.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ExitStatus {
    Code(i32),
    Signal(i32),
    Fault {
        status: i32,
        detail: u64,
        #[serde(default)]
        reason: FaultCause,
    },
}

/// Durable diagnostic produced when the runtime cannot determine a process exit status.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub(crate) struct RuntimeDiagnostic(String);

impl RuntimeDiagnostic {
    pub(crate) fn new(message: String) -> Self {
        Self(message)
    }

    pub(crate) fn message(&self) -> &str {
        &self.0
    }
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
    /// Present only when the terminal runtime wait failed instead of returning an exit status.
    #[serde(default)]
    pub(crate) runtime_diagnostic: Option<RuntimeDiagnostic>,
}

impl Container {
    /// Construct a container record with a clean lifecycle generation and no terminal diagnostic.
    #[must_use]
    pub fn new(id: ContainerId, spec: ContainerSpec, state: ContainerState, created_at_ms: u64) -> Self {
        Self {
            id,
            spec,
            state,
            created_at_ms,
            generation: 0,
            restart: Restart::default(),
            health: None,
            checkpoint: None,
            runtime_diagnostic: None,
        }
    }

    /// The runtime's own account of why this container's wait failed instead of returning an exit
    /// status, when there is one.
    ///
    /// It is present exactly beside the stand-in status that replaces the one the runtime could not
    /// produce -- `ExitStatus::Fault { status: -1, detail: 0, reason: FaultCause::Unknown }` -- and
    /// that value carries no cause at all: `-1`, `0` and `Unknown` are what the record says when
    /// nothing is known, so a caller reading only the status cannot tell an engine refusal from a
    /// guest that faulted with no diagnostic. The text is the whole cause and was reachable only
    /// through `wait`, which a caller inspecting a stopped container never calls.
    #[must_use]
    pub fn runtime_diagnostic(&self) -> Option<&str> {
        self.runtime_diagnostic.as_ref().map(RuntimeDiagnostic::message)
    }

    pub(crate) fn uses_volume(&self, name: &str) -> bool {
        self.volume_names().any(|value| value == name)
    }

    pub(crate) fn volume_names(&self) -> impl Iterator<Item = &str> {
        self.spec.mounts.iter().filter_map(|mount| match &mount.source {
            MountSource::Volume(name) | MountSource::Anonymous(name) => Some(name.as_str()),
            MountSource::Bind(_) | MountSource::Tmpfs(_) => None,
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

    /// Container state persists across upgrades, so a record written before the
    /// cause was carried must still load.
    #[test]
    fn a_persisted_fault_without_a_reason_loads_as_unknown() {
        let stored = serde_json::json!({"kind": "fault", "value": {"status": -1, "detail": 0}});

        let decoded: ExitStatus = serde_json::from_value(stored).unwrap();

        assert_eq!(
            decoded,
            ExitStatus::Fault {
                status: -1,
                detail: 0,
                reason: FaultCause::Unknown
            }
        );
    }

    #[test]
    fn engine_namespace_is_stable_and_bounded_for_docker_ids() {
        let id = ContainerId::from_str("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef").unwrap();

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
            runtime_diagnostic: None,
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
        assert!(
            first
                .as_str()
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
        assert_ne!(first, second);
    }

    #[test]
    fn new_id_bulk_are_all_64_lowercase_hex_and_collision_free() {
        let ids = (0..200).map(|_| ContainerId::new()).collect::<Vec<_>>();
        assert!(ids.iter().all(|id| {
            id.as_str().len() == 64
                && id
                    .as_str()
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }));
        assert_eq!(ids.iter().collect::<std::collections::BTreeSet<_>>().len(), ids.len());
    }

    #[test]
    fn new_id_short_id_prefix_is_hex() {
        let id = ContainerId::new();
        assert!(
            id.as_str()[..12]
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
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
