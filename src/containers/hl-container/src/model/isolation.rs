use serde::{Deserialize, Serialize};

/// Guest syscall isolation policy.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Sandbox {
    Disabled,
    /// Enable sentry routing and the engine's deny-default worker profile. This is an explicit
    /// opt-in for workloads whose entire syscall surface is supported by that profile.
    Enabled,
    /// Route untrusted syscalls through the engine sentry without its worker profile. This is the
    /// compatibility-safe production default for general Linux programs.
    #[default]
    SentryOnly,
}

/// Container network attachment policy.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    /// Derive disabled or virtual networking from isolation and attached endpoints.
    #[default]
    Automatic,
    /// Use the engine's host network stack without a guest network namespace or endpoints.
    Host,
}

/// Guest-visible seccomp state at launch.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SeccompBaseline {
    /// Report a filter already installed, as Docker's default profile does.
    #[default]
    Container,
    /// Report no filter, matching `docker run --security-opt seccomp=unconfined`.
    Disabled,
}

/// Isolation applied to a container launch.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Isolation {
    pub sandbox: Sandbox,
    pub read_only_root: bool,
    pub network_isolated: bool,
    pub seccomp_baseline: SeccompBaseline,
}

impl Default for Isolation {
    fn default() -> Self {
        Self {
            sandbox: Sandbox::SentryOnly,
            read_only_root: false,
            network_isolated: false,
            seccomp_baseline: SeccompBaseline::Container,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Isolation is opt-in: a default container is bridged and owns an `eth0`,
    /// matching Docker's default and the retained engine's `HL_NET_ISOLATE` gate.
    #[test]
    fn default_isolation_leaves_networking_enabled() {
        assert!(!Isolation::default().network_isolated);
    }
}

/// Resource ceilings. Zero means the engine's platform default.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Resources {
    pub memory_bytes: u64,
    pub process_count: u32,
    /// Number of guest-visible logical CPUs. Zero selects the engine default.
    pub cpu_count: u32,
}
