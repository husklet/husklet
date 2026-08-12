//! Application composition boundary for host process construction.

use std::ffi::OsStr;

/// Owns construction of host processes used by the testing application.
pub(crate) struct HostProcess;

impl HostProcess {
    pub(crate) fn standard(program: impl AsRef<OsStr>) -> std::process::Command {
        std::process::Command::new(program)
    }

    pub(crate) fn asynchronous(program: impl AsRef<OsStr>) -> tokio::process::Command {
        tokio::process::Command::new(program)
    }
}
