//! Launch a workspace's image as a real container **in-process via dd-jit** — no daemon, no `docker`,
//! no socket. `dd_jit::Runtime::start` forks the linked engine and gives us the guest's PTY directly;
//! `dd_images::Store` resolves (and pulls, if missing) the image rootfs; a per-workspace persistent
//! overlay upper makes it a dev environment you return to.
//!
//! [`DdJitPty`] adapts the async `RunningContainer` to the synchronous [`PtyBackend`] the CLI runner
//! drives: a background multi-thread tokio runtime keeps dd-jit's IO pumps alive, output is drained
//! from its broadcast, and `write_stdin`/`resize`/`waitpid` are plain synchronous calls.

use crate::paths;
use dd_term_core::workspace::{Arch, Workspace};
use dd_term_core::PtyBackend;
use std::collections::VecDeque;
use std::io;
use std::os::unix::io::RawFd;

fn to_io<E: std::fmt::Display>(e: E) -> io::Error {
    io::Error::new(io::ErrorKind::Other, e.to_string())
}

fn guest_of(arch: Arch) -> dd_jit::Guest {
    match arch {
        Arch::Arm64 => dd_jit::Guest::LinuxAarch64,
        Arch::Amd64 => dd_jit::Guest::LinuxX86_64,
        Arch::DarwinArm64 => dd_jit::Guest::DarwinAarch64,
    }
}

/// The image target we require for a workspace's arch — used to pull the RIGHT variant and to verify
/// the on-disk rootfs really is that ISA before we hand it to an engine.
fn want_arch(arch: Arch) -> dd_images::Arch {
    match arch {
        Arch::Arm64 => dd_images::Arch::LinuxAarch64,
        Arch::Amd64 => dd_images::Arch::LinuxX86_64,
        Arch::DarwinArm64 => dd_images::Arch::DarwinAarch64,
    }
}

/// Split `image` into `(repository, tag)`, defaulting the tag to `latest`.
fn split_ref(image: &str) -> (String, String) {
    // Only split on a ':' AFTER the last '/', so a registry host:port isn't mistaken for a tag.
    let last_slash = image.rfind('/').map(|i| i + 1).unwrap_or(0);
    match image[last_slash..].rfind(':') {
        Some(rel) => {
            let at = last_slash + rel;
            (image[..at].to_string(), image[at + 1..].to_string())
        }
        None => (image.to_string(), "latest".to_string()),
    }
}

/// Launch `ws` as an in-process dd-jit container and return a [`PtyBackend`] over its shell. Errors
/// (including "this host's engine can't run that arch") let the caller fall back to a local shell.
/// Launch (or, when `restore`, RESUME from the last whole-workspace checkpoint) `ws`. Checkpointing is always
/// armed: the engine's `DDJIT_CHECKPOINT_DIR` is exported (inherited by the posix_spawn'd engine and every
/// guest process it forks), so the running tree can be frozen later with `Runtime::checkpoint`. When
/// `restore`, `DDJIT_RESTORE_DIR` is set too, so the engine rebuilds the saved process tree instead of
/// starting a fresh shell. The container init's host pid is recorded so a separate `workspace checkpoint`
/// invocation can signal the live tree.
pub fn launch_ex(ws: &Workspace, cols: u16, rows: u16, restore: bool) -> io::Result<Box<dyn PtyBackend>> {
    let guest = guest_of(ws.arch);
    // Deterministic high placement + fixed image bases (required for a restore's MAP_FIXED to land on free
    // VAs) need the persistent translated-code cache ON, so give the runtime a per-workspace cache dir.
    let ckpt_dir = ws.checkpoint_dir(&paths::dd_root());
    let pcache_dir = ws.storage_dir(&paths::dd_root()).join("pcache");
    let _ = std::fs::create_dir_all(&pcache_dir);
    if let Some(p) = ckpt_dir.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    // Arm checkpoint/restore for the engine this process spawns (ffi.c execve()s with our environ). Each
    // `ddcli workspace launch` is its own process, so these process-global vars never race across workspaces.
    std::env::set_var("DDJIT_CHECKPOINT_DIR", &ckpt_dir);
    if restore && ckpt_dir.join("MANIFEST").exists() {
        std::env::set_var("DDJIT_RESTORE_DIR", &ckpt_dir);
    } else {
        std::env::remove_var("DDJIT_RESTORE_DIR");
        if restore {
            eprintln!("[dd] no checkpoint to restore for {:?}; starting a fresh workspace", ws.name);
        }
    }
    let rt = dd_jit::Runtime::new().map_err(to_io)?.cache_dir(pcache_dir.to_string_lossy().into_owned());
    if !rt.supports(guest) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("no engine for {} on this host", ws.arch.as_str()),
        ));
    }

    // Resolve the image rootfs, pulling on first use. The store keys a rootfs by image ref ONLY (no
    // arch), so an `alpine` pulled as arm64 elsewhere would otherwise be reused for an amd64 workspace
    // and fed to the x86 engine (→ `e_machine` mismatch). Guard against that two ways: give each arch
    // its own store dir, and verify the unpacked rootfs's real ISA matches before use.
    let want = want_arch(ws.arch);
    let images_dir = paths::images_dir().join(ws.arch.as_str()).to_string_lossy().into_owned();
    let store = dd_images::Store::new(&images_dir);
    let (from, tag) = split_ref(&ws.image);
    let iref = dd_images::image_ref(&from, &tag);
    let rootfs_pb = store.rootfs_path(&iref);
    let present_ok =
        rootfs_pb.is_dir() && dd_images::detect_arch(&rootfs_pb).map(|a| a == want).unwrap_or(false);
    if !present_ok {
        eprintln!("[dd] pulling {} ({}) …", ws.image, ws.arch.as_str());
        store
            .pull_archs(&from, &tag, dd_images::Credentials::none(), &[want.oci().1], &mut |_| {})
            .map_err(to_io)?;
    }
    let rootfs = rootfs_pb.to_string_lossy().into_owned();

    // Per-workspace persistent writable upper: the dev environment that survives across launches.
    let upper_pb = ws.upper_dir(&paths::dd_root());
    std::fs::create_dir_all(&upper_pb)?;
    let upper = upper_pb.to_string_lossy().into_owned();

    // Build the container: the persistent upper overlays the image rootfs; a FORCED-interactive login
    // shell (bash if present, else sh) with a real controlling PTY; a private loopback keyed by the
    // workspace. `-i` forces the prompt even though our parent `sh -c` is non-interactive.
    let image = dd_jit::Image::overlay(upper, [rootfs]).guest(guest);
    // Pick the shell WITHOUT redirecting the final exec's stderr: interactive bash decides it's
    // interactive from isatty(stderr) AND writes its prompt (PS1) to stderr, so a `2>/dev/null` would
    // silently make it non-interactive with a hidden prompt (looks hung).
    // Honor a configured shell; else auto-pick bash (interactive login) then sh.
    let inner = match ws.shell.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => format!("exec {s}"),
        None => "if command -v bash >/dev/null 2>&1; then exec bash -il; else exec sh -i; fi".to_string(),
    };
    let shell = vec!["/bin/sh".to_string(), "-c".to_string(), inner];

    let mut env = vec![
        "TERM=xterm-256color".to_string(),
        "HOME=/root".to_string(),
        "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string(),
    ];
    // The workspace's configured environment variables.
    for (k, v) in &ws.env {
        env.push(format!("{k}={v}"));
    }

    let mut builder = dd_jit::Container::builder(image)
        .cmd(shell)
        .cwd("/root".to_string())
        .guest_env(&env, true)
        .hostname(sanitize_host(&ws.name))
        .private_network(format!("ws-{}", sanitize_host(&ws.name)));
    // Configured bind mounts + resource caps from the workspace definition.
    for m in &ws.mounts {
        builder = builder.bind(m.host.clone(), m.container.clone(), m.ro);
    }
    // Mount the workspace's ISOLATED daemon socket so the normal `docker` CLI works inside. Bind at the
    // canonical /run/docker.sock (in the image /var/run is a symlink to /run).
    if ws.docker_sock {
        if let Ok(sock) = crate::wsdaemon::ensure(&ws.name) {
            builder = builder.bind(sock.to_string_lossy().into_owned(), "/run/docker.sock".to_string(), false);
            env.push("DOCKER_HOST=unix:///run/docker.sock".to_string());
            // Inject a static `docker` CLI (matching the workspace arch) so it works even in a bare image.
            let docker_bin = paths::dd_root().join("bin").join(match ws.arch {
                Arch::Amd64 => "docker-amd64",
                _ => "docker-arm64",
            });
            if docker_bin.exists() {
                builder = builder.bind(docker_bin.to_string_lossy().into_owned(), "/usr/local/bin/docker".to_string(), true);
            }
            builder = builder.guest_env(&env, true); // re-apply so DOCKER_HOST is included
        }
    }
    if let Some(c) = ws.cpus {
        builder = builder.cpus(c);
    }
    if let Some(mb) = ws.memory_mb {
        builder = builder.memory_mb(mb as u64);
    }
    let container = builder.build().map_err(to_io)?;

    // dd-jit's start_into() spawns tokio IO pumps, so it must run inside a runtime; keep that runtime
    // alive in the handle so the pumps keep feeding the broadcast we drain synchronously.
    let trt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;

    // CRITICAL: subscribe to the output BEFORE launching so the shell's first prompt (emitted the instant
    // the guest starts) is never lost — otherwise the terminal shows only the banner and looks hung.
    let (out, rx) = {
        let (tx, rx) = tokio::sync::broadcast::channel::<(u8, Vec<u8>)>(4096);
        (tx, rx)
    };
    let log_chunks = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let (stdin_tx, stdin_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(256);
    let launched = trt
        .block_on(async { rt.start_into(&container, dd_jit::Stdio3 { tty: true }, out, log_chunks, stdin_rx) })
        .map_err(to_io)?;

    // Record the container init's host pid so a separate `workspace checkpoint` can signal the live tree.
    let _ = std::fs::write(ws.checkpoint_pid_file(&paths::dd_root()), launched.pid.to_string());

    let mut pty = DdJitPty {
        _rt: trt,
        stdin_tx,
        rx,
        master: launched.pty_master,
        pid: launched.pid as libc::pid_t,
        pending: VecDeque::new(),
        exited: None,
    };
    pty.resize(cols, rows);
    Ok(Box::new(pty))
}

/// Sanitize a workspace name into a hostname/netns-safe token.
fn sanitize_host(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect();
    let t = s.trim_matches('-');
    if t.is_empty() { "workspace".to_string() } else { t.to_string() }
}

/// A synchronous [`PtyBackend`] over a dd-jit-launched container: output drained from the pre-subscribed
/// broadcast, input pushed to the guest stdin channel, resize/reap via the master fd + pid.
struct DdJitPty {
    /// Kept alive so dd-jit's IO pump tasks keep running (they feed the broadcast we drain).
    _rt: tokio::runtime::Runtime,
    stdin_tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    rx: tokio::sync::broadcast::Receiver<(u8, Vec<u8>)>,
    master: Option<RawFd>,
    pid: libc::pid_t,
    /// Bytes received from the broadcast that didn't fit the last `read` buffer.
    pending: VecDeque<u8>,
    exited: Option<i32>,
}

impl PtyBackend for DdJitPty {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        let _ = self.stdin_tx.try_send(bytes.to_vec());
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        use tokio::sync::broadcast::error::TryRecvError;
        let mut n = 0;
        while n < buf.len() {
            if let Some(b) = self.pending.pop_front() {
                buf[n] = b;
                n += 1;
                continue;
            }
            match self.rx.try_recv() {
                Ok((_stream, bytes)) => self.pending.extend(bytes),
                Err(TryRecvError::Empty) | Err(TryRecvError::Closed) => break,
                Err(TryRecvError::Lagged(_)) => continue, // dropped under burst; keep draining
            }
        }
        Ok(n)
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        if let Some(fd) = self.master {
            let ws = libc::winsize { ws_row: rows.max(1), ws_col: cols.max(1), ws_xpixel: 0, ws_ypixel: 0 };
            unsafe {
                libc::ioctl(fd, libc::TIOCSWINSZ, &ws);
            }
        }
    }

    fn master_fd(&self) -> Option<RawFd> {
        None // output is drained from the broadcast, not the fd (dd-jit's pump owns the fd)
    }

    fn try_wait(&mut self) -> Option<i32> {
        if self.exited.is_some() {
            return self.exited;
        }
        let mut status: libc::c_int = 0;
        let r = unsafe { libc::waitpid(self.pid, &mut status, libc::WNOHANG) };
        if r == self.pid {
            let code = if libc::WIFEXITED(status) {
                libc::WEXITSTATUS(status)
            } else if libc::WIFSIGNALED(status) {
                128 + libc::WTERMSIG(status)
            } else {
                -1
            };
            self.exited = Some(code);
        }
        self.exited
    }
}

impl Drop for DdJitPty {
    fn drop(&mut self) {
        // Stop the guest's process group (pid == pgid); the pumps end when the PTY closes. ESRCH (already
        // gone) is fine.
        if self.exited.is_none() {
            unsafe {
                libc::killpg(self.pid, libc::SIGHUP);
            }
        }
    }
}
