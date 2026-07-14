//! Managing the hl-daemon process: start our bundled copy, stop an external LaunchAgent, and
//! resolve the daemon binary (+ the dir holding the `hljit-*` engines).

use std::path::PathBuf;

/// Spawn the hl-daemon binary detached, with the canonical socket/images/JIT env, and return the
/// child so we can stop it later. Resolves the binary from `$HL_DAEMON_BIN`, the app bundle
/// (`Contents/Resources/hl-daemon`), or a sibling of this executable (the dev/`cargo` layout).
pub(crate) fn spawn_daemon() -> Option<std::process::Child> {
    use std::process::{Command, Stdio};
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let hl = PathBuf::from(&home).join(".hl");
    let run = hl.join("run");
    let images = hl.join("images");
    let _ = std::fs::create_dir_all(&run);
    let _ = std::fs::create_dir_all(&images);

    // Send the daemon's output to the same log file the LaunchAgent uses, so the System view can tail
    // what the daemon is logging regardless of how it was started.
    let logs = PathBuf::from(&home).join("Library/Logs/hl");
    let _ = std::fs::create_dir_all(&logs);
    let log = |name: &str| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(logs.join(name))
            .ok()
    };
    let (out, err): (Stdio, Stdio) = match (log("daemon.out.log"), log("daemon.err.log")) {
        (Some(o), Some(e)) => (o.into(), e.into()),
        _ => (Stdio::null(), Stdio::null()),
    };

    let (bin, jit_dir) = resolve_daemon()?;
    Command::new(&bin)
        .env("HL_DOCKER_SOCK", run.join("docker.sock"))
        .env("HL_IMAGES", &images)
        .env("HL_JIT_DIR", &jit_dir)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn()
        .ok()
}

/// Stop a daemon we didn't start: if it's an installed LaunchAgent, bootout the per-user service.
pub(crate) fn stop_external_daemon() {
    extern "C" {
        fn getuid() -> u32;
    }
    let uid = unsafe { getuid() };
    let _ = std::process::Command::new("launchctl")
        .args(["bootout", &format!("gui/{uid}/com.hl.daemon")])
        .output();
}

/// Locate the daemon binary and the dir holding the `hljit-*` engines.
fn resolve_daemon() -> Option<(PathBuf, PathBuf)> {
    if let Some(p) = std::env::var_os("HL_DAEMON_BIN") {
        let p = PathBuf::from(p);
        let dir = p.parent().map(|d| d.to_path_buf()).unwrap_or_default();
        return Some((p, dir));
    }
    let exe = std::env::current_exe().ok()?;
    // Bundle: .../Contents/MacOS/hl-app -> .../Contents/Resources/hl-daemon
    if let Some(contents) = exe.parent().and_then(|p| p.parent()) {
        let res = contents.join("Resources");
        let cand = res.join("hl-daemon");
        if cand.exists() {
            return Some((cand, res));
        }
    }
    // Dev: a hl-daemon next to this binary (hljit-* paths are baked in at compile time there).
    if let Some(dir) = exe.parent() {
        let cand = dir.join("hl-daemon");
        if cand.exists() {
            return Some((cand, dir.to_path_buf()));
        }
    }
    None
}
