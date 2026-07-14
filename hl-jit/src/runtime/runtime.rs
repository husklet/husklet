//! [`Runtime`] — the host backend that runs containers, and the operator-level defaults (persistent
//! translated-code cache, default guest sandbox) it applies to every container. The synchronous
//! [`Runtime::run`] lives here; the async launch/supervision surface is in [`super::engine`].

use super::container::Container;
use super::error::Error;
use super::handle::RunHandle;
use hl_jit_darwin::{Guest, SpawnConfig};

/// The runtime — the host backend that runs containers. Construct with [`Runtime::new`].
///
/// The runtime owns the host/operator-level defaults that apply to EVERY container it launches, so a
/// container manager (e.g. `dd-daemon`) never handles them itself: the persistent translated-code cache
/// (on unless `DD_PCACHE=0`) and the default guest sandbox (`DD_SANDBOX=1`). Set the cache location with
/// [`Runtime::cache_dir`].
pub struct Runtime {
    /// Enable the persistent translated-code cache by default (operator env `DD_PCACHE`, on unless "0").
    pcache: bool,
    /// Where the persistent cache lives (created on demand). `None` ⇒ the cache stays off even if `pcache`.
    pcache_dir: Option<String>,
    /// Run every guest under the sandbox by default (operator env `DD_SANDBOX == "1"`).
    sandbox_default: bool,
}

impl Runtime {
    /// Create a runtime bound to this host's backend (`dd-jit-darwin` on macOS), reading the operator
    /// env defaults (`DD_PCACHE`, `DD_SANDBOX`, `DDJIT_PCACHE_DIR`).
    pub fn new() -> Result<Self, Error> {
        Ok(Runtime {
            pcache: std::env::var("DD_PCACHE").as_deref() != Ok("0"),
            pcache_dir: std::env::var("DDJIT_PCACHE_DIR").ok(),
            sandbox_default: std::env::var("DD_SANDBOX").as_deref() == Ok("1"),
        })
    }

    /// Set the directory backing the persistent translated-code cache (the host storage location). The
    /// cache is only enabled when both this is set and `DD_PCACHE` is not "0".
    pub fn cache_dir(mut self, dir: impl Into<String>) -> Self {
        self.pcache_dir = Some(dir.into());
        self
    }

    /// Whether this runtime can run the given guest personality (its engine is built).
    pub fn supports(&self, g: Guest) -> bool {
        hl_jit_darwin::available(g)
    }

    /// Apply the operator defaults (persistent cache, default sandbox) to a container, unless it already
    /// sets them. Returns the effective container to launch.
    pub(crate) fn with_defaults(&self, c: &Container) -> Container {
        let mut c = c.clone();
        let has = |cfg: &SpawnConfig, k: &str| cfg.env.iter().any(|(ek, _)| ek == k);
        if self.pcache && !has(&c.cfg, "DDJIT_PCACHE") {
            if let Some(dir) = &self.pcache_dir {
                let _ = std::fs::create_dir_all(dir);
                c.cfg.env.push(("DDJIT_PCACHE".into(), "1".into()));
                c.cfg.env.push(("DDJIT_PCACHE_DIR".into(), dir.clone()));
            }
        }
        if self.sandbox_default && !has(&c.cfg, "DDJIT_SANDBOX") {
            c.cfg.env.push(("DDJIT_UNTRUSTED".into(), "1".into()));
            c.cfg.env.push(("DDJIT_SANDBOX".into(), "1".into()));
        }
        c
    }

    /// Checkpoint a RUNNING container's whole process tree (all shells, background jobs, and their children)
    /// to `dir`, freeing its memory, and wait for the freeze to complete. `pid` is the container init the
    /// launch returned; the container must have been launched with checkpointing armed for the SAME `dir`
    /// (the launcher exports `DDJIT_CHECKPOINT_DIR`). This sends the engine's checkpoint control signal
    /// (SIGUSR1) — the init coordinates a tree-wide freeze at the next safe guest-block boundary, each
    /// process snapshots its RAM+CPU+fds to `dir/proc.<gpid>/`, then the `dir/MANIFEST` is published and
    /// every process exits. Resume later with a launch that sets `DDJIT_RESTORE_DIR` to the same `dir`.
    pub fn checkpoint(&self, pid: u32, dir: &str, timeout: std::time::Duration) -> Result<(), Error> {
        use std::time::Instant;
        // Prepare a fresh, empty checkpoint dir BEFORE advancing the trigger: every engine process sees the
        // shared generation independently and drops its proc.<gpid> here, so the dir must already exist and be
        // clear of the previous checkpoint. The MANIFEST (written last by the init) marks completion.
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).map_err(Error::Io)?;
        let manifest = std::path::Path::new(dir).join("MANIFEST");
                                                 // Advance the shared trigger generation the engine polls at its safepoint (a MAP_SHARED u32 at
                                                 // "<dir>.trigger"), then kick the init with the engine's guest-proof THREAD_INT_SIG (SIGINFO) so it
                                                 // reaches a safepoint promptly. A signal alone is unusable as the trigger — a guest's rt_sigaction
                                                 // silently remaps every guest-reachable host signal — so intent travels through shared memory.
        // macOS SIGINFO (BSD signal 29) — the engine's guest-clobber-proof kick signal. Hardcoded (not
        // `libc::SIGINFO`) so this crate also COMPILES on Linux hosts, where the constant is absent; the
        // checkpoint path only ever runs against the macOS engine.
        const KICK: libc::c_int = 29;
        bump_trigger(dir).map_err(Error::Io)?;
        if unsafe { libc::kill(pid as libc::pid_t, KICK) } != 0 {
            return Err(Error::Io(std::io::Error::last_os_error()));
        }
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if manifest.exists() {
                // A complete, restorable checkpoint (MANIFEST is written last). Unlink the trigger + pid so
                // the slot dir is self-contained and a later restore/launch never inherits a stale generation
                // or pid; the engine re-creates a fresh trigger (generation 0) when it re-arms, and the
                // launcher rewrites the pid. The init writes the MANIFEST just BEFORE it _exit()s, so wait
                // for its pid to actually die first — while it still holds the trigger MAP_SHARED-mapped the
                // unlink races the dying engine and the trigger survives with its bumped generation.
                cleanup_trigger_pid(dir, pid);
                return Ok(());
            }
            // Re-kick: the init may not have reached a safepoint on the first signal (a long blocking wait).
            let _ = unsafe { libc::kill(pid as libc::pid_t, KICK) };
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("checkpoint of pid {pid} did not complete within the timeout"),
        )))
    }

    /// Run a container. Returns a handle to wait on / signal. Forks the linked engine directly via the
    /// typed FFI (`hl_spawn`) — no `bash`, no separate `ddjit-*` binary, no `DD_*` environment. The
    /// child inherits the host stdio.
    pub fn run(&self, c: &Container) -> Result<RunHandle, Error> {
        if !hl_jit_darwin::available(c.guest) {
            return Err(Error::NoBackend(c.guest));
        }
        let c = self.with_defaults(c);
        let lc = c.launch_config();
        let pid = hl_jit_darwin::spawn(c.guest(), &lc).map_err(Error::Io)?;
        Ok(RunHandle { pid })
    }
}

/// Remove a slot's control-channel leftovers (`<dir>.trigger` + `<dir>.pid`) once no engine maps them.
/// Called after a checkpoint completes so a slot never accumulates stale generation files across sessions;
/// both are re-created cleanly on the next launch/restore. `init_pid` is the container init: it writes the
/// MANIFEST immediately before `_exit`, so we briefly wait for it to actually die (it still holds the
/// trigger MAP_SHARED-mapped until then) before unlinking, otherwise the trigger survives with its bumped
/// generation. Bounded so a stuck pid never stalls close; the fresh-launch path wipes any residue anyway.
fn cleanup_trigger_pid(dir: &str, init_pid: u32) {
    for _ in 0..100 {
        if unsafe { libc::kill(init_pid as libc::pid_t, 0) } != 0 {
            break; // ESRCH: the init is gone, nothing maps the trigger any more
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let _ = std::fs::remove_file(format!("{dir}.trigger"));
    let _ = std::fs::remove_file(format!("{dir}.pid"));
}

/// Advance the checkpoint trigger generation at `<dir>.trigger` — a 4-byte MAP_SHARED counter the engine
/// polls at its safepoint. Writing through a MAP_SHARED mapping (not a plain `write`) guarantees the running
/// engine sees the new value on the same physical page. Created (zero-initialised) on first use.
fn bump_trigger(dir: &str) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let path = format!("{dir}.trigger");
    if let Some(p) = std::path::Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let f = std::fs::OpenOptions::new().read(true).write(true).create(true).open(&path)?;
    f.set_len(4)?;
    let m = unsafe {
        libc::mmap(std::ptr::null_mut(), 4, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, f.as_raw_fd(), 0)
    };
    if m == libc::MAP_FAILED {
        return Err(std::io::Error::last_os_error());
    }
    unsafe {
        let cell = m as *mut u32;
        std::ptr::write_volatile(cell, std::ptr::read_volatile(cell).wrapping_add(1));
        libc::msync(m, 4, libc::MS_SYNC);
        libc::munmap(m, 4);
    }
    Ok(())
}
