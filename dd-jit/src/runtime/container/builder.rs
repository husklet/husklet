//! The fluent [`ContainerBuilder`] — every `docker run` knob as a chainable method, finalized by
//! [`ContainerBuilder::build`] into a [`Container`]. Applies the docker env/`--user` dialect via the
//! [`guest_env`]/[`resolve_user`] helpers.

use super::*;
use super::super::error::Error;
use dd_jit_darwin::{Guest, PortMap, SpawnConfig, Volume};

/// Fluent builder for a [`Container`]. All fields have sensible defaults (unlimited resources, no
/// ports, shared network, root user); set only what you need.
#[derive(Clone, Debug)]
pub struct ContainerBuilder {
    pub(super) cfg: SpawnConfig,
    pub(super) guest: Guest,
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

    /// Memory limit in bytes (cgroup `memory.max`). 0 = unlimited.
    pub fn memory_bytes(mut self, bytes: u64) -> Self {
        self.cfg.mem_max = bytes;
        self
    }

    /// The host working directory for resolving relative rootfs paths (empty = none).
    pub fn host_workdir(mut self, dir: impl Into<String>) -> Self {
        self.cfg.work_dir = dir.into();
        self
    }

    /// Replace the guest argv entirely (entrypoint + args) — used for launch wrappers.
    pub fn argv(mut self, argv: Vec<String>) -> Self {
        self.cfg.argv = argv;
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

    /// The guest's initial working directory (docker `-w`/`WorkingDir`).
    pub fn cwd(mut self, dir: impl Into<String>) -> Self {
        let dir = dir.into();
        if !dir.is_empty() {
            self.cfg.env.push(("DD_CWD".into(), dir));
        }
        self
    }

    /// Set the guest-visible environment from merged `K=V` lines (image env + `-e` overrides). Applies
    /// docker env semantics (last-wins dedup, default PATH, `TERM=xterm` under `tty`) via [`guest_env`]
    /// and forwards EXACTLY these to the guest — never the host/daemon environment.
    pub fn guest_env(mut self, env: &[String], tty: bool) -> Self {
        let genv = guest_env(env, tty);
        if !genv.is_empty() {
            self.cfg.env.push(("DD_GUEST_ENV".into(), genv.join("\n")));
        }
        self
    }

    /// Resolve a docker `--user U[:G]` spec against the image `rootfs` and surface the uid/gid to the
    /// guest (getuid/getgid/setuid). An unresolvable name is ignored (guest keeps its default identity).
    pub fn user_spec(mut self, rootfs: &str, spec: &str) -> Self {
        if !spec.is_empty() {
            if let Some((uid, gid)) = resolve_user(rootfs, spec) {
                self.cfg.uid = Some(uid);
                self.cfg.gid = Some(gid);
            }
        }
        self
    }

    /// Run the guest under the untrusted-guest sentry / OS sandbox (docker `--security-opt sandbox`).
    pub fn sandbox(mut self, on: bool) -> Self {
        if on {
            self.cfg.env.push(("DDJIT_UNTRUSTED".into(), "1".into()));
            self.cfg.env.push(("DDJIT_SANDBOX".into(), "1".into()));
        }
        self
    }

    /// `--network none`: refuse all non-loopback egress.
    pub fn net_isolate(mut self, on: bool) -> Self {
        if on {
            self.cfg.env.push(("DD_NET_ISOLATE".into(), "1".into()));
        }
        self
    }

    /// Join a user-defined network's virtual switch: the network id (switch key) and this container's IP,
    /// so in-subnet peers reach each other by container<->container TCP.
    pub fn bridge(mut self, netid: impl Into<String>, ip: impl Into<String>) -> Self {
        self.cfg.env.push(("DD_NETBR".into(), netid.into()));
        self.cfg.env.push(("DD_IP".into(), ip.into()));
        self
    }

    /// Hand the guest the shared external-writer generation file so daemon-side writes into the live fs
    /// (docker cp, /etc rewrites) invalidate the engine's path/metadata caches and become guest-visible.
    pub fn write_coherence_file(mut self, path: impl Into<String>) -> Self {
        self.cfg.env.push(("DD_FSGEN_FILE".into(), path.into()));
        self
    }

    /// Enable the persistent translated-code cache in `dir` (2nd+ run of an image skips translation).
    /// Self-invalidating (keyed by image hash + engine version) and graceful-miss safe.
    pub fn persistent_cache(mut self, dir: impl Into<String>) -> Self {
        self.cfg.env.push(("DDJIT_PCACHE".into(), "1".into()));
        self.cfg.env.push(("DDJIT_PCACHE_DIR".into(), dir.into()));
        self
    }

    /// Tell the engine NOT to start its own in-process host TCP forwarder — the caller owns a
    /// process-independent host forwarder for published ports (still passes the port map for getsockname).
    pub fn external_port_forwarder(mut self, on: bool) -> Self {
        if on {
            self.cfg.env.push(("DD_PUBLISH_DAEMON".into(), "1".into()));
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(c: &Container) -> Vec<(String, String)> {
        c.cfg.env.clone()
    }

    fn has(c: &Container, k: &str, v: &str) -> bool {
        c.cfg.env.iter().any(|(ek, ev)| ek == k && ev == v)
    }

    #[test]
    fn builder_dialect_matches_daemon_keys() {
        // Every promoted DD_*/DDJIT_* key encodes exactly as the daemon's spawn_cfg did.
        let c = Container::builder(Image::from_rootfs("/img"))
            .cmd(["/bin/true"])
            .cwd("/work")
            .guest_env(&["FOO=bar".to_string()], true)
            .sandbox(true)
            .net_isolate(true)
            .bridge("net123", "10.0.0.5")
            .write_coherence_file("/run/fsgen")
            .persistent_cache("/home/dd/pcache")
            .external_port_forwarder(true)
            .build()
            .unwrap();
        assert!(has(&c, "DD_CWD", "/work"));
        // guest_env injects the default PATH (FOO=bar set none) then TERM=xterm under the tty.
        assert!(has(&c, "DD_GUEST_ENV", &format!("FOO=bar\n{DEFAULT_GUEST_PATH}\nTERM=xterm")));
        assert!(has(&c, "DDJIT_UNTRUSTED", "1"));
        assert!(has(&c, "DDJIT_SANDBOX", "1"));
        assert!(has(&c, "DD_NET_ISOLATE", "1"));
        assert!(has(&c, "DD_NETBR", "net123"));
        assert!(has(&c, "DD_IP", "10.0.0.5"));
        assert!(has(&c, "DD_FSGEN_FILE", "/run/fsgen"));
        assert!(has(&c, "DDJIT_PCACHE", "1"));
        assert!(has(&c, "DDJIT_PCACHE_DIR", "/home/dd/pcache"));
        assert!(has(&c, "DD_PUBLISH_DAEMON", "1"));
    }

    #[test]
    fn builder_off_switches_emit_nothing() {
        // Disabled flags and empty cwd must NOT emit their keys (docker parity — absence is meaningful).
        let c = Container::builder(Image::from_rootfs("/img"))
            .cwd("")
            .sandbox(false)
            .net_isolate(false)
            .external_port_forwarder(false)
            .build()
            .unwrap();
        assert!(env_of(&c).is_empty());
    }

    #[test]
    fn user_spec_numeric_and_missing_rootfs() {
        // A numeric uid needs no rootfs and defaults gid to 0 (docker semantics).
        let c = Container::builder(Image::from_rootfs("/nonexistent"))
            .user_spec("/nonexistent", "1000")
            .build()
            .unwrap();
        assert_eq!((c.cfg.uid, c.cfg.gid), (Some(1000), Some(0)));
        // `uid:gid` both numeric, verbatim.
        let c2 = Container::builder(Image::from_rootfs("/x"))
            .user_spec("/x", "1000:2000")
            .build()
            .unwrap();
        assert_eq!((c2.cfg.uid, c2.cfg.gid), (Some(1000), Some(2000)));
        // An unresolvable NAME against a missing rootfs leaves the default identity.
        let c3 = Container::builder(Image::from_rootfs("/x"))
            .user_spec("/x", "postgres")
            .build()
            .unwrap();
        assert_eq!((c3.cfg.uid, c3.cfg.gid), (None, None));
    }
}
