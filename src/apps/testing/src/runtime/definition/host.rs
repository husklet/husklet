use crate::suite::Error;
use serde::Deserialize;
use std::collections::BTreeSet;

/// Host operating system of the integrated engine under test.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "lowercase")]
pub(crate) enum EngineHost {
    Linux,
    Macos,
    Windows,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HostExclusion {
    hosts: Vec<EngineHost>,
    reason: String,
    evidence: String,
}

impl EngineHost {
    #[cfg(target_os = "linux")]
    pub(crate) const fn current() -> Self {
        Self::Linux
    }

    #[cfg(target_os = "macos")]
    pub(crate) const fn current() -> Self {
        Self::Macos
    }

    #[cfg(target_os = "windows")]
    pub(crate) const fn current() -> Self {
        Self::Windows
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
compile_error!("runtime compatibility supports Linux, macOS, and Windows hosts");

impl HostExclusion {
    pub(super) fn validate(&self) -> Result<(), Error> {
        let unique = self.hosts.iter().copied().collect::<BTreeSet<_>>();
        if self.hosts.is_empty()
            || unique.len() != self.hosts.len()
            || unique.len() == 3
            || self.reason.trim().is_empty()
            || self.evidence.trim().is_empty()
        {
            return Err(
                "host exclusion requires a nonempty proper host set, unique hosts, reason, and evidence".into(),
            );
        }
        Ok(())
    }

    pub(super) fn inactive(&self, host: EngineHost) -> Option<(&'static str, &str, &str)> {
        self.hosts
            .contains(&host)
            .then_some(("UNSUPPORTED", self.reason.as_str(), self.evidence.as_str()))
    }
}
