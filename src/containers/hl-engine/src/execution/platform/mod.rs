//! Retained-worker host process adapter.

use std::ffi::OsStr;

pub(super) fn worker(executable: impl AsRef<OsStr>) -> std::process::Command {
    std::process::Command::new(executable)
}
