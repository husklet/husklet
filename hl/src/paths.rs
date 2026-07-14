//! Canonical filesystem locations for a dd install. Everything lives under the user's `$HOME`
//! so install/uninstall never needs root. The socket sits at `~/.dd/run/docker.sock` — short and
//! space-free so it stays under the ~104-byte `sun_path` limit (never under "Application Support").

use std::path::PathBuf;

/// The base service label/id for the per-user daemon (launchd label on macOS, the systemd unit
/// stem on Linux).
pub const AGENT_LABEL: &str = "com.dd.daemon";

/// `$HOME`, or `.` as a last resort.
pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `~/.dd` — state root (images, volumes, state.json, run/).
pub fn hl_root() -> PathBuf {
    home().join(".dd")
}

/// `~/.dd/run` — runtime dir holding the socket.
pub fn run_dir() -> PathBuf {
    hl_root().join("run")
}

/// `~/.dd/run/docker.sock` — the daemon's listen socket (== `HL_DOCKER_SOCK`).
pub fn socket() -> PathBuf {
    run_dir().join("docker.sock")
}

/// `~/.dd/images` — image rootfs dirs (== `HL_IMAGES`).
pub fn images_dir() -> PathBuf {
    hl_root().join("images")
}

/// Daemon stdout/stderr logs dir. Platform-specific (macOS `~/Library/Logs/dd`, Linux `~/.dd/logs`).
pub fn logs_dir() -> PathBuf {
    crate::platform::logs_dir()
}

/// The `dd-daemon` binary the agent should launch. Order: `$HL_DAEMON_BIN`, the installed app
/// bundle (macOS only), then a binary sitting next to this `dd` executable (the dev/`cargo` layout).
pub fn daemon_bin() -> PathBuf {
    if let Some(p) = std::env::var_os("HL_DAEMON_BIN") {
        return PathBuf::from(p);
    }
    if let Some(bundle) = crate::platform::app_bundle() {
        let bundled = bundle.join("Contents/Resources/dd-daemon");
        if bundled.exists() {
            return bundled;
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sib = dir.join("dd-daemon");
            if sib.exists() {
                return sib;
            }
        }
    }
    // Fallback: the bundle path on macOS, else a `~/.dd`-relative location.
    crate::platform::app_bundle()
        .map(|b| b.join("Contents/Resources/dd-daemon"))
        .unwrap_or_else(|| hl_root().join("dd-daemon"))
}

/// `unix://<socket>` — the DOCKER_HOST / docker-context endpoint.
pub fn docker_host() -> String {
    format!("unix://{}", socket().display())
}
