use super::*;

mod etchosts;
mod live;
mod net;
mod spec;
mod tmpfs;

pub(crate) use live::*;
pub(crate) use tmpfs::*;
use spec::*;

/// Docker's default container PATH (moby's `system.DefaultPathEnv` for unix). Used when neither the
/// image config nor a `-e PATH=` override supplies one, so bare commands in the standard sbin/bin dirs
/// (e.g. alpine's `apk` in /sbin) resolve without an absolute path.
/// Translate the daemon's Docker container model into a typed `dd_jit::Container`. The daemon states
/// WHAT the container is; dd-jit owns HOW it launches. `None` only if the spec can't be built.
pub(crate) fn spawn_container(
    c: &Container,
    volumes_dir: &str,
    vols: &[Vol],
    bridge: Option<(String, String)>,
) -> Option<JitContainer> {
    let guest = c.arch.unwrap_or(Guest::LinuxAarch64);
    // Per-container copy-on-write: the private writable UPPER (`c.upper`) overlays the read-only image
    // rootfs (the lower) so guest writes/whiteouts never mutate the shared image. Linux guests only —
    // darwin runs natively jailed (its lower is the host `/`); a flat rootfs is used when no upper exists.
    let overlay = guest.os() != "darwin" && !c.upper.is_empty();
    let rootfs = if overlay { c.upper.clone() } else { c.rootfs.clone() };
    let image = if overlay {
        Image::overlay(rootfs, [c.rootfs.clone()])
    } else if guest.os() == "darwin" {
        // darwinjail: the host filesystem is the read-only lower so native binaries find their /nix deps.
        Image::overlay(rootfs, ["/".to_string()])
    } else {
        Image::from_rootfs(rootfs)
    }
    .guest(guest);

    // Every knob below is a typed dd-jit API call — the daemon states WHAT the container is; dd-jit owns
    // HOW it launches (the wire dialect, overlay/netns/pcache/fsgen encoding, the engine invocation).
    let mut b = JitContainer::builder(image)
        .cmd(c.cmd.clone())
        // `-w DIR` (WorkingDir): the guest's initial cwd.
        .cwd(c.working_dir.clone())
        // container env (image ENV + `-e`) forwarded EXACTLY to the guest (docker env semantics), never
        // the daemon/host environment.
        .guest_env(&c.env, c.tty)
        // Effective UTS hostname: the user's `--hostname`, else Docker's default 12-char short id.
        .hostname(etchosts::eff_hostname(&c.id, &c.hostname))
        .memory_bytes(c.memory.max(0) as u64)
        .pids(c.pids_limit.max(0) as u32)
        // `--cpus`: NanoCpus -> ceil to whole online CPUs (0 = unlimited).
        .cpus(nano_cpus_to_cpus(c.nano_cpus))
        .read_only(c.readonly_rootfs);
    for u in &c.ulimits {
        b = b.ulimit(u.name.clone(), u.soft.max(0) as u64, u.hard.max(0) as u64);
    }
    // (The operator-level persistent translated-code cache is owned by dd_jit::Runtime, applied to every
    // container it launches — the daemon states no cache policy of its own.)
    // `--network host` shares the host network; otherwise isolate in a private loopback named by the
    // TARGET container's id (a `docker exec` sets `netns_key` so it joins the container's 127.0.0.1
    // instead of its own). Truncated to 40 chars to match the engine.
    if c.network_mode != "host" {
        let ns_key = c.netns_key.as_deref().unwrap_or(&c.id);
        b = b.private_network(ns_key[..ns_key.len().min(40)].to_string());
    }
    // daemon-write coherence: hand every Linux engine the shared external-writer generation file so a
    // daemon-side write into the live fs (docker cp's PUT, the exec /etc rewrites) drops the engine's
    // path/metadata caches and is guest-visible by its next syscall.
    if guest.os() != "darwin" {
        let key = c.netns_key.as_deref().unwrap_or(&c.id);
        b = b.write_coherence_file(crate::util::fsgen_ensure(key).to_string_lossy().into_owned());
    }
    // `--network none`: no external egress.
    b = b.net_isolate(c.network_mode == "none");
    // per-network AF_UNIX virtual switch: in-subnet container<->container TCP.
    if let Some((netid, ip)) = bridge {
        b = b.bridge(netid, ip);
    }
    // `--user U[:G]` / `Config.User`: resolve a NAME against the image rootfs and surface the uid/gid to
    // the guest (getuid/getgid/setuid) — lets e.g. the postgres entrypoint see `id -u != 0` and skip its
    // gosu re-exec. An unresolvable name leaves the guest's default identity.
    b = b.user_spec(&c.rootfs, &c.user);
    // `--security-opt sandbox`/`untrusted`: run under the untrusted-guest sentry (deny-default OS sandbox
    // + syscall forwarding). The daemon-wide default sandbox (operator env) is owned by dd_jit::Runtime.
    b = b.sandbox(wants_sandbox(&c.security_opt));
    // `-v src:dst[:opts]` / Binds. `ro` marks the mount read-only (write-intent EROFS under the mount).
    for bd in &c.binds {
        if let Some((host, dst, ro)) = parse_bind(bd) {
            b = b.bind(resolve_mount_src(host, volumes_dir, vols), dst, ro);
        }
    }
    // `--mount` / HostConfig.Mounts: type=bind Source is a host path; type=volume Source is a named volume.
    for m in &c.mounts {
        if m.target.is_empty() {
            continue;
        }
        let host = if m.typ == "bind" {
            m.source.clone()
        } else {
            resolve_mount_src(&m.source, volumes_dir, vols)
        };
        if host.is_empty() {
            continue;
        }
        b = b.bind(host, m.target.clone(), m.read_only);
    }
    // `--tmpfs DST` / `--mount type=tmpfs`: a fresh empty scratch dir path-spliced over the target, keyed
    // by the container id (an exec shares it), cleared to empty on each start (clear_tmpfs).
    let tmpfs_key = c.netns_key.as_deref().unwrap_or(&c.id);
    for target in c.tmpfs.keys() {
        if target.is_empty() {
            continue;
        }
        b = b.bind(tmpfs_hostdir(tmpfs_key, target), target.clone(), false);
    }
    // Published ports. The daemon owns the process-independent host forwarder (containers/ports.rs), so
    // the engine must NOT start its own in-process listener (which raced/broke prefork servers) — but it
    // still gets the port map to report container ports + keep the guest-side switch redirect.
    for p in crate::containers::parse_publish(&c.publish)
        .into_iter()
        .filter(|p| p.proto == "tcp")
    {
        b = b.publish(p.host_port, p.container_port);
    }
    b = b.external_port_forwarder(!c.publish.is_empty());
    // macOS containers (darwinjail): forward the image ENV as real process env (the native jailed binaries
    // see it; DD_GUEST_ENV is Linux-only) with a nix-first PATH default, and wrap the entry in the in-jail
    // bash so a bare command resolves via the in-jail PATH and the entry shell stays inside the jail (a
    // login shell or `/bin/sh` would source the host profile / run arm64e tools and escape the arm64 jail).
    if guest.os() == "darwin" {
        let mut have_path = false;
        for kv in &c.env {
            if let Some((k, v)) = kv.split_once('=') {
                if k == "PATH" {
                    have_path = true;
                }
                b = b.env(k.to_string(), v.to_string());
            }
        }
        if !have_path {
            b = b.env("PATH", "/profile/bin:/usr/bin:/bin");
        }
        b = b.argv(darwin_wrapped_argv(&c.rootfs, &c.cmd));
    }
    b.build().ok()
}
