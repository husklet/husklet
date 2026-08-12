//! Application composition boundary for host process construction.

use std::ffi::OsStr;

/// Owns construction of host processes used by the testing application.
pub(crate) struct HostProcess;

impl HostProcess {
    pub(crate) fn standard(program: impl AsRef<OsStr>) -> std::process::Command {
        std::process::Command::new(program)
    }

}
