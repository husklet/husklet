//! Per-workspace dd-daemon: each workspace gets its OWN isolated Docker-API daemon (own socket, state,
//! and volumes under `~/.dd/ws/<name>/`), so `docker` inside the workspace — and the dashboard reading
//! it — see only that workspace's containers. The image cache is shared (images are immutable).
//!
//! [`ensure`] is idempotent: it returns the socket path, starting the daemon (detached) on first use.

use crate::paths;
use std::path::PathBuf;

/// Per-workspace state directory `~/.dd/ws/<name>`.
pub fn ws_dir(name: &str) -> PathBuf {
    paths::dd_root().join("ws").join(sanitize(name))
}

/// The workspace daemon's listen socket.
pub fn socket(name: &str) -> PathBuf {
    ws_dir(name).join("docker.sock")
}

/// Ensure the workspace's daemon is running and return its socket path. Idempotent — a live socket is
/// reused; otherwise the daemon is spawned detached and we wait (briefly) for it to listen.
pub fn ensure(name: &str) -> std::io::Result<PathBuf> {
    let dir = ws_dir(name);
    std::fs::create_dir_all(dir.join("volumes"))?;
    std::fs::create_dir_all(dir.join("images"))?;
    let sock = socket(name);

    if is_up(&sock) {
        return Ok(sock);
    }
    // Stale socket file from a dead daemon — remove so bind() succeeds.
    let _ = std::fs::remove_file(&sock);

    let bin = daemon_bin();
    if !bin.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("dd-daemon binary not found at {} (set DD_DAEMON_BIN)", bin.display()),
        ));
    }

    let log = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("daemon.log"))?;
    let errlog = log.try_clone()?;
    let mut cmd = std::process::Command::new(&bin);
    cmd.env("DDOCKERD_SOCK", &sock)
        .env("DD_IMAGES", dir.join("images")) // per-workspace image store (fully isolated)
        .env("DD_STATE", dir.join("state.json")) // per-workspace container state
        .env("DD_VOLUMES", dir.join("volumes")) // per-workspace volumes
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(errlog));
    // Where the ddjit-* engines live (needed to actually run containers): DD_ENGINE_DIR, else next
    // to the daemon binary.
    let engine_dir = std::env::var_os("DD_ENGINE_DIR").map(PathBuf::from).or_else(|| bin.parent().map(|p| p.to_path_buf()));
    if let Some(d) = engine_dir {
        cmd.env("DDJIT_DIR", d);
    }
    // Detach into its own session so it outlives the launching command.
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn()?;
    // Do NOT block the caller waiting for the socket — the daemon takes a few seconds to init its JIT
    // engines + discover images, and blocking here stalls every shell launch. Return the path now; the
    // docker-socket bind + the dashboard connect it once it's up (the poller started it on window-open).
    Ok(sock)
}

/// Is a daemon actually listening on `sock`? (socket file exists AND accepts a connection).
fn is_up(sock: &std::path::Path) -> bool {
    if !sock.exists() {
        return false;
    }
    std::os::unix::net::UnixStream::connect(sock).is_ok()
}

/// Resolve the dd-daemon binary: `DD_DAEMON_BIN`, else the installed bundle path.
fn daemon_bin() -> PathBuf {
    if let Some(p) = std::env::var_os("DD_DAEMON_BIN") {
        return PathBuf::from(p);
    }
    paths::daemon_bin()
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}
