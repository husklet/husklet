//! [`Container`] + [`ContainerBuilder`] — the typed, fluent container spec, plus the docker-parity
//! environment and `--user` resolution helpers ([`guest_env`], [`resolve_user`]) the builder applies.

use super::image::Image;
use dd_jit_darwin::{Guest, SpawnConfig};

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

    /// Map this container into the typed, env-free [`dd_jit_darwin::LaunchConfig`] the FFI spawn path
    /// consumes. The builder stores some knobs as `DD_*`/`DDJIT_*` env pairs (docker-parity encoding);
    /// this translates each known key into its typed wire field, so nothing crosses the FFI as loose
    /// environment. Pure engine *tuning* knobs (CRASHDBG/COLDPROF/DDJIT_NOPCACHE) are not part of the
    /// container contract and are intentionally dropped here — a follow-up can carry them if needed.
    pub(crate) fn launch_config(&self) -> dd_jit_darwin::LaunchConfig {
        let c = &self.cfg;
        let mut lc = dd_jit_darwin::LaunchConfig {
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
        // Translate the builder's DD_*/DDJIT_* env pairs into typed wire fields (the engine setenv's them
        // back internally from the wire, so the API carries zero environment).
        for (k, v) in &c.env {
            match k.as_str() {
                "DD_CWD" => lc.cwd = v.clone(),
                "DD_GUEST_ENV" => lc.guest_env = v.split('\n').map(str::to_string).collect(),
                "DDJIT_PCACHE_DIR" => lc.pcache_dir = v.clone(),
                "DDJIT_SANDBOX" | "DDJIT_UNTRUSTED" => lc.sandbox = true,
                "DD_NET_ISOLATE" => lc.net_isolate = true,
                "DD_NETBR" => lc.netbr = v.clone(),
                "DD_IP" => lc.ip = v.clone(),
                "DD_FSGEN_FILE" => lc.fsgen_file = v.clone(),
                "DD_PUBLISH_DAEMON" => lc.publish_daemon = true,
                "DD_GPU_IOSURFACE" => lc.gpu_iosurface = true,
                // "DDJIT_PCACHE" is a bare enable gate; the pcache dir's presence enables it engine-side.
                // CRASHDBG/COLDPROF/DDJIT_NOPCACHE are tuning knobs, not container config — dropped here.
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

    // ---- launch_config(): DD_*/DDJIT_* + typed SpawnConfig -> typed LaunchConfig mapping ----

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
            .write_coherence_file("/run/fsgen")
            .persistent_cache("/home/dd/pcache")
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

        // DD_*/DDJIT_* env-pair translation.
        assert_eq!(lc.cwd, "/work");
        // DD_GUEST_ENV is newline-joined at the builder and split back into a Vec here.
        assert_eq!(lc.guest_env, vec!["FOO=bar".to_string(), DEFAULT_GUEST_PATH.to_string(), "TERM=xterm".to_string()]);
        assert!(lc.sandbox); // DDJIT_SANDBOX / DDJIT_UNTRUSTED -> sandbox
        assert!(lc.net_isolate); // DD_NET_ISOLATE
        assert!(lc.publish_daemon); // DD_PUBLISH_DAEMON
        assert_eq!(lc.netbr, "net123"); // DD_NETBR
        assert_eq!(lc.ip, "10.0.0.5"); // DD_IP
        assert_eq!(lc.fsgen_file, "/run/fsgen"); // DD_FSGEN_FILE
        assert_eq!(lc.pcache_dir, "/home/dd/pcache"); // DDJIT_PCACHE_DIR
        assert!(lc.gpu_iosurface); // DD_GPU_IOSURFACE
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
        // env-backed fields: all default (empty / false) because no DD_*/DDJIT_* pairs were stored.
        assert_eq!(lc.cwd, "");
        assert!(lc.guest_env.is_empty());
        assert!(!lc.sandbox);
        assert!(!lc.net_isolate);
        assert!(!lc.publish_daemon);
        assert_eq!(lc.netbr, "");
        assert_eq!(lc.ip, "");
        assert_eq!(lc.fsgen_file, "");
        assert_eq!(lc.pcache_dir, "");
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
            .env("DDJIT_NOPCACHE", "1")
            .persistent_cache("/pc") // also emits the bare DDJIT_PCACHE gate, which is likewise dropped
            .build()
            .unwrap();
        // Precondition: those keys really are stored on the spawn config.
        assert!(has(&c, "CRASHDBG", "1"));
        assert!(has(&c, "COLDPROF", "1"));
        assert!(has(&c, "DDJIT_NOPCACHE", "1"));
        assert!(has(&c, "DDJIT_PCACHE", "1"));

        let lc = c.launch_config();
        // The only DDJIT_* key that maps is DDJIT_PCACHE_DIR; the bare gate + tuning keys leave no trace.
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
