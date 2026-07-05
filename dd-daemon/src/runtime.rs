#![allow(unused_imports, dead_code)]
use crate::archive::*;
use crate::build::*;
use crate::containers::*;
use crate::images::*;
use crate::model::*;
use crate::networks::*;
use crate::registry::{Client, Credentials, ImageRef};
use crate::system::*;
use crate::util::*;
use crate::volumes::*;
use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use ddjit::{Container as JitContainer, Error as JitError, Guest, Image, PortMap, Runtime as JitRuntime, SpawnConfig, Stdio3, Volume};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc, watch, Mutex};

/// Cap on the retained `docker logs` replay buffer (per container/exec). A chatty or long-lived guest
/// would otherwise grow `live.log_chunks` without bound and OOM the daemon. When a new chunk pushes the
/// buffer over this, the oldest chunks are dropped from the front — standard log-rotation behavior, so
/// `docker logs` shows the most-recent ≤ 8 MiB of output.
const LOG_CHUNKS_CAP_BYTES: usize = 8 * 1024 * 1024;

/// Write the LIVE reach-by-name table for one user-defined network into the engine's per-network switch
/// dir (`/tmp/.ddbr-<netid[..40]>/.names`), one `ip\tname` line per endpoint. The in-engine 127.0.0.11
/// resolver reads this file per DNS query (net.c `dns_local_lookup`) BEFORE falling through to the macOS
/// host resolver, so a container resolves a same-network peer by name even if that peer joined AFTER this
/// container launched (its `/etc/hosts` snapshot, seeded once at start, can't see it). The `.40s`
/// truncation matches the engine's `snprintf` for `DD_NETBR`, so the path byte-matches what the engine
/// computes. Best-effort: never fail a spawn on an I/O error.
fn write_net_names(netid: &str, endpoints: &HashMap<String, Endpoint>) {
    let dir = format!("/tmp/.ddbr-{}", &netid[..netid.len().min(40)]);
    let _ = std::fs::create_dir_all(&dir); // the engine also mkdir 0700's this; either creating it is fine
    let mut body = String::new();
    for e in endpoints.values() {
        if !e.ip.is_empty() && !e.name.is_empty() {
            body.push_str(&format!("{}\t{}\n", e.ip, e.name));
        }
    }
    let _ = std::fs::write(format!("{dir}/.names"), body);
}


/// Per-container host scratch dir backing a `--tmpfs`/`--mount type=tmpfs` mount at `target`. A plain
/// host dir (path-spliced over the guest target like a bind); it is cleared fresh on every container
/// start (see [`clear_tmpfs`]), so the guest sees an empty mount each run — the "in-memory tmpfs" contract
/// that matters to callers. Keyed by CONTAINER id (an exec passes the container's id via `netns_key`) so
/// an exec into the container sees the same tmpfs. Size/mode options are metadata only (not a real tmpfs).
pub(crate) fn tmpfs_hostdir(cid: &str, target: &str) -> String {
    let slug: String = target
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    crate::util::dd_home()
        .join("containers")
        .join(cid)
        .join("tmpfs")
        .join(slug)
        .to_string_lossy()
        .into_owned()
}

/// Reset every `--tmpfs` target of a container to a FRESH empty dir. Called on each real container start
/// (never on an exec, which must not wipe the container's live tmpfs). Best-effort.
pub(crate) fn clear_tmpfs(c: &Container) {
    for target in c.tmpfs.keys() {
        let d = tmpfs_hostdir(&c.id, target);
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::create_dir_all(&d);
    }
}

/// Docker's default container PATH (moby's `system.DefaultPathEnv` for unix). Used when neither the
/// image config nor a `-e PATH=` override supplies one, so bare commands in the standard sbin/bin dirs
/// (e.g. alpine's `apk` in /sbin) resolve without an absolute path.
/// Translate the container into a typed [`SpawnConfig`] and run it in the matching guest's JIT.
/// Named-volume binds (`name:/path`, no leading `/`) are resolved against `volumes_dir`.
/// Build the (program, args) that launches this container in the matching guest's JIT. `None` if no JIT
/// was built for the image's arch.
pub(crate) fn spawn_cfg(
    c: &Container,
    volumes_dir: &str,
    vols: &[Vol],
    bridge: Option<(String, String)>,
) -> Option<(String, Vec<String>)> {
    spawn_container(c, volumes_dir, vols, bridge).and_then(|c| c.command())
}

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
        .hostname(if c.hostname.is_empty() {
            c.id[..c.id.len().min(12)].to_string()
        } else {
            c.hostname.clone()
        })
        .memory_bytes(c.memory.max(0) as u64)
        .pids(c.pids_limit.max(0) as u32)
        // `--cpus`: NanoCpus -> ceil to whole online CPUs (0 = unlimited).
        .cpus(if c.nano_cpus > 0 {
            ((c.nano_cpus + 999_999_999) / 1_000_000_000) as u32
        } else {
            0
        })
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
    let sandbox = c.security_opt.iter().any(|o| {
        let o = o.to_ascii_lowercase();
        o.contains("sandbox") || o.contains("untrusted")
    });
    b = b.sandbox(sandbox);
    // Resolve a mount source to a host path: an absolute path is a bind; anything else is a named volume
    // (its registered mountpoint or a dir under `volumes_dir`). Shared by `-v`/Binds and `--mount`.
    let resolve_src = |src: &str| -> String {
        if src.starts_with('/') {
            src.to_string()
        } else if let Some(v) = vols.iter().find(|v| v.name == src) {
            v.mountpoint.clone()
        } else {
            PathBuf::from(volumes_dir).join(src).to_string_lossy().into_owned()
        }
    };
    // `-v src:dst[:opts]` / Binds. `ro` marks the mount read-only (write-intent EROFS under the mount).
    for bd in &c.binds {
        if let Some((host, dst, ro)) = parse_bind(bd) {
            b = b.bind(resolve_src(host), dst, ro);
        }
    }
    // `--mount` / HostConfig.Mounts: type=bind Source is a host path; type=volume Source is a named volume.
    for m in &c.mounts {
        if m.target.is_empty() {
            continue;
        }
        let host = if m.typ == "bind" { m.source.clone() } else { resolve_src(&m.source) };
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
        let wrapper = format!("{}/profile/bin/bash", c.rootfs);
        let mut argv = c.cmd.clone();
        if argv.is_empty() {
            argv = vec!["bash".into()];
        } else if matches!(argv[0].as_str(), "/bin/sh" | "sh" | "/bin/bash" | "bash") {
            argv[0] = "bash".into();
        }
        let mut wrapped = vec![wrapper, "-c".into(), "exec \"$@\"".into(), "dd-mac".into()];
        wrapped.extend(argv);
        b = b.argv(wrapped);
    }
    b.build().ok()
}

/// Spawn the container's guest process live (piped stdio) and wire its IO into `live`: stdout/stderr fan
/// out to attached clients + the log buffers; on exit, the container's status/exit-code are finalized.
/// Idempotent per container (start is a no-op if already running). Returns false if no JIT for the arch.
pub(crate) async fn spawn_live(app: &App, c: &Container, vols: &[Vol], live: Arc<Live>) -> bool {
    use std::sync::atomic::Ordering;
    if live.started.swap(true, Ordering::SeqCst) {
        return true; // already started
    }
    // `--tmpfs`: reset this container's tmpfs targets to a fresh empty dir for the new run. Skipped for an
    // exec (netns_key is Some) — an exec must see the container's LIVE tmpfs, not wipe it.
    if c.netns_key.is_none() {
        clear_tmpfs(c);
    }
    // Reach-by-name identity key. A `docker exec` runs with `c.id` set to the EXEC id (not a network
    // endpoint), but it JOINS the target container's network (`netns_key`), so it must inherit that
    // container's bridge (netid, ip) + /etc/hosts view — otherwise the exec'd process gets no DD_NETBR/
    // DD_IP and can neither reach peers by IP nor consult the live resolver. A normal container has
    // `netns_key == None` and this resolves to its own id (unchanged behaviour).
    let lookup_id = c.netns_key.clone().unwrap_or_else(|| c.id.clone());
    // netstack PR2: this container's (network-id, assigned-ip) from PR1's per-network endpoints map,
    // plus the /etc/hosts reach-by-name table (this container + same-network peers, name -> ip). We also
    // refresh a LIVE per-user-network names file the in-engine 127.0.0.11 resolver consults, so a peer
    // that appears AFTER this container launched (its /etc/hosts snapshot is frozen at launch) is still
    // resolvable — the reach-by-name fix (see net.c dns_build_response local-name lookup).
    let (bridge, hosts) = {
        let g = app.inner.lock().await;
        let bridge = g.networks.iter().find_map(|n| {
            n.endpoints
                .get(&lookup_id)
                .map(|e| (n.id.clone(), e.ip.clone()))
        });
        // netstack reach-by-name: a guest's getaddrinfo("peer") must resolve to the peer's network IP so
        // the per-network br_* switch (PR2) can carry the connect. Docker drives this via embedded DNS;
        // the equivalent here is to seed /etc/hosts with every endpoint on the network(s) this container
        // is attached to. musl/glibc read /etc/hosts before any nameserver, so the resolve is local.
        let mut hosts = String::from("127.0.0.1\tlocalhost\n");
        // own entry (once): own-ip  own-name [hostname]. Absent for --network host/none (no endpoint).
        if let Some(own) = g.networks.iter().find_map(|n| n.endpoints.get(&lookup_id)) {
            let mut names = own.name.clone();
            if !c.hostname.is_empty() && c.hostname != own.name {
                names.push(' ');
                names.push_str(&c.hostname);
            }
            hosts.push_str(&format!("{}\t{}\n", own.ip, names));
        }
        // peers: every OTHER endpoint on any network this container is a member of.
        for n in &g.networks {
            if !n.endpoints.contains_key(&lookup_id) {
                continue;
            }
            for (cid, e) in &n.endpoints {
                if cid != &lookup_id {
                    hosts.push_str(&format!("{}\t{}\n", e.ip, e.name));
                }
            }
        }
        // Refresh the live resolver name file for every USER-defined network this container is on. Docker
        // withholds reach-by-name on the default `bridge` (only user networks get embedded-DNS names), so
        // predefined networks are skipped. The file is rewritten on EVERY start, so it always reflects the
        // current endpoint set (late-joining peers included). The engine reads it per DNS query.
        for n in &g.networks {
            if !n.endpoints.contains_key(&lookup_id) || is_predefined(&n.name) {
                continue;
            }
            write_net_names(&n.id, &n.endpoints);
        }
        (bridge, hosts)
    };
    // Write the table best-effort — Docker manages /etc/hosts, so overwriting with the generated
    // reach-by-name content is correct; never fail the spawn on an I/O error.
    {
        // Write /etc/hosts into the writable layer the guest actually sees: the per-container overlay UPPER
        // (so it shadows the image's /etc/hosts via the overlay and never drifts the shared image), or the
        // flat rootfs when there's no upper (darwin / legacy containers).
        let base = if c.upper.is_empty() {
            &c.rootfs
        } else {
            &c.upper
        };
        let etc = format!("{base}/etc");
        let _ = std::fs::create_dir_all(&etc);
        if let Err(e) = std::fs::write(format!("{etc}/hosts"), &hosts) {
            if std::env::var("DD_DEBUG").is_ok() {
                eprintln!(
                    "[live] {} write /etc/hosts failed: {e}",
                    &c.id[..c.id.len().min(12)]
                );
            }
        }
        // Provision /etc/resolv.conf with dd's embedded nameserver (mirrors Docker's 127.0.0.11). Many base
        // images ship an EMPTY /etc/resolv.conf (Docker fills it at runtime); without this the guest has no
        // nameserver at all and every DNS lookup fails (apt-get "Ign"/"failed to fetch"). The engine
        // intercepts UDP/TCP :53 to this address and resolves via the macOS host resolver (net.c dns_*),
        // so the container inherits the host's DNS config -- including a corporate VPN's split-DNS -- exactly
        // like the ddcli-mac container. `ndots:0` matches Docker's embedded-DNS resolv.conf (names are tried
        // as-is first; we have no search domains to append). Written into the SAME writable layer as
        // /etc/hosts so it shadows the image's file via the overlay. --network none still gets the file, but
        // the engine leaves :53 un-intercepted under DD_NET_ISOLATE, so name resolution fails as Docker's
        // null network does. Best-effort: never fail the spawn on an I/O error.
        let resolv = "nameserver 127.0.0.11\noptions ndots:0\n";
        if let Err(e) = std::fs::write(format!("{etc}/resolv.conf"), resolv) {
            if std::env::var("DD_DEBUG").is_ok() {
                eprintln!(
                    "[live] {} write /etc/resolv.conf failed: {e}",
                    &c.id[..c.id.len().min(12)]
                );
            }
        }
        // /etc/hostname: Docker generates this beside /etc/hosts and /etc/resolv.conf (the container's UTS
        // name + newline), shadowing any image copy via the overlay upper. Same value spawn_cfg passes as
        // DD_HOSTNAME -> gethostname(), so the two agree (user --hostname, else the 12-char short id).
        let eff_hostname = if c.hostname.is_empty() {
            c.id[..c.id.len().min(12)].to_string()
        } else {
            c.hostname.clone()
        };
        if let Err(e) = std::fs::write(format!("{etc}/hostname"), format!("{eff_hostname}\n")) {
            if std::env::var("DD_DEBUG").is_ok() {
                eprintln!(
                    "[live] {} write /etc/hostname failed: {e}",
                    &c.id[..c.id.len().min(12)]
                );
            }
        }
        // for an exec/health-probe spawn those /etc writes just landed in a LIVE container's upper
        // (the container's engine is already running with warm caches) — bump its external-writer
        // generation so every running engine drops its caches. For a fresh container start this is a
        // harmless no-op signal: no engine is up yet, and the engine snapshots the current value at boot.
        crate::util::fsgen_bump(&lookup_id);
    }
    // start the daemon-owned, process-independent host→container port forwarders. Idempotent (the
    // restart path re-enters here but the listeners persist), a no-op for a container that publishes
    // nothing, and a no-op for an exec (empty publish). Bound now so `docker port`/`ps`/inspect report a
    // live, deterministic host port and a re-listening server stays reachable. `bridge` is still owned here
    // (spawn_cfg consumes it just below); we only borrow it.
    crate::containers::ports::start_for(c, &bridge);
    // No launch command means the JIT engine for this guest arch isn't bundled (e.g. a darwin-only build
    // shipped without ddjit-linux_*). Surface a CLEAN error (exit 127, like every other spawn failure) so an
    // interactive `docker run -it` exits with a message instead of hanging forever on a stream that never
    // opens -- the missing-engine hang that looked like a frozen, Ctrl-C-deaf shell.
    let Some(container) = spawn_container(c, &app.volumes_dir, vols, bridge) else {
        return live_fail(app, &c.id, &live, "dd: failed to build container spec".into()).await;
    };
    // Launch + supervise the guest via the dd-jit runtime API: it spawns the engine (piped or PTY stdio),
    // feeds stdin from this container's channel, and pumps stdout/stderr INTO this container's live `out`
    // broadcast + rotated `log_chunks`. The daemon owns only the Docker bookkeeping (status/events/health/
    // restart/--rm) in the reaper below -- the process mechanics live in dd_jit::Runtime::start_into.
    let stdin_rx = live
        .stdin_rx
        .lock()
        .await
        .take()
        .expect("stdin_rx is consumed exactly once per container start");
    // dd-jit owns the operator cache/sandbox policy; the daemon only supplies its storage location so the
    // persistent cache lands under the dd home (reported by `system df`, cleared by `system prune`).
    let rt = JitRuntime::new()
        .expect("dd-jit runtime")
        .cache_dir(crate::util::dd_home().join("pcache").to_string_lossy().into_owned());
    let launched = match rt.start_into(
        &container,
        Stdio3 { tty: c.tty },
        live.out.clone(),
        live.log_chunks.clone(),
        stdin_rx,
    ) {
        Ok(l) => l,
        Err(JitError::NoBackend(guest)) => {
            // No engine for this guest arch is bundled -- surface a CLEAN error (like every other spawn
            // failure) so an interactive `docker run -it` exits with a message instead of hanging on a
            // stream that never opens (the missing-engine hang that looked like a frozen, Ctrl-C-deaf shell).
            return live_fail(app, &c.id, &live,
                format!("dd: no JIT engine for {} guests in this build (ddjit-{} missing) -- cannot start container",
                    guest.target(), guest.target())).await;
        }
        Err(e) => return live_fail(app, &c.id, &live, format!("jit exec failed: {e}")).await,
    };
    *live.pty_master.lock().unwrap() = launched.pty_master;
    let (mut child, io_handles) = (launched.child, launched.io_handles);

    *live.pid.lock().unwrap() = child.id(); // remember the pid so pause can SIGSTOP/SIGCONT it
                                            // HEALTHCHECK (§8.3-1): a real container (not an exec) with a resolved probe gets a background monitor
                                            // tied to THIS Live — it probes on `interval`, flips `State.Health` starting→healthy/unhealthy per
                                            // `retries`/`start_period`, and exits when this Live's process dies (so a restart spawns a fresh one).
    if c.netns_key.is_none() {
        if let Some(hcfg) = c.healthcheck.clone() {
            let app2 = app.clone();
            let cid2 = c.id.clone();
            let cont = c.clone();
            let vols2 = vols.to_vec();
            let exit_rx = live.exit_rx.clone();
            tokio::spawn(async move {
                health_monitor(app2, cid2, cont, vols2, hcfg, exit_rx).await;
            });
        }
    }
    let app = app.clone();
    let cid = c.id.clone();
    let auto_remove = c.auto_remove; // `--rm`: drop the container from state once it exits (see below)
    let dbg = std::env::var("DD_DEBUG").is_ok();
    tokio::spawn(async move {
        let code = child.wait().await.ok().and_then(|s| s.code()).unwrap_or(-1) as i64;
        if dbg {
            eprintln!("[live] {} exited code={code}", &cid[..12]);
        }
        *live.pty_master.lock().unwrap() = None;
        // Signal the natural exit IMMEDIATELY — flip status + fire the exit watch the instant the guest's
        // own process dies, so an interactive `docker run`/`ddcli mac` returns at once when the user types
        // `exit`. CRITICAL: this must NOT be gated on draining the PTY/pipe reader tasks. A stray
        // grandchild that inherited the slave/pipe fds can keep those readers from ever hitting EOF, and
        // the previous code awaited them BEFORE flipping status — which made `exit` hang for as long as
        // such a child lived (the daemon never told `docker run`/`/wait` the container was done). We drain
        // the readers AFTER, with a bounded grace, purely to finalize the log snapshot.
        {
            let mut g = app.inner.lock().await;
            if let Some(cc) = g.containers.get_mut(&cid) {
                cc.status = "exited".into();
                cc.exit_code = code;
                cc.finished_at = now_secs();
                cc.finished_at_ns = now_nanos();
            }
            let (cname, cimage) = g
                .containers
                .get(&cid)
                .map(|c| (c.name.clone(), c.image.clone()))
                .unwrap_or_default();
            crate::events::emit_event(
                &app.events,
                "container",
                "die",
                &cid,
                serde_json::json!({"exitCode": code.to_string(), "name": cname, "image": cimage}),
            );
            save_state(&g, &app.state_path);
        }
        let _ = live.exit.send(Some(code));
        // Drain the reader tasks so the final output lands in the log buffer, but never block the reaper
        // forever: a lingering child still holding the fds open would keep a reader from EOFing, so cap
        // the wait. In the normal case (the guest's process tree fully exited) the slave/pipes close at
        // once and each reader returns in well under this grace, so the snapshot below is complete.
        for h in io_handles {
            let _ = tokio::time::timeout(std::time::Duration::from_millis(500), h).await;
        }
        // The pumps have now flushed every byte into the `out` broadcast, so signal output-complete.
        // Streaming consumers (attach/exec hijack, `logs -f`) wait on this -- NOT on the immediate `exit`
        // above -- before closing, so a fast-exiting command's tail is never lost to the pump race.
        let _ = live.out_done.send(true);
        {
            let mut g = app.inner.lock().await;
            if let Some(cc) = g.containers.get_mut(&cid) {
                // Finalize the per-stream snapshots downstream code reads (cc.stdout/cc.stderr) by
                // filtering the ordered log by stream. The ordered log itself is LEFT INTACT on the
                // retained Live so `docker logs` can still replay stdout/stderr interleaved after exit
                // (the Live stays in the map for non-`--rm` containers).
                let (mut so, mut se) = (Vec::new(), Vec::new());
                for (_, kind, data) in live.log_chunks.lock().await.iter() {
                    if *kind == 1 {
                        so.extend_from_slice(data);
                    } else {
                        se.extend_from_slice(data);
                    }
                }
                cc.stdout = so;
                cc.stderr = se;
            }
        }
        // Exec cleanup: a `docker exec` runs through spawn_live with the EXEC id as `cid` (never a
        // `containers` entry). Record the exit code on the Exec, then drop ONLY its Live now that it has
        // exited — the Live's 8 MiB log buffer + channels are the real leak; without this every exec
        // leaks one. We KEEP the (tiny) Exec record so a post-exit `docker exec inspect` still returns
        // the real code (it reads `exec.exit_code` once the Live is gone). Independent of `--rm`, which
        // governs the parent CONTAINER, not the exec. Return: AutoRemove/RestartPolicy below are
        // container-only (and would no-op on an exec id anyway).
        {
            let mut g = app.inner.lock().await;
            if let Some(e) = g.execs.get_mut(&cid) {
                e.exit_code = code;
                g.live.remove(&cid);
                return;
            }
        }
        // `--rm` (AutoRemove): drop the container from state now that it has exited and its final
        // status/logs are recorded. AutoRemove and RestartPolicy are mutually exclusive in docker, so a
        // removed container is never a restart candidate — return before the supervisor runs. Anything
        // waiting on the exit watch (the `docker run --rm` foreground client) already saw Some(code).
        if auto_remove {
            crate::containers::ports::stop(&cid); // free published host ports on `--rm` teardown
            let mut g = app.inner.lock().await;
            if let Some(dc) = g.containers.remove(&cid) {
                crate::events::emit_event(
                    &app.events,
                    "container",
                    "destroy",
                    &cid,
                    serde_json::json!({"name": dc.name, "image": dc.image}),
                );
                for n in g.networks.iter_mut() {
                    leave_network(n, &cid);
                }
                // Reclaim the private writable upper layer (mirrors `docker rm`; the shared image is untouched).
                discard_container_layer(&dc.upper);
            }
            g.live.remove(&cid);
            save_state(&g, &app.state_path);
            return;
        }
        // RestartPolicy supervisor: re-run the container per `--restart` unless it was deliberately
        // stopped (stop/kill/rm set stop_requested). A no-op for the default `no`/empty policy, so the
        // common `docker run` path is untouched.
        if !live
            .stop_requested
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            maybe_restart(&app, &cid, code).await;
        }
    });
    true
}

/// Apply the container's `--restart` policy after an exit. Restarts on `always`/`unless-stopped`
/// (any exit) or `on-failure` (non-zero exit, up to MaximumRetryCount). `no`/empty never restarts.
/// A short backoff avoids a tight crash-loop. Spawns a fresh [`Live`] (the old one is spent) and
/// re-enters [`spawn_live`], whose reaper re-applies this policy on the next exit.
fn maybe_restart<'a>(
    app: &'a App,
    cid: &'a str,
    code: i64,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        let (name, max_retry, count, c, vols) = {
            let g = app.inner.lock().await;
            let Some(c) = g.containers.get(cid) else {
                return;
            };
            // Don't restart a container that's already been removed or re-started elsewhere, nor one the user
            // deliberately stopped (durable `manually_stopped` — the persisted HasBeenManuallyStopped; keeps
            // `unless-stopped`/`always` from resurrecting a `docker stop`ped container even across a restart).
            if c.status != "exited" || c.manually_stopped {
                return;
            }
            (
                c.restart_policy.name.clone(),
                c.restart_policy.max_retry,
                c.restart_count,
                c.clone(),
                g.volumes.clone(),
            )
        };
        let should = match name.as_str() {
            "always" | "unless-stopped" => true,
            "on-failure" => code != 0 && (max_retry <= 0 || count < max_retry),
            _ => false, // "no" / "" / unknown
        };
        if !should {
            return;
        }
        // §8.3-4 state machine: the container is `restarting` for the duration of the backoff window (Moby's
        // `SetRestarting` keeps Running=true through it) — inspect reports State.Restarting=true meanwhile.
        {
            let mut g = app.inner.lock().await;
            if let Some(cc) = g.containers.get_mut(cid) {
                if cc.status == "exited" {
                    cc.status = "restarting".into();
                }
            }
            save_state(&g, &app.state_path);
        }
        // Backoff (capped) so a container that exits immediately doesn't spin the daemon.
        let backoff = (100u64 << (count.clamp(0, 6) as u32)).min(10_000);
        tokio::time::sleep(std::time::Duration::from_millis(backoff)).await;
        // Install a fresh Live (the prior one is "started"/spent), mark running, bump the restart count.
        let live = Live::new(c.tty);
        {
            let mut g = app.inner.lock().await;
            match g.containers.get(cid) {
                // Re-check: a stop/rm may have raced in during the backoff. Accept the `restarting` status we
                // set before the backoff (as well as `exited` for the legacy path).
                Some(cc) if cc.status == "exited" || cc.status == "restarting" => {}
                _ => return,
            }
            // A deliberate `docker stop`/`kill`/`rm` during the backoff sets `stop_requested` on the OLD,
            // spent Live (still the `g.live[cid]` entry until we replace it below) but leaves status
            // "exited" — so the status check above can't see it. Re-read that flag and abort the restart,
            // otherwise the container the user just stopped would respawn.
            if g.live.get(cid).map_or(false, |l| {
                l.stop_requested.load(std::sync::atomic::Ordering::SeqCst)
            }) {
                return;
            }
            g.live.insert(cid.to_string(), live.clone());
            if let Some(cc) = g.containers.get_mut(cid) {
                cc.status = "running".into();
                cc.started_at = now_secs();
                cc.started_at_ns = now_nanos();
                cc.restart_count += 1;
            }
            save_state(&g, &app.state_path);
        }
        crate::events::emit_event(
            &app.events,
            "container",
            "restart",
            cid,
            serde_json::json!({"name": c.name}),
        );
        spawn_live(app, &c, &vols, live).await;
    })
}

/// Run ONE health probe: spawn the container's HEALTHCHECK test command as a fresh JIT process that JOINS
/// the container's loopback (so `curl localhost`/`pg_isready -h localhost` reach the container's server),
/// bounded by the probe timeout. Returns (exit_code, captured stdout+stderr). Docker's `Test` forms:
/// `["CMD", argv…]` execs directly, `["CMD-SHELL", script]` runs via `/bin/sh -c`, a bare list is a
/// legacy shell command. A timeout is recorded as exit -1 (matching docker).
async fn run_health_probe(
    app: &App,
    cont: &Container,
    vols: &[Vol],
    hcfg: &HealthConfig,
) -> (i64, String) {
    let mut test = hcfg.test.clone();
    let argv = match test.first().map(|s| s.as_str()) {
        Some("CMD-SHELL") => vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            test.get(1).cloned().unwrap_or_default(),
        ],
        Some("CMD") => test.split_off(1),
        Some("NONE") | None => return (0, String::new()),
        _ => test,
    };
    if argv.is_empty() {
        return (0, String::new());
    }
    let mut temp = cont.clone();
    temp.id = format!("health-{}", cont.id);
    temp.netns_key = Some(cont.id.clone()); // share the container's 127.0.0.1 so localhost probes work
    temp.cmd = argv;
    temp.tty = false;
    temp.healthcheck = None; // the probe process is not itself health-checked
    let Some((prog, args)) = spawn_cfg(&temp, &app.volumes_dir, vols, None) else {
        return (-1, String::new());
    };
    let mut cmd = tokio::process::Command::new(prog);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let timeout_ns = if hcfg.timeout > 0 {
        hcfg.timeout
    } else {
        30_000_000_000
    };
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return (-1, format!("probe spawn: {e}")),
    };
    match tokio::time::timeout(
        std::time::Duration::from_nanos(timeout_ns as u64),
        child.wait_with_output(),
    )
    .await
    {
        Ok(Ok(out)) => {
            let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
            s.push_str(&String::from_utf8_lossy(&out.stderr));
            (
                out.status.code().unwrap_or(-1) as i64,
                s.chars().take(4096).collect(),
            )
        }
        Ok(Err(e)) => (-1, format!("probe: {e}")),
        Err(_) => (-1, "Health check exceeded timeout".into()),
    }
}

/// The HEALTHCHECK monitor loop for one running container (§8.3-1). Probes every `interval` (default 30s),
/// maintaining inspect `State.Health`: exit 0 ⇒ healthy + streak reset; a non-zero probe increments the
/// failing streak and, once it reaches `retries` (default 3) AND the `start_period` grace has elapsed,
/// flips to unhealthy. Keeps the last 5 probe results in `Log[]`. Emits `health_status: …` events on a
/// transition. Exits when the container's process dies (this Live's `exit` fires) or it stops running.
async fn health_monitor(
    app: App,
    cid: String,
    cont: Container,
    vols: Vec<Vol>,
    hcfg: HealthConfig,
    mut exit_rx: watch::Receiver<Option<i64>>,
) {
    let interval = std::time::Duration::from_nanos(if hcfg.interval > 0 {
        hcfg.interval
    } else {
        30_000_000_000
    } as u64);
    let retries = if hcfg.retries > 0 { hcfg.retries } else { 3 };
    let start_ns = hcfg.start_period.max(0);
    let started = std::time::Instant::now();
    {
        let mut g = app.inner.lock().await;
        let Some(c) = g.containers.get_mut(&cid) else {
            return;
        };
        c.health = Some(HealthState {
            status: "starting".into(),
            failing_streak: 0,
            log: Vec::new(),
        });
        save_state(&g, &app.state_path);
    }
    crate::events::emit_event(
        &app.events,
        "container",
        "health_status: starting",
        &cid,
        serde_json::json!({}),
    );
    loop {
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = exit_rx.changed() => { if exit_rx.borrow().is_some() { break; } }
        }
        if {
            let g = app.inner.lock().await;
            g.containers
                .get(&cid)
                .map(|c| c.status != "running")
                .unwrap_or(true)
        } {
            break;
        }
        let start_ts = now_secs();
        let (code, output) = run_health_probe(&app, &cont, &vols, &hcfg).await;
        let end_ts = now_secs();
        let in_start_period = (started.elapsed().as_nanos() as i64) < start_ns;
        let mut g = app.inner.lock().await;
        let Some(c) = g.containers.get_mut(&cid) else {
            break;
        };
        if c.status != "running" {
            break;
        }
        let h = c.health.get_or_insert_with(|| HealthState {
            status: "starting".into(),
            failing_streak: 0,
            log: Vec::new(),
        });
        let prev = h.status.clone();
        h.log.push(HealthLog {
            start: fmt_rfc3339(start_ts),
            end: fmt_rfc3339(end_ts),
            exit_code: code,
            output,
        });
        let n = h.log.len();
        if n > 5 {
            h.log.drain(..n - 5);
        }
        if code == 0 {
            h.failing_streak = 0;
            h.status = "healthy".into();
        } else {
            h.failing_streak += 1;
            if !in_start_period && h.failing_streak >= retries {
                h.status = "unhealthy".into();
            }
        }
        let cur = h.status.clone();
        save_state(&g, &app.state_path);
        drop(g);
        if cur != prev {
            crate::events::emit_event(
                &app.events,
                "container",
                &format!("health_status: {cur}"),
                &cid,
                serde_json::json!({}),
            );
        }
    }
}

/// Record the failure on a Live and finalize the container as exit 127. Returns false (spawn failed).
pub(crate) async fn live_fail(app: &App, cid: &str, live: &Arc<Live>, msg: String) -> bool {
    let _ = live.out.send((2, format!("{msg}\n").into_bytes()));
    live.log_chunks
        .lock()
        .await
        .push((now_secs(), 2, format!("{msg}\n").into_bytes()));
    // No pumps run on the failure path, so the error line above is the last output -- signal both the
    // exit and output-complete so a hijack/`logs -f` consumer drains the message and ends cleanly.
    let _ = live.exit.send(Some(127));
    let _ = live.out_done.send(true);
    if let Some(cc) = app.inner.lock().await.containers.get_mut(cid) {
        cc.status = "exited".into();
        cc.exit_code = 127;
    }
    false
}
