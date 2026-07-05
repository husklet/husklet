//! The ergonomic runtime API: `Runtime` (host backend), `Image`, `Container` + its builder, and a
//! `RunHandle` you can wait/signal on. It is a typed layer over the backend's `SpawnConfig` launch
//! contract; today it launches the engine via the backend's command (subprocess), which Phase 3
//! replaces in-place with a linked fork+FFI entry without changing this surface.

use dd_jit_darwin::{Guest, PortMap, SpawnConfig, Volume};
use std::fmt;

/// An error configuring or running a container.
#[derive(Debug)]
pub enum Error {
    /// No engine backend is available for the requested guest (the JIT binary was not built).
    NoBackend(Guest),
    /// The container spec is incomplete (e.g. no image).
    Invalid(&'static str),
    /// The underlying OS failed to launch the container.
    Io(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NoBackend(g) => write!(f, "no dd-jit backend available for {}", g.target()),
            Error::Invalid(m) => write!(f, "invalid container config: {m}"),
            Error::Io(e) => write!(f, "container launch failed: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// A container image: a rootfs (optionally an overlay of read-only lower layers) plus the guest
/// personality (OS + ISA) the engine runs it as.
#[derive(Clone, Debug)]
pub struct Image {
    rootfs: String,
    lowers: Vec<String>,
    guest: Guest,
}

impl Image {
    /// An image backed by a single rootfs directory. The guest personality defaults to the native
    /// Linux/aarch64 guest; use [`Image::guest`] to override (e.g. an x86-64 or macOS image).
    pub fn from_rootfs(rootfs: impl Into<String>) -> Self {
        Image { rootfs: rootfs.into(), lowers: Vec::new(), guest: Guest::default() }
    }

    /// An overlay image: a writable upper `rootfs` over read-only `lowers` (OCI image layers).
    pub fn overlay(rootfs: impl Into<String>, lowers: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Image {
            rootfs: rootfs.into(),
            lowers: lowers.into_iter().map(Into::into).collect(),
            guest: Guest::default(),
        }
    }

    /// Set the guest personality (OS + ISA) this image runs as.
    pub fn guest(mut self, g: Guest) -> Self {
        self.guest = g;
        self
    }

    /// The guest personality this image runs as.
    pub fn guest_of(&self) -> Guest {
        self.guest
    }
}

/// A fully-specified container ready to run. Build one with [`Container::builder`].
#[derive(Clone, Debug)]
pub struct Container {
    cfg: SpawnConfig,
    guest: Guest,
}

impl Container {
    /// Start building a container from an image.
    pub fn builder(image: Image) -> ContainerBuilder {
        let mut cfg = SpawnConfig::new(".", image.rootfs);
        cfg.lowers = image.lowers;
        ContainerBuilder { cfg, guest: image.guest }
    }
}

/// Fluent builder for a [`Container`]. All fields have sensible defaults (unlimited resources, no
/// ports, shared network, root user); set only what you need.
#[derive(Clone, Debug)]
pub struct ContainerBuilder {
    cfg: SpawnConfig,
    guest: Guest,
}

impl ContainerBuilder {
    /// The command to run (entrypoint + args), replacing the image default.
    pub fn cmd<S: Into<String>>(mut self, argv: impl IntoIterator<Item = S>) -> Self {
        self.cfg.argv = argv.into_iter().map(Into::into).collect();
        self
    }

    /// Add an environment variable.
    pub fn env(mut self, key: impl Into<String>, val: impl Into<String>) -> Self {
        self.cfg.env.push((key.into(), val.into()));
        self
    }

    /// Working directory inside the container.
    pub fn workdir(mut self, dir: impl Into<String>) -> Self {
        self.cfg.work_dir = dir.into();
        self
    }

    /// Run as this uid (defaults to root/0).
    pub fn user(mut self, uid: u32, gid: u32) -> Self {
        self.cfg.uid = Some(uid);
        self.cfg.gid = Some(gid);
        self
    }

    /// CPU limit (`--cpus`). 0 = unlimited.
    pub fn cpus(mut self, cpus: u32) -> Self {
        self.cfg.cpus = cpus;
        self
    }

    /// Memory limit in MiB. 0 = unlimited.
    pub fn memory_mb(mut self, mb: u64) -> Self {
        self.cfg.mem_max = mb.saturating_mul(1024 * 1024);
        self
    }

    /// Process (pids) limit. 0 = unlimited.
    pub fn pids(mut self, pids: u32) -> Self {
        self.cfg.pids_max = pids;
        self
    }

    /// Make the rootfs read-only (`--read-only`).
    pub fn read_only(mut self, ro: bool) -> Self {
        self.cfg.read_only = ro;
        self
    }

    /// Container hostname.
    pub fn hostname(mut self, name: impl Into<String>) -> Self {
        self.cfg.hostname = Some(name.into());
        self
    }

    /// Publish a container port on a host port (`-p host:container`).
    pub fn publish(mut self, host: u16, container: u16) -> Self {
        self.cfg.publish.push(PortMap { host, container });
        self
    }

    /// Bind-mount a host path into the container.
    pub fn bind(mut self, host: impl Into<String>, container: impl Into<String>, read_only: bool) -> Self {
        self.cfg.volumes.push(Volume { container: container.into(), host: host.into(), ro: read_only });
        self
    }

    /// Set a resource ulimit (name, soft, hard).
    pub fn ulimit(mut self, name: impl Into<String>, soft: u64, hard: u64) -> Self {
        self.cfg.ulimits.push((name.into(), soft, hard));
        self
    }

    /// Run in a private loopback network namespace (isolated networking).
    pub fn private_network(mut self, netns_id: impl Into<String>) -> Self {
        self.cfg.netns = Some(netns_id.into());
        self
    }

    /// Finalize the container spec.
    pub fn build(self) -> Result<Container, Error> {
        if self.cfg.rootfs.is_empty() {
            return Err(Error::Invalid("image rootfs is empty"));
        }
        Ok(Container { cfg: self.cfg, guest: self.guest })
    }
}

/// The runtime — the host backend that runs containers. Construct with [`Runtime::new`].
pub struct Runtime {
    _private: (),
}

impl Runtime {
    /// Create a runtime bound to this host's backend (`dd-jit-darwin` on macOS).
    pub fn new() -> Result<Self, Error> {
        Ok(Runtime { _private: () })
    }

    /// Whether this runtime can run the given guest personality (its engine is built).
    pub fn supports(&self, g: Guest) -> bool {
        dd_jit_darwin::available(g)
    }

    /// Run a container. Returns a handle to wait on / signal. Launches the linked engine directly —
    /// no `bash`, no separate `ddjit-*` binary is spawned by the caller.
    pub fn run(&self, c: &Container) -> Result<RunHandle, Error> {
        if !dd_jit_darwin::available(c.guest) {
            return Err(Error::NoBackend(c.guest));
        }
        let (prog, args) = c.cfg.command(c.guest).ok_or(Error::NoBackend(c.guest))?;
        let child = std::process::Command::new(prog).args(args).spawn()?;
        Ok(RunHandle { child })
    }
}

/// A running container. Wait on it or send it a signal.
pub struct RunHandle {
    child: std::process::Child,
}

impl RunHandle {
    /// Block until the container exits.
    pub fn wait(&mut self) -> Result<ExitStatus, Error> {
        let st = self.child.wait()?;
        Ok(ExitStatus { code: st.code().unwrap_or(-1) })
    }

    /// The container's host process id.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Signal the container (e.g. `libc::SIGTERM`).
    pub fn signal(&self, sig: i32) -> Result<(), Error> {
        // Safety: kill(2) on our own child's pid; a stale pid just returns ESRCH.
        let r = unsafe { libc_kill(self.child.id() as i32, sig) };
        if r != 0 {
            return Err(Error::Io(std::io::Error::last_os_error()));
        }
        Ok(())
    }
}

extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

/// The exit status of a finished container.
#[derive(Clone, Copy, Debug)]
pub struct ExitStatus {
    code: i32,
}

impl ExitStatus {
    /// The process exit code (-1 if terminated by signal / unavailable).
    pub fn code(&self) -> i32 {
        self.code
    }

    /// Whether the container exited successfully (code 0).
    pub fn success(&self) -> bool {
        self.code == 0
    }
}
