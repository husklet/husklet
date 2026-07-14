//! [`Container`] + [`ContainerBuilder`] — the typed, fluent container spec, plus the docker-parity
//! environment and `--user` resolution helpers ([`guest_env`], [`resolve_user`]) the builder applies.

use super::image::Image;
use hl_jit_darwin::{Guest, SpawnConfig};

mod builder;
mod env;
mod user;

pub use builder::ContainerBuilder;
pub use env::{guest_env, DEFAULT_GUEST_PATH};
pub use user::resolve_user;

/// A fully-specified container ready to run. Build one with [`Container::builder`].
#[derive(Clone, Debug)]
pub struct Container {
    pub(crate) cfg: SpawnConfig,
    pub(crate) guest: Guest,
}

impl Container {
    /// Start building a container from an image.
    pub fn builder(image: Image) -> ContainerBuilder {
        // work_dir is the HOST cwd for resolving relative rootfs paths; empty = no `cd` (the common case,
        // callers pass absolute rootfs paths). Set it explicitly with [`ContainerBuilder::host_workdir`].
        let mut cfg = SpawnConfig::new("", image.rootfs);
        cfg.lowers = image.lowers;
        ContainerBuilder { cfg, guest: image.guest }
    }

    /// The guest personality this container runs as.
    pub fn guest(&self) -> Guest {
        self.guest
    }

    /// Map this container into the typed, env-free [`hl_jit_darwin::LaunchConfig`] the FFI spawn path
    /// consumes. The builder stores some knobs as `HL_*`/`HL_JIT_*` env pairs (docker-parity encoding);
    /// this translates each known key into its typed wire field, so nothing crosses the FFI as loose
    /// environment. Pure engine *tuning* knobs (CRASHDBG/COLDPROF/HL_JIT_NOPCACHE) are not part of the
    /// container contract and are intentionally dropped here — a follow-up can carry them if needed.
    pub(crate) fn launch_config(&self) -> hl_jit_darwin::LaunchConfig {
        let c = &self.cfg;
        let mut lc = hl_jit_darwin::LaunchConfig {
            rootfs: c.rootfs.clone(),
            lowers: c.lowers.clone(),
            hostname: c.hostname.clone().unwrap_or_default(),
            mem_max: c.mem_max,
            pids_max: c.pids_max,
            cpus: c.cpus,
            rootfs_ro: c.read_only,
            uid: c.uid,
            gid: c.gid,
            netns: c.netns.clone().unwrap_or_default(),
            publish: c.publish.iter().map(|p| (p.host, p.container)).collect(),
            volumes: c.volumes.iter().map(|v| (v.container.clone(), v.host.clone(), v.ro)).collect(),
            ulimits: c.ulimits.clone(),
            argv: c.argv.clone(),
            ..Default::default()
        };
        // Translate the builder's HL_*/HL_JIT_* env pairs into typed wire fields (the engine setenv's them
        // back internally from the wire, so the API carries zero environment).
        for (k, v) in &c.env {
            match k.as_str() {
                "HL_CWD" => lc.cwd = v.clone(),
                "HL_GUEST_ENV" => lc.guest_env = v.split('\n').map(str::to_string).collect(),
                "HL_JIT_PCACHE_DIR" => lc.pcache_dir = v.clone(),
                "HL_JIT_SANDBOX" | "HL_JIT_UNTRUSTED" => lc.sandbox = true,
                "HL_NET_ISOLATE" => lc.net_isolate = true,
                "HL_NETBR" => lc.netbr = v.clone(),
                "HL_IP" => lc.ip = v.clone(),
                "HL_FSGEN_FILE" => lc.fsgen_file = v.clone(),
                // Per-workspace VPN egress (docs/VPN.md): the builder's egress_socks() records the SOCKS5
                // endpoint here; carry it into the typed launch so the engine's egress redirect actually
                // arms. It was silently dropped, so a workspace configured with VPN egress ran DIRECT.
                "HL_EGRESS_SOCKS" => lc.egress_socks = v.clone(),
                "HL_PUBLISH_DAEMON" => lc.publish_daemon = true,
                "HL_GPU_IOSURFACE" => lc.gpu_iosurface = true,
                // "HL_JIT_PCACHE" is a bare enable gate; the pcache dir's presence enables it engine-side.
                // HL_JIT_NOPCACHE is a per-container pcache kill switch — carried through so an operator can opt
                // one container out even under global pcache defaults. CRASHDBG/COLDPROF stay tuning-only.
                "HL_JIT_NOPCACHE" => lc.nopcache = true,
                _ => {}
            }
        }
        lc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has(c: &Container, k: &str, v: &str) -> bool {
        c.cfg.env.iter().any(|(ek, ev)| ek == k && ev == v)
    }

    // ---- launch_config(): HL_*/HL_JIT_* + typed SpawnConfig -> typed LaunchConfig mapping ----

    #[test]
    fn launch_config_maps_all_fields() {
        // A container with every knob set must surface each into its typed LaunchConfig field.
        let c = Container::builder(Image::from_rootfs("/img").guest(Guest::LinuxAarch64))
            .cmd(["/bin/sh", "-c", "echo hi"])
            .cwd("/work")
            .guest_env(&["FOO=bar".to_string()], true)
            .sandbox(true)
            .net_isolate(true)
            .bridge("net123", "10.0.0.5")
            .egress_socks("127.30.0.1:1080")
            .write_coherence_file("/run/fsgen")
            .persistent_cache("/home/hl/pcache")
            .external_port_forwarder(true)
            .render_node(true)
            .hostname("host1")
            .memory_bytes(4096)
            .pids(200)
            .cpus(4)
            .read_only(true)
            .user(1000, 2000)
            .publish(8080, 80)
            .bind("/hostpath", "/ctr/mnt", true)
            .ulimit("nofile", 1024, 2048)
            .private_network("ns-abc")
            .build()
            .unwrap();
        let lc = c.launch_config();

        // Typed passthrough from SpawnConfig.
        assert_eq!(lc.rootfs, "/img");
        assert!(lc.lowers.is_empty());
        assert_eq!(lc.hostname, "host1");
        assert_eq!(lc.mem_max, 4096);
        assert_eq!(lc.pids_max, 200);
        assert_eq!(lc.cpus, 4);
        assert!(lc.rootfs_ro);
        assert_eq!(lc.uid, Some(1000));
        assert_eq!(lc.gid, Some(2000));
        assert_eq!(lc.netns, "ns-abc");
        assert_eq!(lc.publish, vec![(8080u16, 80u16)]);
        assert_eq!(lc.volumes, vec![("/ctr/mnt".to_string(), "/hostpath".to_string(), true)]);
        assert_eq!(lc.ulimits, vec![("nofile".to_string(), 1024u64, 2048u64)]);
        assert_eq!(lc.argv, vec!["/bin/sh", "-c", "echo hi"]);

        // HL_*/HL_JIT_* env-pair translation.
        assert_eq!(lc.cwd, "/work");
        // HL_GUEST_ENV is newline-joined at the builder and split back into a Vec here.
        assert_eq!(lc.guest_env, vec!["FOO=bar".to_string(), DEFAULT_GUEST_PATH.to_string(), "TERM=xterm".to_string()]);
        assert!(lc.sandbox); // HL_JIT_SANDBOX / HL_JIT_UNTRUSTED -> sandbox
        assert!(lc.net_isolate); // HL_NET_ISOLATE
        assert!(lc.publish_daemon); // HL_PUBLISH_DAEMON
        assert_eq!(lc.netbr, "net123"); // HL_NETBR
        assert_eq!(lc.ip, "10.0.0.5"); // HL_IP
        assert_eq!(lc.fsgen_file, "/run/fsgen"); // HL_FSGEN_FILE
        assert_eq!(lc.egress_socks, "127.30.0.1:1080"); // HL_EGRESS_SOCKS
        assert_eq!(lc.pcache_dir, "/home/hl/pcache"); // HL_JIT_PCACHE_DIR
        assert!(lc.gpu_iosurface); // HL_GPU_IOSURFACE
    }

    #[test]
    fn launch_config_carries_per_container_nopcache() {
        // A per-container HL_JIT_NOPCACHE (pcache kill switch) must reach the typed LaunchConfig, so an
        // operator can opt ONE container out even under global pcache defaults. It was silently dropped.
        let c = Container::builder(Image::from_rootfs("/img").guest(Guest::LinuxAarch64))
            .cmd(["/bin/true"])
            .env("HL_JIT_NOPCACHE", "1")
            .build()
            .unwrap();
        assert!(c.launch_config().nopcache, "per-container HL_JIT_NOPCACHE must carry through typed launch");

        // A container that did NOT set it keeps pcache following the global gate.
        let d = Container::builder(Image::from_rootfs("/img").guest(Guest::LinuxAarch64))
            .cmd(["/bin/true"])
            .build()
            .unwrap();
        assert!(!d.launch_config().nopcache);
    }

    #[test]
    fn launch_config_overlay_lowers_copy_across() {
        // Overlay lowers are a typed passthrough (highest-priority first).
        let c = Container::builder(Image::overlay("/upper", ["/lo0", "/lo1"]).guest(Guest::LinuxAarch64))
            .cmd(["/bin/true"])
            .build()
            .unwrap();
        let lc = c.launch_config();
        assert_eq!(lc.rootfs, "/upper");
        assert_eq!(lc.lowers, vec!["/lo0".to_string(), "/lo1".to_string()]);
    }

    // "Dockerfile WORKDIR Is Ignored for RUN" (P1): `.host_workdir` feeds HOST-side ADD/COPY path
    // resolution only — it must NOT populate the guest cwd. Setting the guest cwd (so `WORKDIR /app;
    // RUN pwd` prints `/app`) requires `.workdir`/`.cwd`. The build RUN step used to call only
    // `.host_workdir`, so RUN executed from `/`. This locks the distinction the fix relies on.
    #[test]
    fn host_workdir_is_not_guest_cwd_but_workdir_is() {
        let host_only = Container::builder(Image::from_rootfs("/img").guest(Guest::LinuxAarch64))
            .cmd(["/bin/true"])
            .host_workdir("/app")
            .build()
            .unwrap();
        assert_eq!(host_only.launch_config().cwd, "", "host_workdir must NOT set the guest cwd");

        let with_wd = Container::builder(Image::from_rootfs("/img").guest(Guest::LinuxAarch64))
            .cmd(["/bin/true"])
            .host_workdir("/app")
            .workdir("/app")
            .build()
            .unwrap();
        assert_eq!(with_wd.launch_config().cwd, "/app", "workdir sets the guest cwd");
    }

    #[test]
    fn launch_config_defaults_surface_nothing() {
        // A minimal runnable container: no env pairs, so every optional/env-backed field stays at its default.
        let c = Container::builder(Image::from_rootfs("/img").guest(Guest::LinuxAarch64))
            .cmd(["/bin/true"])
            .build()
            .unwrap();
        assert!(c.cfg.env.is_empty()); // precondition: nothing to translate
        let lc = c.launch_config();
        assert_eq!(lc.rootfs, "/img");
        assert!(lc.lowers.is_empty());
        assert_eq!(lc.hostname, "");
        assert_eq!(lc.mem_max, 0);
        assert_eq!(lc.pids_max, 0);
        assert_eq!(lc.cpus, 0);
        assert!(!lc.rootfs_ro);
        assert_eq!(lc.uid, None);
        assert_eq!(lc.gid, None);
        assert_eq!(lc.netns, "");
        assert!(lc.publish.is_empty());
        assert!(lc.volumes.is_empty());
        assert!(lc.ulimits.is_empty());
        assert_eq!(lc.argv, vec!["/bin/true"]);
        // env-backed fields: all default (empty / false) because no HL_*/HL_JIT_* pairs were stored.
        assert_eq!(lc.cwd, "");
        assert!(lc.guest_env.is_empty());
        assert!(!lc.sandbox);
        assert!(!lc.net_isolate);
        assert!(!lc.publish_daemon);
        assert_eq!(lc.netbr, "");
        assert_eq!(lc.ip, "");
        assert_eq!(lc.fsgen_file, "");
        assert_eq!(lc.egress_socks, "");
        assert_eq!(lc.pcache_dir, "");
    }

    // "Workspace VPN Egress Is Dropped" (P1): the builder's egress_socks() records HL_EGRESS_SOCKS, but
    // launch_config() used to drop it, so a workspace configured with VPN/SOCKS egress launched the engine
    // with NO egress redirect — the guest's external TCP went DIRECT, defeating the VPN. This locks the
    // key through to the typed LaunchConfig (the engine setenv's HL_EGRESS_SOCKS back from the wire and
    // netns.c's egress_socks() arms the redirect). An empty/absent VPN carries nothing (direct, unchanged).
    #[test]
    fn launch_config_carries_egress_socks() {
        let c = Container::builder(Image::from_rootfs("/img").guest(Guest::LinuxAarch64))
            .cmd(["/bin/true"])
            .egress_socks("10.8.0.1:1080")
            .build()
            .unwrap();
        // Precondition: the builder really recorded the SOCKS endpoint env pair.
        assert!(has(&c, "HL_EGRESS_SOCKS", "10.8.0.1:1080"));
        // The fix: it now reaches the typed launch config instead of being silently dropped.
        assert_eq!(
            c.launch_config().egress_socks,
            "10.8.0.1:1080",
            "HL_EGRESS_SOCKS must carry through to the typed LaunchConfig so the VPN redirect actually arms"
        );

        // No VPN configured: egress stays empty (direct host egress, byte-for-byte unchanged).
        let d = Container::builder(Image::from_rootfs("/img").guest(Guest::LinuxAarch64))
            .cmd(["/bin/true"])
            .build()
            .unwrap();
        assert_eq!(d.launch_config().egress_socks, "");
    }

    #[test]
    fn launch_config_guest_env_only_injected_path_when_empty() {
        // guest_env(&[], false) yields just the default PATH; it round-trips as a one-element Vec.
        let c = Container::builder(Image::from_rootfs("/img").guest(Guest::LinuxAarch64))
            .cmd(["/bin/true"])
            .guest_env(&[], false)
            .build()
            .unwrap();
        let lc = c.launch_config();
        assert_eq!(lc.guest_env, vec![DEFAULT_GUEST_PATH.to_string()]);
    }

    #[test]
    fn launch_config_drops_tuning_keys() {
        // Pure engine tuning knobs are NOT part of the container contract: they must be dropped, never
        // surfaced into any typed LaunchConfig field. (This is why run_step still needs .guest_env.)
        let c = Container::builder(Image::from_rootfs("/img").guest(Guest::LinuxAarch64))
            .cmd(["/bin/true"])
            .env("CRASHDBG", "1")
            .env("COLDPROF", "1")
            .env("HL_JIT_NOPCACHE", "1")
            .persistent_cache("/pc") // also emits the bare HL_JIT_PCACHE gate, which is likewise dropped
            .build()
            .unwrap();
        // Precondition: those keys really are stored on the spawn config.
        assert!(has(&c, "CRASHDBG", "1"));
        assert!(has(&c, "COLDPROF", "1"));
        assert!(has(&c, "HL_JIT_NOPCACHE", "1"));
        assert!(has(&c, "HL_JIT_PCACHE", "1"));

        let lc = c.launch_config();
        // The only HL_JIT_* key that maps is HL_JIT_PCACHE_DIR; the bare gate + tuning keys leave no trace.
        assert_eq!(lc.pcache_dir, "/pc");
        assert!(!lc.sandbox);
        assert!(!lc.net_isolate);
        assert!(!lc.publish_daemon);
        assert_eq!(lc.cwd, "");
        assert!(lc.guest_env.is_empty());
        assert_eq!(lc.netbr, "");
        assert_eq!(lc.ip, "");
        assert_eq!(lc.fsgen_file, "");
    }
}
