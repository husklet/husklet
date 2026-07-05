//! [`Runtime`] — the host backend that runs containers, and the operator-level defaults (persistent
//! translated-code cache, default guest sandbox) it applies to every container. The synchronous
//! [`Runtime::run`] lives here; the async launch/supervision surface is in [`super::engine`].

use super::container::Container;
use super::error::Error;
use super::handle::RunHandle;
use dd_jit_darwin::{Guest, SpawnConfig};

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
        dd_jit_darwin::available(g)
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

    /// Run a container. Returns a handle to wait on / signal. Launches the linked engine directly —
    /// no `bash`, no separate `ddjit-*` binary is spawned by the caller.
    pub fn run(&self, c: &Container) -> Result<RunHandle, Error> {
        if !dd_jit_darwin::available(c.guest) {
            return Err(Error::NoBackend(c.guest));
        }
        let c = self.with_defaults(c);
        let (prog, args) = c.cfg.command(c.guest).ok_or(Error::NoBackend(c.guest))?;
        let child = std::process::Command::new(prog).args(args).spawn()?;
        Ok(RunHandle { child })
    }
}
