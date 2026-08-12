//! Standard-library process construction adapter.

use std::ffi::OsStr;

pub(super) fn command(program: impl AsRef<OsStr>) -> std::process::Command {
    std::process::Command::new(program)
}
