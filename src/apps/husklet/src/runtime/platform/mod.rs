//! Host-process construction for the application runtime.

use std::ffi::OsStr;

pub(super) fn current_application() -> std::io::Result<std::process::Command> {
    Ok(std::process::Command::new(std::env::current_exe()?))
}

pub(super) fn daemon(executable: impl AsRef<OsStr>) -> std::process::Command {
    std::process::Command::new(executable)
}
