//! `hl run` / `hl <image>` — launch a container with *easy-access* defaults:
//! the current directory mounted at the same path and used as the working dir, host networking, and an
//! interactive shell when no command is given. We drive the hl daemon through the stock `docker` CLI
//! (pointed at dd's socket), so the streaming/TTY behaviour is exactly docker's.

use std::io::IsTerminal;
use std::process::Command;

use crate::agent;
use crate::paths;
use crate::run;

/// A parsed `run` invocation. `hl run …` and the bare-image shorthand `hl <image> …` both parse
/// into this via [`parse`].
pub struct RunArgs {
    /// `--platform linux/amd64` etc.; `None` = native (arm64).
    pub platform: Option<String>,
    /// `--isolated`: skip the automatic cwd mount + host networking.
    pub isolated: bool,
    /// `--keep`: don't remove the container when it exits (default is `--rm`).
    pub keep: bool,
    pub image: String,
    /// Command to run instead of the image default (an interactive shell).
    pub command: Vec<String>,
}

/// Parse `[--platform P] [--isolated] [--keep] <image> [command…]`. hl's own flags are recognized
/// wherever they appear (before or after the image, matching the casual `hl run ubuntu --platform …`);
/// the first remaining token is the image and the rest are the command.
pub fn parse(raw: Vec<String>) -> Result<RunArgs, String> {
    let (mut platform, mut isolated, mut keep) = (None, false, false);
    let mut rest = Vec::new();
    let mut it = raw.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--isolated" => isolated = true,
            "--keep" => keep = true,
            "--platform" => platform = it.next(),
            s if s.starts_with("--platform=") => {
                platform = Some(s["--platform=".len()..].to_string())
            }
            _ => rest.push(a),
        }
    }
    let mut rest = rest.into_iter();
    let image = rest.next().ok_or("usage: hl run <image> [command…]")?;
    Ok(RunArgs {
        platform,
        isolated,
        keep,
        image,
        command: rest.collect(),
    })
}

/// Run a container with the easy-access defaults, by invoking `docker` against dd's socket.
pub fn run(args: RunArgs) -> i32 {
    if !docker_present() {
        eprintln!(
            "hl needs the `docker` CLI on PATH — it drives the hl daemon. Install Docker's CLI."
        );
        return 1;
    }
    if let Err(e) = ensure_daemon() {
        eprintln!("hl daemon isn't reachable: {e}\nTry:  hl install");
        return 1;
    }
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "/".into());

    let mut cmd = Command::new("docker");
    cmd.arg("--host").arg(paths::docker_host()).arg("run");
    if !args.keep {
        cmd.arg("--rm");
    }
    // Always attach stdin; allocate a TTY only when we actually have one (so pipes still work).
    cmd.arg("-i");
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        cmd.arg("-t");
    }
    // Easy access: the current directory is the container's working dir, mounted at the same path, on the
    // host network. `--isolated` opts out for a clean, sandboxed run.
    if !args.isolated {
        cmd.arg("--network").arg("host");
        cmd.arg("-v").arg(format!("{cwd}:{cwd}"));
        cmd.arg("-w").arg(&cwd);
    }
    if let Some(p) = &args.platform {
        cmd.arg("--platform").arg(p);
    }
    cmd.arg(&args.image);
    if args.command.is_empty() {
        // No command given → an interactive shell. Prefer bash when the image has it (a nicer dev
        // shell: history, line editing, completion), falling back to sh — resolved INSIDE the
        // container since we can't see its filesystem from here. The fallback is an ABSOLUTE path
        // (`/bin/sh`) so it resolves even when bare `sh` isn't on PATH.
        cmd.args([
            "/bin/sh",
            "-c",
            "command -v bash >/dev/null 2>&1 && exec bash || exec /bin/sh",
        ]);
    } else {
        cmd.args(&args.command);
    }

    match cmd.status() {
        Ok(s) => s.code().unwrap_or(0),
        Err(e) => {
            eprintln!("failed to launch docker: {e}");
            1
        }
    }
}

fn docker_present() -> bool {
    Command::new("docker")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// `hl run …` and the bare-image shorthand both land here.
pub fn cmd_run(raw: Vec<String>) -> i32 {
    match run::parse(raw) {
        Ok(args) => run::run(args),
        Err(e) => {
            eprintln!("{e}");
            2
        }
    }
}

/// Ensure the daemon socket answers; if not, try to (re)start the agent and wait briefly for it.
fn ensure_daemon() -> Result<(), String> {
    let sock = paths::socket();
    if ping_socket(&sock) {
        return Ok(());
    }
    let _ = agent::ensure(); // best-effort: start the daemon service for this platform
    for _ in 0..40 {
        if ping_socket(&sock) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Err(format!("no daemon listening at {}", sock.display()))
}

/// Synchronously connect to the socket to confirm the daemon answers `_ping`. (Previously in the
/// removed `doctor` module; `run` is now its only caller.)
fn ping_socket(sock: &std::path::Path) -> bool {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(_) => return false,
    };
    rt.block_on(async {
        let c = hl_client::Client::new(sock);
        c.ping().await.is_ok()
    })
}
