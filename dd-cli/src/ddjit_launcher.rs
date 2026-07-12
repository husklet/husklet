//! Launch a workspace's image as a real container **in-process via dd-jit** — no daemon, no `docker`,
//! no socket. `dd_jit::Runtime::start` forks the linked engine and gives us the guest's PTY directly;
//! `dd_images::Store` resolves (and pulls, if missing) the image rootfs; a per-workspace persistent
//! overlay upper makes it a dev environment you return to.
//!
//! [`DdJitPty`] adapts the async `RunningContainer` to the synchronous [`PtyBackend`] the CLI runner
//! drives: a background multi-thread tokio runtime keeps dd-jit's IO pumps alive, output is drained
//! from its broadcast, and `write_stdin`/`resize`/`waitpid` are plain synchronous calls.

use crate::paths;
use dd_jit::DeviceProvider;
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
pub fn launch_ex(ws: &Workspace, cols: u16, rows: u16, restore: bool, cwd: Option<&str>, slot: Option<&str>) -> io::Result<Box<dyn PtyBackend>> {
    let guest = guest_of(ws.arch);
    // Deterministic high placement + fixed image bases (required for a restore's MAP_FIXED to land on free
    // VAs) need the persistent translated-code cache ON, so give the runtime a per-workspace cache dir.
    // Each terminal pane is its OWN engine, so a per-pane SLOT freezes/restores into its own checkpoint
    // dir (`<storage>/checkpoint/<slot>`); None keeps the single shared slot (back-compat). The pcache is
    // a translation cache — safe (and cheaper) to SHARE across all of a workspace's slots.
    let (ckpt_dir, ckpt_pid_file) = match slot {
        Some(s) => (ws.checkpoint_slot_dir(&paths::dd_root(), s), ws.checkpoint_slot_pid_file(&paths::dd_root(), s)),
        None => (ws.checkpoint_dir(&paths::dd_root()), ws.checkpoint_pid_file(&paths::dd_root())),
    };
    let pcache_dir = ws.storage_dir(&paths::dd_root()).join("pcache");
    let _ = std::fs::create_dir_all(&pcache_dir);
    if let Some(p) = ckpt_dir.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    // Arm checkpoint/restore for the engine this process spawns (ffi.c execve()s with our environ). Each
    // `ddcli workspace launch` is its own process, so these process-global vars never race across workspaces.
    std::env::set_var("DDJIT_CHECKPOINT_DIR", &ckpt_dir);
    // The engine implements guest fork() as a real host fork() of a multithreaded, objc-using process
    // (Metal/IOSurface/NSString on the GPU path), and a guest execve() reloads the image IN-PLACE (no
    // host exec), so libobjc's initialize-fork-safety poison survives into every guest process. If a
    // guest fork races another thread's +initialize, the child later aborts on its first Foundation use
    // (objc_initializeAfterForkError) — e.g. a multiprocess Wayland client dies right after gl_shim
    // surface_up. The engine's process model requires the suppression; guarantee it here instead of relying on the launcher's
    // environment. libobjc reads it once at the engine's exec (ffi.c passes our environ). An explicit
    // caller-provided value (e.g. NO, to debug fork hygiene) is honored.
    if std::env::var_os("OBJC_DISABLE_INITIALIZE_FORK_SAFETY").is_none() {
        std::env::set_var("OBJC_DISABLE_INITIALIZE_FORK_SAFETY", "YES");
    }
    if restore && ckpt_dir.join("MANIFEST").exists() {
        std::env::set_var("DDJIT_RESTORE_DIR", &ckpt_dir);
    } else {
        std::env::remove_var("DDJIT_RESTORE_DIR");
        if restore {
            eprintln!("[dd] no checkpoint to restore for {:?}; starting a fresh workspace", ws.name);
        }
        // A FRESH launch (not resuming a saved tree): wipe any leftover checkpoint dir + control-channel
        // files for this slot so the new engine starts from a clean, self-contained slot and never inherits
        // a stale trigger generation or pid from a prior session. The engine re-creates the trigger fresh.
        let ckpt_str = ckpt_dir.to_string_lossy().into_owned();
        let _ = std::fs::remove_dir_all(&ckpt_dir);
        let _ = std::fs::remove_file(format!("{ckpt_str}.trigger"));
        let _ = std::fs::remove_file(format!("{ckpt_str}.pid"));
        // GUI reliability: a fresh `--gui` launch resets the persistent pcache first. The persistent
        // translated-code cache bakes host-arena-relative absolute addresses that are only valid when the
        // engine re-secures the SAME fixed arena base it was written at. A large C++/PIE Wayland binary
        // (e.g. glmark2 or a browser engine) that was cached in a PRIOR session can be re-loaded in a later
        // session at a MISMATCHED base (the fixed VA is occupied → NULL-hint fallback) → stale absolutes → an
        // intermittent SIGSEGV (exit 139) or garbage reads during EGL/config init, BEFORE any draw. This is
        // the "rendered one session, exit-139'd the next with no code change" flakiness. Building the cache
        // cold at the current session's base (then reusing it in-session, where the base stays available) is
        // 100% reliable and costs only a one-time re-translation at startup (negligible for an interactive
        // GUI app). A `restore` keeps the cache (its MAP_FIXED placement needs it) — this only fires on a
        // fresh gui launch. Opt out of the wipe (keep the persistent cache) with `DD_GUI_KEEP_PCACHE=1`.
        let keep_gui_pcache = std::env::var("DD_GUI_KEEP_PCACHE").ok().as_deref() == Some("1");
        if ws.gui && !keep_gui_pcache {
            let _ = std::fs::remove_dir_all(&pcache_dir);
            let _ = std::fs::create_dir_all(&pcache_dir);
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
    let base = match ws.shell.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => format!("exec {s}"),
        None => "if command -v bash >/dev/null 2>&1; then exec bash -il; else exec sh -i; fi".to_string(),
    };
    // OSC-7 "new tab in same cwd": start in `cwd` when the GUI passes one (a plain guest path). Guarded
    // with `2>/dev/null` so a stale/removed dir just falls back to the default working dir. Ignored on a
    // restore (the checkpoint already carries every process's cwd).
    let start_dir = cwd
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.starts_with('/') && !restore)
        .map(|s| s.to_string());
    let inner = match &start_dir {
        Some(dir) => format!("cd {} 2>/dev/null; {base}", shell_quote(dir)),
        None => base,
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
    // Generic Wayland/GL rendering diagnostics for a `--gui` workspace: opt-in HOST env knobs forwarded
    // into the guest only when set, so an ordinary launch keeps its configured environment byte-for-byte.
    // They are consumed by the injected GL shim (libEGL/libGLESv2) and libwayland — no application is
    // special-cased; any Wayland client honors them. App-specific launch tuning (flags, profile dirs,
    // timeouts) belongs in the workspace's own configured env, never hard-coded here.
    for k in ["WAYLAND_DEBUG", "DD_SHIM_DEBUG", "DD_SHADER_DUMP_DIR", "DD_TEXTURE_DUMP_DIR"] {
        if let Ok(v) = std::env::var(k) {
            env.push(format!("{k}={v}"));
        }
    }

    let mut builder = dd_jit::Container::builder(image)
        .cmd(shell)
        .cwd(start_dir.clone().unwrap_or_else(|| "/root".to_string()))
        .guest_env(&env, true)
        .hostname(sanitize_host(&ws.name))
        .private_network(format!("ws-{}", sanitize_host(&ws.name)));
    // Per-workspace VPN egress (docs/VPN.md): when the workspace is configured with a VPN that resolves to a
    // SOCKS5 endpoint, arm the engine's egress redirect so every genuine external TCP connect the guest makes
    // is funneled through that proxy. Absent a VPN (the default), nothing is set and the engine's direct
    // connect() path runs unchanged — zero overhead, no behavior change. Tunnel kinds (WireGuard/OpenVPN)
    // model a config path but need the userspace-tunnel helper to front them as SOCKS first (future `wsvpn`).
    if let Some(vpn) = &ws.vpn {
        match vpn.socks_endpoint() {
            Some(sock) => {
                builder = builder.egress_socks(sock.to_string());
            }
            None => eprintln!(
                "[dd] workspace {:?} VPN kind {:?} needs the userspace-tunnel helper (not yet wired); egress is direct",
                ws.name, vpn.kind
            ),
        }
    }
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
    // GPU integration (accelerated `--gui` display + simulated CUDA device) is now expressed to dd-jit
    // through a GENERIC device-provider seam (docs/ideas/{RENDERING_PLAN,CUDA_ON_METAL}.md): dd-cli
    // resolves *where the host artifacts live* (the `~/.dd/...` drop-ins, the socket paths, the guest ISA)
    // and hands a `dd_gpu::integration::GpuIntegration` to the dd-jit builder as a `DeviceProvider`. The
    // provider (in dd-gpu) owns *how a GPU maps into a guest* — target multiarch lib dir, guest socket
    // paths, the WAYLAND_DISPLAY/DD_GPU_EXEC/DD_CUDA_* env contract, the LD_LIBRARY_PATH composition, and
    // the render-node request — while dd-jit / the engine stay device-agnostic (they only see mounts +
    // env + a render-node bool; no CUDA/IOSurface/Wayland vocabulary crosses the runtime boundary). Inert
    // unless the workspace is `gui` and/or configures a `cuda` device → headless workspaces byte-identical.
    let mut gpu = dd_gpu::integration::GpuIntegration::new(match ws.arch {
        Arch::Amd64 => dd_gpu::integration::GuestArch::X86_64,
        _ => dd_gpu::integration::GuestArch::Aarch64,
    });
    if ws.gui {
        // Host socket paths: `DD_DISPLAY_SOCK`/`DD_GPU_EXEC_SOCK` overrides (tests / bespoke layouts), else
        // the per-session shared endpoints under the dd root (`dd-display`, spawned lazily elsewhere,
        // creates + listens on them). Drop-in dirs (client libs / demo bins) are passed only when present,
        // so a bare image needs no "dd-gpu image".
        let host_sock = std::env::var("DD_DISPLAY_SOCK")
            .unwrap_or_else(|_| paths::dd_root().join("run").join("wayland-0").to_string_lossy().into_owned());
        let host_gpu_sock = std::env::var("DD_GPU_EXEC_SOCK").unwrap_or_else(|_| {
            std::path::Path::new(&host_sock)
                .parent()
                .map(|d| d.join("dd-gpu.sock").to_string_lossy().into_owned())
                .unwrap_or_else(|| paths::dd_root().join("run").join("dd-gpu.sock").to_string_lossy().into_owned())
        });
        let gui_arch = match ws.arch {
            Arch::Amd64 => "x86_64",
            _ => "aarch64",
        };
        let host_gui_lib = paths::dd_root().join("gui").join(gui_arch).join("lib");
        let host_gui_bin = paths::dd_root().join("gui").join(gui_arch).join("bin");
        // The workspace overlay UPPER's copy of the guest multiarch lib dir, so the provider can self-heal
        // legacy overlays by pruning stale 0-byte inject stubs it left there (empty files first on
        // LD_LIBRARY_PATH shadow the guest's real base libs -> ENOEXEC / cold-Chrome EXIT=127). Always
        // passed (the dir may not exist yet on a fresh workspace — the provider's read_dir no-ops then).
        let overlay_multiarch = upper_pb.join("usr").join("lib").join(format!("{gui_arch}-linux-gnu"));
        gpu = gpu.with_display(dd_gpu::integration::DisplayIntegration {
            wayland_sock: host_sock,
            gpu_exec_sock: host_gpu_sock,
            // Pass a dir only when it exists, matching the original `is_dir()` gate (an empty string = no
            // drop-ins → the provider binds nothing and adds no LD_LIBRARY_PATH for that slot).
            lib_dir: if host_gui_lib.is_dir() { host_gui_lib.to_string_lossy().into_owned() } else { String::new() },
            bin_dir: if host_gui_bin.is_dir() { host_gui_bin.to_string_lossy().into_owned() } else { String::new() },
            overlay_lib_dir: overlay_multiarch.to_string_lossy().into_owned(),
        });
    }
    if let Some(cuda) = &ws.cuda {
        // Host-side artifacts follow the same drop-in pattern as the injected `docker` CLI:
        //   ~/.dd/nvml/<arch>/libnvidia-ml.so.1   (built by dd-gpu/nvml/build.sh install)
        //   ~/.dd/bin/nvidia-smi[-<arch>]         (the REAL NVIDIA binary; user-provided, never committed)
        // dd-cli resolves + existence-checks them (and warns exactly as before); dd-gpu injects whatever is
        // present (an empty path = not injected).
        let (nvml_arch, smi_arch) = match ws.arch {
            Arch::Amd64 => ("x86_64", "nvidia-smi-amd64"),
            _ => ("aarch64", "nvidia-smi-arm64"),
        };
        let dd = paths::dd_root();
        let nvml_pb = dd.join("nvml").join(nvml_arch).join("libnvidia-ml.so.1");
        let nvml_so = if nvml_pb.exists() {
            nvml_pb.to_string_lossy().into_owned()
        } else {
            eprintln!(
                "[dd] workspace {:?} has a CUDA device configured but the NVML shim is missing ({}); \
                 build it with dd-gpu/nvml/build.sh install. nvidia-smi will not find a GPU.",
                ws.name,
                nvml_pb.display()
            );
            String::new()
        };
        // Prefer the arch-specific nvidia-smi name, then a generic `nvidia-smi`. Never ship the closed binary.
        let smi_a = dd.join("bin").join(smi_arch);
        let smi_g = dd.join("bin").join("nvidia-smi");
        let nvidia_smi = if smi_a.exists() {
            smi_a.to_string_lossy().into_owned()
        } else if smi_g.exists() {
            smi_g.to_string_lossy().into_owned()
        } else {
            eprintln!(
                "[dd] workspace {:?}: drop the real nvidia-smi at {} (or {}) to run it against dd's NVML; \
                 the NVML shim is still injected so any NVML client sees the device.",
                ws.name,
                smi_a.display(),
                smi_g.display()
            );
            String::new()
        };
        gpu = gpu.with_cuda(dd_gpu::integration::CudaIntegration {
            name: cuda.name.clone(),
            compute_capability: cuda.compute_capability.clone(),
            vram_mb: cuda.vram_mb,
            nvml_so,
            nvidia_smi,
            // Merge our lib dir ahead of any user-set LD_LIBRARY_PATH (first match, as the original did).
            prior_ld_library_path: ws.env.iter().find(|(k, _)| k == "LD_LIBRARY_PATH").map(|(_, v)| v.clone()),
        });
    }
    if !gpu.is_inert() {
        // Ask the provider what it needs (composing its LD_LIBRARY_PATH against the current guest `env`),
        // apply the mounts + render-node generically, fold its env in, and re-apply the guest env once so
        // the added K=V lines go through the normal docker last-wins dedup — byte-identical to the old
        // inline gui+cuda blocks (same volumes, same env order, same DD_GPU_IOSURFACE flag).
        let req = gpu.device_request(&env);
        builder = builder.apply_device(&req);
        env.extend(req.env);
        builder = builder.guest_env(&env, true);
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

    // Record the container init's host pid so a separate `workspace checkpoint` can signal the live tree
    // (per-pane slot when given, else the shared slot).
    let _ = std::fs::write(&ckpt_pid_file, launched.pid.to_string());

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

/// Single-quote a path for safe inclusion in the `sh -c` script (wrap in `'…'`, escaping embedded `'`).
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
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
