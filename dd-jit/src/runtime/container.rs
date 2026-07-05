//! [`Container`] + [`ContainerBuilder`] — the typed, fluent container spec, plus the docker-parity
//! environment and `--user` resolution helpers ([`guest_env`], [`resolve_user`]) the builder applies.

use super::error::Error;
use super::image::Image;
use dd_jit_darwin::{Guest, PortMap, SpawnConfig, Volume};

/// The PATH a guest gets when its image sets none — matches the docker daemon's default.
pub const DEFAULT_GUEST_PATH: &str =
    "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// Resolve a merged container environment (image `Config.Env` then `-e` overrides, as `K=V` lines) into
/// the exact lines a guest should see: duplicate keys collapse to their LAST value (an explicit `-e KEY=`
/// overrides the image), forward order is preserved, a default PATH is injected when the image set none,
/// and — when a pseudo-terminal is allocated (`tty`) and the image set no TERM — `TERM=xterm` is added
/// (docker parity, so readline/ncurses/debconf get a real terminal). A non-tty container gets no TERM.
pub fn guest_env(env: &[String], tty: bool) -> Vec<String> {
    let key = |kv: &str| kv.split('=').next().unwrap_or(kv).to_string();
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::with_capacity(env.len());
    // Walk back-to-front so the LAST assignment of each key wins, then restore forward order.
    for kv in env.iter().rev() {
        if seen.insert(key(kv)) {
            out.push(kv.clone());
        }
    }
    out.reverse();
    if !seen.contains("PATH") {
        out.push(DEFAULT_GUEST_PATH.to_string());
    }
    if tty && !seen.contains("TERM") {
        out.push("TERM=xterm".to_string());
    }
    out
}

/// Resolve a docker `--user`/`Config.User` spec to a numeric `(uid, gid)` against a container `rootfs`.
/// Accepts every docker form: `uid`, `name`, `uid:gid`, `name:group`, `uid:group`, `name:gid`. A numeric
/// component is taken verbatim (no file access); a NAME is looked up in `<rootfs>/etc/passwd` (user) or
/// `<rootfs>/etc/group` (group). With no group, a NAME uses its passwd primary gid while a numeric uid
/// defaults to gid 0 (docker semantics). Returns `None` if a name component can't be resolved.
pub fn resolve_user(rootfs: &str, spec: &str) -> Option<(u32, u32)> {
    let (us, gs) = spec.split_once(':').map_or((spec, None), |(u, g)| (u, Some(g)));
    // passwd line: name:passwd:uid:gid:gecos:home:shell — return (uid, primary gid) for a name match.
    let lookup_passwd = |name: &str| -> Option<(u32, u32)> {
        let passwd = std::fs::read_to_string(format!("{rootfs}/etc/passwd")).ok()?;
        passwd.lines().find_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            (f.len() >= 4 && f[0] == name).then(|| Some((f[2].parse().ok()?, f[3].parse().ok()?)))?
        })
    };
    // group line: name:passwd:gid:members — return the gid for a name match.
    let lookup_group = |name: &str| -> Option<u32> {
        let group = std::fs::read_to_string(format!("{rootfs}/etc/group")).ok()?;
        group.lines().find_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            (f.len() >= 3 && f[0] == name).then(|| f[2].parse().ok())?
        })
    };
    let (uid, primary_gid) = match us.parse::<u32>() {
        Ok(n) => (n, None),
        Err(_) => {
            let (u, g) = lookup_passwd(us)?;
            (u, Some(g))
        }
    };
    // A trailing-colon empty group (`"name:"` / `"1000:"`) means "no group" — not a parse failure.
    let gid = match gs.filter(|g| !g.is_empty()) {
        None => primary_gid.unwrap_or(0),
        Some(g) => g.parse().ok().or_else(|| lookup_group(g))?,
    };
    Some((uid, gid))
}

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
                // "DDJIT_PCACHE" is a bare enable gate; the pcache dir's presence enables it engine-side.
                // CRASHDBG/COLDPROF/DDJIT_NOPCACHE are tuning knobs, not container config — dropped here.
                _ => {}
            }
        }
        lc
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
    fn guest_env_dedup_last_wins_and_default_path() {
        // Last assignment of a key wins; forward order preserved; PATH injected when absent.
        let merged = vec!["A=1".to_string(), "B=x".to_string(), "A=2".to_string()];
        let out = guest_env(&merged, false);
        assert_eq!(out, vec!["B=x", "A=2", DEFAULT_GUEST_PATH]);
        // No TERM without a tty.
        assert!(!out.iter().any(|l| l.starts_with("TERM=")));
    }

    #[test]
    fn guest_env_tty_injects_term_and_respects_image_path() {
        let merged = vec!["PATH=/opt/bin".to_string()];
        let out = guest_env(&merged, true);
        assert_eq!(out, vec!["PATH=/opt/bin", "TERM=xterm"]);
    }

    #[test]
    fn guest_env_empty_gets_default_path_and_no_term() {
        // Empty env with no tty: only the default PATH is injected.
        let out = guest_env(&[], false);
        assert_eq!(out, vec![DEFAULT_GUEST_PATH]);
        // With a tty, an empty env gets both PATH and TERM (PATH first, in injection order).
        let out = guest_env(&[], true);
        assert_eq!(out, vec![DEFAULT_GUEST_PATH, "TERM=xterm"]);
    }

    #[test]
    fn guest_env_existing_term_not_overwritten() {
        // A caller-supplied TERM survives; no second TERM is appended.
        let merged = vec!["TERM=screen-256color".to_string()];
        let out = guest_env(&merged, true);
        assert_eq!(out, vec!["TERM=screen-256color", DEFAULT_GUEST_PATH]);
    }

    /// Build a throwaway rootfs with the given `etc/passwd`/`etc/group` contents; returns its path.
    fn make_rootfs(tag: &str, passwd: Option<&str>, group: Option<&str>) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ddjit-resolve-user-{}-{}-{}",
            std::process::id(),
            tag,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("etc")).unwrap();
        if let Some(p) = passwd {
            std::fs::write(dir.join("etc/passwd"), p).unwrap();
        }
        if let Some(g) = group {
            std::fs::write(dir.join("etc/group"), g).unwrap();
        }
        dir
    }

    const PASSWD: &str = "root:x:0:0:root:/root:/bin/sh\npostgres:x:70:70:postgres:/var/lib/postgresql:/bin/sh\n";
    const GROUP: &str = "root:x:0:\npostgres:x:70:\nstaff:x:50:\n";

    #[test]
    fn resolve_user_numeric_uid_defaults_gid_zero() {
        // A bare numeric uid needs no rootfs and gets gid 0 (docker semantics).
        let dir = make_rootfs("num", None, None);
        assert_eq!(resolve_user(dir.to_str().unwrap(), "1000"), Some((1000, 0)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_user_numeric_uid_and_gid() {
        let dir = make_rootfs("numnum", None, None);
        assert_eq!(resolve_user(dir.to_str().unwrap(), "1000:2000"), Some((1000, 2000)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_user_name_uses_passwd_primary_gid() {
        let dir = make_rootfs("name", Some(PASSWD), Some(GROUP));
        assert_eq!(resolve_user(dir.to_str().unwrap(), "postgres"), Some((70, 70)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_user_name_group_via_group_lookup() {
        // `name:group` resolves the group through /etc/group.
        let dir = make_rootfs("namegrp", Some(PASSWD), Some(GROUP));
        assert_eq!(resolve_user(dir.to_str().unwrap(), "postgres:postgres"), Some((70, 70)));
        // A cross group: user postgres with the numeric-named `staff` group by name.
        assert_eq!(resolve_user(dir.to_str().unwrap(), "postgres:staff"), Some((70, 50)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_user_trailing_colon_empty_group_is_gid_zero() {
        // A numeric uid with a trailing empty group: not a parse failure, gid falls back to 0.
        let dir = make_rootfs("trail", None, None);
        assert_eq!(resolve_user(dir.to_str().unwrap(), "1000:"), Some((1000, 0)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_user_name_trailing_colon_keeps_primary_gid() {
        // A NAME with a trailing empty group keeps its passwd primary gid (not 0).
        let dir = make_rootfs("nametrail", Some(PASSWD), Some(GROUP));
        assert_eq!(resolve_user(dir.to_str().unwrap(), "postgres:"), Some((70, 70)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_user_unresolvable_name_returns_none() {
        // No passwd file at all: a name can't be resolved.
        let dir = make_rootfs("missing", None, None);
        assert_eq!(resolve_user(dir.to_str().unwrap(), "postgres"), None);
        // Present passwd but the name isn't in it.
        let dir2 = make_rootfs("absent", Some(PASSWD), Some(GROUP));
        assert_eq!(resolve_user(dir2.to_str().unwrap(), "nobody"), None);
        // Known user, but the named group is absent from /etc/group.
        assert_eq!(resolve_user(dir2.to_str().unwrap(), "postgres:ghosts"), None);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
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
    }

    #[test]
    fn launch_config_overlay_lowers_copy_across() {
        // Overlay lowers are a typed passthrough (highest-priority first).
        let c = Container::builder(Image::overlay("/upper", ["/lo0", "/lo1"]).guest(Guest::LinuxAarch64))
            .build()
            .unwrap();
        let lc = c.launch_config();
        assert_eq!(lc.rootfs, "/upper");
        assert_eq!(lc.lowers, vec!["/lo0".to_string(), "/lo1".to_string()]);
    }

    #[test]
    fn launch_config_defaults_surface_nothing() {
        // A bare container: no env pairs, so every optional/env-backed field stays at its default.
        let c = Container::builder(Image::from_rootfs("/img").guest(Guest::LinuxAarch64))
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
        assert!(lc.argv.is_empty());
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
