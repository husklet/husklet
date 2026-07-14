//! Linux platform impl. Service management is a `systemd --user` unit (no root): it writes
//! `~/.config/systemd/user/com.hl.daemon.service` with the same daemon exec + env as the
//! macOS LaunchAgent (`HL_DOCKER_SOCK` / `HL_IMAGES` / `HL_JIT_DIR` — names unchanged) and drives
//! it with `systemctl --user`. There is no Gatekeeper and no `.app` bundle on Linux, so
//! quarantine is always `false` and `app_bundle()` is `None`. Logs live under `~/.hl/logs`.
//!
//! `hl daemon run` (foreground exec) always works regardless of systemd; these functions only
//! cover the managed start/stop/restart/status lifecycle.

use crate::paths;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

/// The `systemd --user` unit name, e.g. `com.hl.daemon.service`.
fn unit_name() -> String {
    format!("{}.service", paths::AGENT_LABEL)
}

/// `~/.config/systemd/user/com.hl.daemon.service`.
fn unit_path() -> PathBuf {
    paths::home()
        .join(".config/systemd/user")
        .join(unit_name())
}

/// Render the systemd user unit. Mirrors the macOS plist: same ExecStart + environment.
fn render_unit() -> String {
    let daemon = paths::daemon_bin();
    // The JIT binaries (hljit-*) live next to the daemon.
    let jit_dir = daemon
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(paths::hl_root);
    let sock = paths::socket();
    let images = paths::images_dir();
    let out = logs_dir().join("daemon.out.log");
    let err = logs_dir().join("daemon.err.log");
    format!(
        "[Unit]\n\
         Description=hl VM-less container daemon\n\
         \n\
         [Service]\n\
         ExecStart={daemon}\n\
         Environment=HL_DOCKER_SOCK={sock}\n\
         Environment=HL_IMAGES={images}\n\
         Environment=HL_JIT_DIR={jit_dir}\n\
         Restart=always\n\
         StandardOutput=append:{out}\n\
         StandardError=append:{err}\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        daemon = daemon.display(),
        sock = sock.display(),
        images = images.display(),
        jit_dir = jit_dir.display(),
        out = out.display(),
        err = err.display(),
    )
}

// ── Seam surface ──────────────────────────────────────────────────────────────

/// Create the `~/.hl` tree and write the systemd unit (does not start it). Returns the unit path.
pub fn service_write() -> std::io::Result<PathBuf> {
    for d in [
        paths::run_dir(),
        paths::images_dir(),
        paths::hl_root().join("volumes"),
        logs_dir(),
    ] {
        std::fs::create_dir_all(&d)?;
    }
    let unit = unit_path();
    if let Some(parent) = unit.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::File::create(&unit)?;
    f.write_all(render_unit().as_bytes())?;
    let _ = run("systemctl", &["--user", "daemon-reload"]);
    Ok(unit)
}

/// Write the unit (if missing), reload, then `systemctl --user enable --now`.
pub fn service_ensure() -> Result<(), String> {
    if !unit_path().exists() {
        service_write().map_err(|e| e.to_string())?;
    } else {
        let _ = run("systemctl", &["--user", "daemon-reload"]);
    }
    run("systemctl", &["--user", "enable", "--now", &unit_name()]).map_err(|e| e.to_string())
}

/// `systemctl --user stop` (leave the unit installed).
pub fn service_stop() -> std::io::Result<()> {
    run("systemctl", &["--user", "stop", &unit_name()])
}

/// `systemctl --user restart`.
pub fn service_restart() -> std::io::Result<()> {
    run("systemctl", &["--user", "restart", &unit_name()])
}

/// `systemctl --user status` (streamed to our stdout); returns whether the unit is active.
pub fn service_status() -> std::io::Result<bool> {
    let st = Command::new("systemctl")
        .args(["--user", "status", &unit_name()])
        .status()?;
    Ok(st.success())
}

/// `systemctl --user disable --now`, remove the unit file, reload.
pub fn service_remove() -> std::io::Result<()> {
    let _ = run("systemctl", &["--user", "disable", "--now", &unit_name()]);
    let _ = std::fs::remove_file(unit_path());
    let _ = run("systemctl", &["--user", "daemon-reload"]);
    Ok(())
}

pub fn service_label() -> String {
    format!("{} (systemd --user)", unit_name())
}

/// No Gatekeeper on Linux.
pub fn is_quarantined(_p: &std::path::Path) -> bool {
    false
}

/// No `.app` bundle on Linux.
pub fn app_bundle() -> Option<PathBuf> {
    None
}

/// `~/.hl/logs` — daemon stdout/stderr logs.
pub fn logs_dir() -> PathBuf {
    paths::hl_root().join("logs")
}

fn run(cmd: &str, args: &[&str]) -> std::io::Result<()> {
    let out = Command::new(cmd).args(args).output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "{cmd} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}
