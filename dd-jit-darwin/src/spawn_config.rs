use crate::{Guest, PortMap, Volume};

/// Everything needed to launch one container in the JIT. Mirrors the JIT's flag/env contract:
/// `--rootfs/--lower/--hostname/--mem-max/--pids-max/--uid/--gid/--publish` + `DDVOL`/`DD_NETNS` env.
#[derive(Clone, Debug, Default)]
pub struct SpawnConfig {
    /// Working directory on the host (where relative image/rootfs paths resolve).
    pub work_dir: String,
    /// The writable rootfs (the overlay UPPER, or a plain single rootfs).
    pub rootfs: String,
    /// Read-only overlay lower layers, highest-priority first (the OCI image layers).
    pub lowers: Vec<String>,
    /// Bind-mounted volumes.
    pub volumes: Vec<Volume>,
    /// Published ports (`-p`).
    pub publish: Vec<PortMap>,
    /// Private-loopback network namespace id (isolates 127.0.0.0/8). `None` = shared.
    pub netns: Option<String>,
    /// UTS hostname.
    pub hostname: Option<String>,
    /// cgroup `memory.max` in bytes (0 = unlimited).
    pub mem_max: u64,
    /// cgroup `pids.max` (0 = unlimited).
    pub pids_max: u32,
    /// docker `--cpus`: online-CPU count the container advertises = ceil(NanoCpus/1e9). 0 = unlimited.
    pub cpus: u32,
    /// docker `--read-only`: writes to the rootfs/overlay-upper fail EROFS (/proc /dev /sys /tmp /run stay rw).
    pub read_only: bool,
    /// docker `--ulimit`: (name, soft, hard) triples, e.g. ("nofile", 1024, 2048). Serialized to DD_ULIMITS.
    pub ulimits: Vec<(String, u64, u64)>,
    /// USER-ns uid / gid (default: root = 0).
    pub uid: Option<u32>,
    /// USER-ns gid (default: root = 0).
    pub gid: Option<u32>,
    /// Extra environment for the guest process.
    pub env: Vec<(String, String)>,
    /// The guest argv (entrypoint + args).
    pub argv: Vec<String>,
}

impl SpawnConfig {
    /// A config with only the required `work_dir` and `rootfs` set; all other knobs take their defaults.
    pub fn new(work_dir: impl Into<String>, rootfs: impl Into<String>) -> Self {
        SpawnConfig {
            work_dir: work_dir.into(),
            rootfs: rootfs.into(),
            ..Default::default()
        }
    }

    /// Serialize the docker resource knobs (`--cpus`/`--read-only`/`--ulimit`) into the engine's env
    /// contract (DD_CPUS / DD_ROOTFS_RO / DD_ULIMITS), shared by the linux and darwin launch scripts. Env,
    /// not flags, so it survives the mac bridge + the x86 fork-server and is read identically by all three
    /// engines (linux frontends' container_read_resource_env(); the darwinjail init()). Empty when unset ->
    /// byte-identical launch for containers that use none of these.
    fn resource_env(&self) -> String {
        let mut s = String::new();
        if self.cpus > 0 {
            s += &format!("DD_CPUS={} ", self.cpus);
        }
        if self.read_only {
            s += "DD_ROOTFS_RO=1 ";
        }
        if !self.ulimits.is_empty() {
            let u = self
                .ulimits
                .iter()
                .map(|(n, soft, hard)| format!("{}={}:{}", n, soft, hard))
                .collect::<Vec<_>>()
                .join(",");
            s += &format!("DD_ULIMITS={} ", shq(&u));
        }
        s
    }

    /// The `bash -lc` script that launches the container in the given guest's JIT. The flag/env contract
    /// differs per guest OS — linux (jit/jit86) takes the full container flag set + `DDVOL`/`DD_NETNS`
    /// env; darwin (jitdarwin) takes `--rootfs` + `--volume HOST:CONT`. Returns `None` if not built.
    pub fn script(&self, guest: Guest) -> Option<String> {
        let cd = if self.work_dir.is_empty() {
            String::new()
        } else {
            format!("cd {} && ", shq(&self.work_dir))
        };
        let argv = self
            .argv
            .iter()
            .map(|a| shq(a))
            .collect::<Vec<_>>()
            .join(" ");
        let body = if guest.os() == "darwin" {
            // darwinjail: run the native arm64 binary jailed via an interposing dylib (DYLD_INSERT) -- no
            // DBT. The container model (rootfs/lowers/volumes/hostname/limits/publish) is passed as env.
            let jail = guest.jail_dylib()?;
            let mut env = format!("DYLD_INSERT_LIBRARIES={} DD_SANDBOX=1 ", shq(&jail));
            if !self.rootfs.is_empty() {
                env += &format!("DD_ROOTFS={} ", shq(&self.rootfs));
            }
            if !self.lowers.is_empty() {
                env += &format!("DD_LOWERS={} ", shq(&self.lowers.join(",")));
            }
            // ALWAYS set DD_VOLUMES (empty when there are no binds), never conditionally: the daemon's OWN
            // process env carries a DD_VOLUMES (its named-volume ROOT dir, a plain path) that the container
            // would otherwise inherit -- and the jail parses DD_VOLUMES as `HOST:CONT,…` bind specs, so a
            // colon-less dir path made it abort every no-bind mac container with "invalid DD_VOLUMES"
            // Emitting an explicit (possibly empty) value here shadows the inherited one; empty =>
            // zero binds. (Linux uses DDVOL, a different name, so only darwin hit this collision.)
            let v = self
                .volumes
                .iter()
                .map(|v| format!("{}:{}", v.host, v.container))
                .collect::<Vec<_>>()
                .join(",");
            env += &format!("DD_VOLUMES={} ", shq(&v));
            if let Some(h) = &self.hostname {
                if !h.is_empty() {
                    env += &format!("DD_HOSTNAME={} ", shq(h));
                }
            }
            if self.mem_max > 0 {
                env += &format!("DD_MEM_MAX={} ", self.mem_max);
            }
            if self.pids_max > 0 {
                env += &format!("DD_PIDS_MAX={} ", self.pids_max);
            }
            env += &self.resource_env(); // DD_CPUS / DD_ROOTFS_RO / DD_ULIMITS (docker --cpus/--read-only/--ulimit)
            if !self.publish.is_empty() {
                let p = self
                    .publish
                    .iter()
                    .map(|p| format!("{}:{}", p.host, p.container))
                    .collect::<Vec<_>>()
                    .join(",");
                env += &format!("DD_PUBLISH={} ", shq(&p));
            }
            for (k, val) in &self.env {
                env += &format!("{}={} ", k, shq(val));
            }
            // `exec env …` so the container process REPLACES this shell -- it becomes the session leader /
            // foreground of the PTY, so an interactive shell can read the terminal (no job-control stall).
            format!("exec env {env}{argv}")
        } else {
            let jit = guest.jit_path()?;
            let mut env = String::new();
            // The `mac` bridge drops the ambient env, so forward CRASHDBG explicitly when the host sets it
            // (the JIT installs its crash diagnostics on getenv("CRASHDBG")).
            if std::env::var("CRASHDBG").is_ok() {
                env += "CRASHDBG=1 ";
            }
            // forward the persistent-translated-code-cache controls the same way (opt-in; each is a
            // plain getenv() in the engine). Lets `docker run`/tests enable the cross-process cache.
            for k in [
                "DDJIT_PCACHE",
                "DDJIT_PCACHE_DIR",
                "DDJIT_NOPCACHE",
                "COLDPROF",
            ] {
                if let Ok(v) = std::env::var(k) {
                    env += &format!("{}={} ", k, shq(&v));
                }
            }
            if !self.volumes.is_empty() {
                // Per-volume token is `guest:host`; a read-only bind gets a leading `ro:` marker. A guest
                // path always starts with '/', so the `ro:` prefix is unambiguous even if `host` contains
                // colons, and rw volumes serialize EXACTLY as before (byte-identical -> zero matrix change).
                let v = self
                    .volumes
                    .iter()
                    .map(|v| {
                        if v.ro {
                            format!("ro:{}:{}", v.container, v.host)
                        } else {
                            format!("{}:{}", v.container, v.host)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                env += &format!("DDVOL={} ", shq(&v));
            }
            if let Some(ns) = &self.netns {
                env += &format!("DD_NETNS={} ", shq(ns));
            }
            env += &self.resource_env(); // DD_CPUS / DD_ROOTFS_RO / DD_ULIMITS (docker --cpus/--read-only/--ulimit)
            for (k, val) in &self.env {
                env += &format!("{}={} ", k, shq(val));
            }
            let mut f = String::new();
            if let Some(h) = &self.hostname {
                if !h.is_empty() {
                    f += &format!("--hostname {} ", shq(h));
                }
            }
            if self.mem_max > 0 {
                f += &format!("--mem-max {} ", self.mem_max);
            }
            if self.pids_max > 0 {
                f += &format!("--pids-max {} ", self.pids_max);
            }
            if let Some(u) = self.uid {
                f += &format!("--uid {} ", u);
            }
            if let Some(g) = self.gid {
                f += &format!("--gid {} ", g);
            }
            for l in &self.lowers {
                f += &format!("--lower {} ", shq(l));
            }
            if !self.publish.is_empty() {
                let p = self
                    .publish
                    .iter()
                    .map(|p| format!("{}:{}", p.host, p.container))
                    .collect::<Vec<_>>()
                    .join(",");
                f += &format!("--publish {} ", shq(&p));
            }
            // A bare static-PIE guest needs no rootfs; omit the flag so it runs un-jailed.
            if !self.rootfs.is_empty() {
                f += &format!("--rootfs {} ", shq(&self.rootfs));
            }
            // `exec env …` so the JIT REPLACES the wrapper shell -- it becomes the process the caller
            // tracks (pid), so a signal (SIGSTOP/SIGCONT/SIGTERM) hits the JIT, not a dead bash parent.
            format!("exec env {env}{jit} {f}{argv}")
        };
        Some(format!("{cd}{body}"))
    }

    /// (program, args) for a DIRECT SUBPROCESS launch of the container's engine — used by the test harness
    /// and CLI tools that spawn an engine binary and capture its stdio (the `bash -lc` wrapper carries the
    /// `exec env …` so DD_* survive the `mac` bridge, which drops ambient env). On macOS runs `bash -lc
    /// <script>`; on a non-macOS dev host it goes through the `mac` bridge. `None` if the guest's binary
    /// wasn't built. NOTE: the dd-jit runtime itself does NOT use this — it launches via the typed
    /// [`spawn`]/[`spawn_io`] FFI (`ddjit_spawn`, C-side fork, no shell); this is the out-of-process path.
    pub fn command(&self, guest: Guest) -> Option<(String, Vec<String>)> {
        let script = self.script(guest)?;
        Some(if cfg!(target_os = "macos") {
            ("bash".into(), vec!["-lc".into(), script])
        } else {
            ("mac".into(), vec!["bash".into(), "-lc".into(), script])
        })
    }
}

/// Single-quote a string for safe inclusion in the `bash -lc` [`SpawnConfig::script`] (the direct-launch
/// path). `'` is escaped as `'\''`.
fn shq(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('\'');
    for c in s.chars() {
        if c == '\'' {
            o.push_str("'\\''");
        } else {
            o.push(c);
        }
    }
    o.push('\'');
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn linux_script_has_full_flags() {
        // jit_path may be empty in a host without the toolchain; script() returns None then. Guard.
        let mut c = SpawnConfig::new("/work", "img/upper");
        c.lowers = vec!["img/l0".into()];
        c.hostname = Some("box".into());
        c.mem_max = 256 << 20;
        c.publish = vec![PortMap {
            host: 18080,
            container: 80,
        }];
        c.volumes = vec![Volume {
            container: "/data".into(),
            host: "/h".into(),
            ro: false,
        }];
        c.argv = vec!["/bin/sh".into()];
        if let Some(s) = c.script(Guest::LinuxAarch64) {
            assert!(s.contains("--rootfs 'img/upper'") && s.contains("--lower 'img/l0'"));
            assert!(s.contains("--hostname 'box'") && s.contains("--mem-max 268435456"));
            assert!(s.contains("--publish '18080:80'") && s.contains("DDVOL='/data:/h'"));
        }
    }
    #[test]
    fn darwin_script_uses_rootfs_volume() {
        let mut c = SpawnConfig::new("", "/jail");
        c.volumes = vec![Volume {
            container: "/data".into(),
            host: "/h".into(),
            ro: false,
        }];
        c.argv = vec!["/bin/app".into()];
        if let Some(s) = c.script(Guest::DarwinAarch64) {
            // darwinjail takes the container model as env (DD_ROOTFS/DD_VOLUMES), not flags.
            assert!(s.contains("DD_ROOTFS='/jail'") && s.contains("DD_VOLUMES='/h:/data'"));
            assert!(!s.contains("--mem-max") && !s.contains("DDVOL")); // darwin uses DD_VOLUMES, not linux DDVOL
        }
    }
    #[test]
    fn linux_ro_volume_encodes_prefix() {
        let mut c = SpawnConfig::new("/work", "img/upper");
        // one rw, one ro: rw stays `guest:host` (byte-identical), ro gets the leading `ro:` marker.
        c.volumes = vec![
            Volume {
                container: "/rw".into(),
                host: "/h1".into(),
                ro: false,
            },
            Volume {
                container: "/ro".into(),
                host: "/h2".into(),
                ro: true,
            },
        ];
        c.argv = vec!["/bin/sh".into()];
        if let Some(s) = c.script(Guest::LinuxAarch64) {
            assert!(s.contains("DDVOL='/rw:/h1,ro:/ro:/h2'"));
        }
    }
    #[test]
    fn resource_env_serializes_cpus_readonly_ulimits() {
        let mut c = SpawnConfig::new("/work", "img/upper");
        c.argv = vec!["/bin/sh".into()];
        c.cpus = 2;
        c.read_only = true;
        c.ulimits = vec![("nofile".into(), 1024, 2048), ("nproc".into(), 512, 1024)];
        // env contract is engine-agnostic (linux flags path + darwin env path both emit it).
        let re = c.resource_env();
        assert!(re.contains("DD_CPUS=2"));
        assert!(re.contains("DD_ROOTFS_RO=1"));
        assert!(re.contains("DD_ULIMITS='nofile=1024:2048,nproc=512:1024'"));
        for g in [Guest::LinuxAarch64, Guest::DarwinAarch64] {
            if let Some(s) = c.script(g) {
                assert!(
                    s.contains("DD_CPUS=2")
                        && s.contains("DD_ROOTFS_RO=1")
                        && s.contains("DD_ULIMITS=")
                );
            }
        }
        // unset -> empty (byte-identical launch for containers that use none of these)
        let mut d = SpawnConfig::new("/work", "img/upper");
        d.argv = vec!["/bin/sh".into()];
        assert_eq!(d.resource_env(), "");
    }
}
