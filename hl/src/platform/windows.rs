//! Windows (and any other non-macOS/Linux target) stub. `hl` has no service-management backend
//! here yet: the lifecycle calls return an informative "unsupported" error, quarantine is always
//! `false`, and there is no `.app` bundle. `hl daemon run` (foreground exec) still works — only
//! the managed start/stop/restart/status lifecycle is unavailable. Logs live under `~/.dd/logs`.

use std::path::PathBuf;

const UNSUPPORTED: &str = "daemon service management is not supported on this platform";

fn unsupported_io() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Unsupported, UNSUPPORTED)
}

pub fn service_write() -> std::io::Result<PathBuf> {
    Err(unsupported_io())
}

pub fn service_ensure() -> Result<(), String> {
    Err(UNSUPPORTED.to_string())
}

pub fn service_stop() -> std::io::Result<()> {
    Err(unsupported_io())
}

pub fn service_restart() -> std::io::Result<()> {
    Err(unsupported_io())
}

pub fn service_status() -> std::io::Result<bool> {
    Err(unsupported_io())
}

pub fn service_remove() -> std::io::Result<()> {
    Err(unsupported_io())
}

pub fn service_label() -> String {
    "unsupported".to_string()
}

pub fn is_quarantined(_p: &std::path::Path) -> bool {
    false
}

pub fn app_bundle() -> Option<PathBuf> {
    None
}

/// `~/.dd/logs` — daemon stdout/stderr logs.
pub fn logs_dir() -> PathBuf {
    crate::paths::hl_root().join("logs")
}
