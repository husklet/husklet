//! Per-OS seam. Everything the install/uninstall/daemon/agent plane needs that differs by
//! operating system lives behind these free functions; a `#[cfg(target_os = …)]`-selected
//! submodule provides the implementation:
//!
//!   * **service management** — write/start/stop/restart/status the background daemon
//!     (macOS: a launchd per-user LaunchAgent; Linux: a `systemd --user` unit; other: stub),
//!   * **quarantine** — the macOS Gatekeeper `com.apple.quarantine` probe (no-op elsewhere),
//!   * **app bundle** — the installed `.app` location (macOS only),
//!   * **logs dir** — where the daemon's stdout/stderr logs live.
//!
//! The rest of `hl` calls `crate::platform::…`; it never touches `launchctl`, `systemctl`,
//! `xattr`, the plist, or `/Applications/dd.app` directly.

use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as imp;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as imp;

// Windows (and any other non-macOS/Linux target) gets the graceful stub.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod windows;
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
use windows as imp;

// ── Service management ────────────────────────────────────────────────────────

/// Write the service unit (plist / systemd unit) and the `~/.dd` state tree; does **not**
/// start it. Returns the path of the unit file written (for user-facing output).
pub fn service_write() -> std::io::Result<PathBuf> {
    imp::service_write()
}

/// Write the unit if missing, then start (load/bootstrap/enable) the service. Idempotent.
pub fn service_ensure() -> Result<(), String> {
    imp::service_ensure()
}

/// Stop (unload) the service but leave its unit file in place.
pub fn service_stop() -> std::io::Result<()> {
    imp::service_stop()
}

/// Restart the running service.
pub fn service_restart() -> std::io::Result<()> {
    imp::service_restart()
}

/// Print the service status to our stdout; returns whether the service is loaded/active.
pub fn service_status() -> std::io::Result<bool> {
    imp::service_status()
}

/// Stop the service and remove its unit file (the uninstall path).
pub fn service_remove() -> std::io::Result<()> {
    imp::service_remove()
}

/// A human-readable identifier for the service (shown in install output).
pub fn service_label() -> String {
    imp::service_label()
}

// ── Quarantine ────────────────────────────────────────────────────────────────

/// True when `p` carries the macOS Gatekeeper quarantine xattr. Always `false` off macOS.
pub fn is_quarantined(p: &Path) -> bool {
    imp::is_quarantined(p)
}

// ── App bundle ────────────────────────────────────────────────────────────────

/// The installed GUI `.app` bundle location, or `None` on platforms without one.
pub fn app_bundle() -> Option<PathBuf> {
    imp::app_bundle()
}

// ── Platform paths ────────────────────────────────────────────────────────────

/// Where the daemon's stdout/stderr logs live (macOS `~/Library/Logs/dd`, Linux `~/.dd/logs`).
pub fn logs_dir() -> PathBuf {
    imp::logs_dir()
}
