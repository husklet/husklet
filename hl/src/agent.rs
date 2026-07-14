//! Daemon service management (no root), behind the platform seam. On macOS this is a per-user
//! launchd LaunchAgent; on Linux a `systemd --user` unit; elsewhere an unsupported stub. Callers
//! use these helpers instead of touching `launchctl`/`systemctl`/the unit file directly — see
//! `crate::platform` for the per-OS implementations.

use crate::platform;
use std::path::PathBuf;

/// A human-readable identifier for the daemon service (shown in install output).
pub fn service_target() -> String {
    platform::service_label()
}

/// Write the service unit + `~/.dd` state tree (does not start it). Returns the unit path.
pub fn write_unit() -> std::io::Result<PathBuf> {
    platform::service_write()
}

/// Write the unit (if missing) and start the daemon service. Idempotent.
///
/// Relocated here from the (removed) `doctor` module: this is agent logic, shared by
/// `install`, `daemon start`, and `run`'s best-effort auto-start.
pub fn ensure() -> Result<(), String> {
    platform::service_ensure()
}

/// Stop (unload) the daemon service, leaving its unit file in place.
pub fn bootout() -> std::io::Result<()> {
    platform::service_stop()
}

/// Restart the daemon service.
pub fn kickstart() -> std::io::Result<()> {
    platform::service_restart()
}

/// Stop the service and remove its unit file (the uninstall path).
pub fn remove() -> std::io::Result<()> {
    platform::service_remove()
}

/// Print the service status to stdout; returns whether it is loaded/active.
pub fn print_status() -> std::io::Result<bool> {
    platform::service_status()
}
