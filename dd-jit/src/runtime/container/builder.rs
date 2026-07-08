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

    /// The guest's initial working directory (docker `-w`/`WorkingDir`) — an alias for [`Self::cwd`].
    /// (For the HOST cwd used to resolve relative rootfs paths, use [`Self::host_workdir`] instead; this
    /// used to set that by mistake, contradicting its name.)
    pub fn workdir(self, dir: impl Into<String>) -> Self {
        self.cwd(dir)
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
    /// docker env semantics (last-wins dedup, default PATH, `TERM=xterm` under `tty`) via `guest_env`
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

    /// Per-workspace VPN egress: funnel the guest's genuine external TCP connects through the SOCKS5 proxy
    /// at `addr` (`host:port`) instead of a direct host connect (see `docs/VPN.md`). Sets `DD_EGRESS_SOCKS`,
    /// the engine's egress-redirect switch; an empty `addr` emits nothing (direct egress, unchanged).
    pub fn egress_socks(mut self, addr: impl Into<String>) -> Self {
        let addr = addr.into();
        if !addr.is_empty() {
            self.cfg.env.push(("DD_EGRESS_SOCKS".into(), addr));
        }
        self
    }

    /// Request the engine synthesize a host-backed **synthetic device / render node** (the accelerated
    /// "render node" rung): it materializes `/dev/dri/renderD128` and services the host-backed alloc ioctl.
    /// Runtime-neutral — the runtime does not know which backend wants it; a [`DeviceProvider`] sets it via
    /// [`DeviceRequest::render_node`]. Carried through the typed launch config (the engine reads the wire's
    /// device-node flag), NOT the ambient host env — the FFI spawn does not forward the launcher's
    /// environment, so a `set_var` would never reach the engine. Off (default) leaves the whole path inert:
    /// existing workloads are byte-for-byte unchanged.
    ///
    /// [`DeviceProvider`]: crate::DeviceProvider
    /// [`DeviceRequest::render_node`]: crate::DeviceRequest::render_node
    pub fn render_node(mut self, on: bool) -> Self {
        if on {
            self.cfg.env.push(("DD_GPU_IOSURFACE".into(), "1".into()));
        }
        self
    }

    /// Apply a runtime-neutral [`DeviceRequest`] produced by a [`DeviceProvider`]: bind each of its mounts
    /// and, if asked, request the synthetic device node. The request's extra guest env
    /// ([`DeviceRequest::env`]) is deliberately NOT folded here — the caller merges it into its guest-env
    /// vector and re-applies [`Self::guest_env`], so it participates in the normal docker env dedup. The
    /// runtime thus stays device-agnostic: it never learns what the mounts/env/node actually *mean*.
    ///
    /// [`DeviceProvider`]: crate::DeviceProvider
    /// [`DeviceRequest`]: crate::DeviceRequest
    /// [`DeviceRequest::env`]: crate::DeviceRequest::env
    pub fn apply_device(mut self, req: &crate::DeviceRequest) -> Self {
        for m in &req.mounts {
            self = self.bind(m.host.clone(), m.container.clone(), m.read_only);
        }
        if req.render_node {
            self = self.render_node(true);
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
            .egress_socks("127.30.0.1:1080")
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
        assert!(has(&c, "DD_EGRESS_SOCKS", "127.30.0.1:1080"));
        assert!(has(&c, "DD_FSGEN_FILE", "/run/fsgen"));
        assert!(has(&c, "DDJIT_PCACHE", "1"));
        assert!(has(&c, "DDJIT_PCACHE_DIR", "/home/dd/pcache"));
        assert!(has(&c, "DD_PUBLISH_DAEMON", "1"));
    }

    #[test]
    fn apply_device_binds_mounts_and_render_node() {
        // A runtime-neutral DeviceRequest binds its mounts (in order) and, when render_node is set, arms
        // the same DD_GPU_IOSURFACE flag as .render_node(true) — with NO device-specific vocabulary.
        use crate::{DeviceMount, DeviceRequest};
        let req = DeviceRequest {
            mounts: vec![
                DeviceMount::rw("/host/sock", "/run/user/0/wayland-0"),
                DeviceMount::ro("/host/lib.so", "/usr/lib/x/lib.so"),
            ],
            env: vec!["IGNORED_BY_APPLY_DEVICE=1".to_string()], // env is folded by the caller, not here
            render_node: true,
        };
        let c = Container::builder(Image::from_rootfs("/img"))
            .apply_device(&req)
            .build()
            .unwrap();
        // Mounts land as volumes, order + ro flag preserved.
        let vols: Vec<(String, String, bool)> =
            c.cfg.volumes.iter().map(|v| (v.container.clone(), v.host.clone(), v.ro)).collect();
        assert_eq!(
            vols,
            vec![
                ("/run/user/0/wayland-0".to_string(), "/host/sock".to_string(), false),
                ("/usr/lib/x/lib.so".to_string(), "/host/lib.so".to_string(), true),
            ]
        );
        // render_node → the DD_GPU_IOSURFACE transport flag (unchanged wire).
        assert!(has(&c, "DD_GPU_IOSURFACE", "1"));
        // apply_device does NOT touch the guest env — that is the caller's job via guest_env().
        assert!(!c.cfg.env.iter().any(|(k, _)| k == "DD_GUEST_ENV"));
        assert!(!c.cfg.env.iter().any(|(k, _)| k == "IGNORED_BY_APPLY_DEVICE"));
    }

    #[test]
    fn apply_device_empty_request_is_inert() {
        // A default (empty) request binds nothing and arms no flag — byte-for-byte the no-device path.
        use crate::DeviceRequest;
        let c = Container::builder(Image::from_rootfs("/img"))
            .apply_device(&DeviceRequest::default())
            .build()
            .unwrap();
        assert!(c.cfg.volumes.is_empty());
        assert!(env_of(&c).is_empty());
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
