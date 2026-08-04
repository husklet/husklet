//! Native process mechanisms used only by the lifecycle fixture binary.

#![allow(unsafe_code)]

use std::path::Path;
use std::process::{Child, Command};

/// Explicit platform boundary for the lifecycle fixture's child topology.
pub struct ChildFixture;

impl ChildFixture {
    pub fn spawn(identity: &Path, escape: bool) -> std::io::Result<Child> {
        let child = Command::new(std::env::current_exe()?)
            .env("HL_FIXTURE_BLOCK", "1")
            .env("HL_FIXTURE_CHILD", "1")
            .env("HL_FIXTURE_ESCAPE", if escape { "1" } else { "0" })
            .spawn()?;
        std::fs::write(identity, child.id().to_string())?;
        Ok(child)
    }

    #[cfg(unix)]
    pub fn detach() -> std::io::Result<()> {
        // SAFETY: setsid has no pointer arguments and changes only the calling
        // fixture process's session and group membership.
        if unsafe { libc::setsid() } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    #[cfg(not(unix))]
    pub fn detach() -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "fixture session detachment is unavailable",
        ))
    }
}
