//! macOS platform impl. Service management is the per-user launchd LaunchAgent (no root):
//! it writes `~/Library/LaunchAgents/com.dd.daemon.plist` and drives it with the modern
//! `launchctl bootstrap/bootout/kickstart/print` API in the per-user GUI domain `gui/<uid>`.
//! Quarantine is the Gatekeeper `com.apple.quarantine` xattr probe; the app bundle is
//! `/Applications/dd.app`; logs live under `~/Library/Logs/dd`.

use crate::paths;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

/// Installed app-bundle location (where `ddcli install` expects the signed `.app`).
const APP_BUNDLE: &str = "/Applications/dd.app";

// Avoid a libc dependency for one call.
extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

/// The `gui/<uid>` service target launchd uses for per-user agents.
fn domain_target() -> String {
    let uid = unsafe { libc_getuid() };
    format!("gui/{uid}")
}

/// The `gui/<uid>/com.dd.daemon` service name for kickstart/bootout/print.
fn service_target() -> String {
    format!("{}/{}", domain_target(), paths::AGENT_LABEL)
}

/// `~/Library/LaunchAgents/com.dd.daemon.plist`.
fn agent_plist() -> PathBuf {
    paths::home()
        .join("Library/LaunchAgents")
        .join(format!("{}.plist", paths::AGENT_LABEL))
}

/// Render the LaunchAgent plist. launchd does **not** expand `~`, so every path is absolute.
fn render_plist() -> String {
    let daemon = paths::daemon_bin();
    // The JIT binaries (ddjit-*) live next to the daemon inside the bundle's Resources dir.
    let jit_dir = daemon
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(paths::hl_root);
    let sock = paths::socket();
    let images = paths::images_dir();
    let out = logs_dir().join("daemon.out.log");
    let err = logs_dir().join("daemon.err.log");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>            <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{daemon}</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>HL_DOCKER_SOCK</key>  <string>{sock}</string>
    <key>HL_IMAGES</key>      <string>{images}</string>
    <key>HL_JIT_DIR</key>      <string>{jit_dir}</string>
  </dict>
  <key>RunAtLoad</key>        <true/>
  <key>KeepAlive</key>        <true/>
  <key>ProcessType</key>      <string>Interactive</string>
  <key>StandardOutPath</key>  <string>{out}</string>
  <key>StandardErrorPath</key><string>{err}</string>
</dict>
</plist>
"#,
        label = paths::AGENT_LABEL,
        daemon = daemon.display(),
        sock = sock.display(),
        images = images.display(),
        jit_dir = jit_dir.display(),
        out = out.display(),
        err = err.display(),
    )
}

// ── Seam surface ──────────────────────────────────────────────────────────────

/// Create the `~/.dd` tree and write the plist (does not load it). Returns the plist path.
pub fn service_write() -> std::io::Result<PathBuf> {
    for d in [
        paths::run_dir(),
        paths::images_dir(),
        paths::hl_root().join("volumes"),
        logs_dir(),
    ] {
        std::fs::create_dir_all(&d)?;
    }
    let plist = agent_plist();
    if let Some(parent) = plist.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::File::create(&plist)?;
    f.write_all(render_plist().as_bytes())?;
    Ok(plist)
}

/// Write the plist (if missing) and bootstrap the agent.
pub fn service_ensure() -> Result<(), String> {
    if !agent_plist().exists() {
        service_write().map_err(|e| e.to_string())?;
    }
    bootstrap().map_err(|e| e.to_string())
}

/// `launchctl bootstrap gui/<uid> <plist>` (load + start). Idempotent: a re-bootstrap of an
/// already-loaded agent is treated as success.
fn bootstrap() -> std::io::Result<()> {
    // Best-effort bootout first so re-installs pick up a changed plist.
    let _ = service_stop();
    let plist = agent_plist();
    run(
        "launchctl",
        &["bootstrap", &domain_target(), &plist.to_string_lossy()],
    )
}

/// `launchctl bootout gui/<uid>/com.dd.daemon` (stop + unload).
pub fn service_stop() -> std::io::Result<()> {
    run("launchctl", &["bootout", &service_target()])
}

/// `launchctl kickstart -k gui/<uid>/com.dd.daemon` (restart).
pub fn service_restart() -> std::io::Result<()> {
    run("launchctl", &["kickstart", "-k", &service_target()])
}

/// `launchctl print gui/<uid>/com.dd.daemon` (status, streamed to our stdout).
pub fn service_status() -> std::io::Result<bool> {
    let st = Command::new("launchctl")
        .args(["print", &service_target()])
        .status()?;
    Ok(st.success())
}

/// Stop the agent and remove its plist.
pub fn service_remove() -> std::io::Result<()> {
    let _ = service_stop();
    let _ = std::fs::remove_file(agent_plist());
    Ok(())
}

/// The `gui/<uid>/com.dd.daemon` service target (shown in install output).
pub fn service_label() -> String {
    service_target()
}

pub fn is_quarantined(p: &std::path::Path) -> bool {
    Command::new("xattr")
        .arg("-p")
        .arg("com.apple.quarantine")
        .arg(p)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn app_bundle() -> Option<PathBuf> {
    Some(PathBuf::from(APP_BUNDLE))
}

/// `~/Library/Logs/dd` — daemon stdout/stderr logs.
pub fn logs_dir() -> PathBuf {
    paths::home().join("Library/Logs/dd")
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
