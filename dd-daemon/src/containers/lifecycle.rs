#![allow(unused_imports, dead_code)]
//! Container lifecycle / control handlers: create, start, stop, kill, restart,
//! pause/unpause, rename, wait, delete. Moved verbatim from the former
//! `containers.rs`; shared helpers (parse_bind, parse_signal, do_stop, q_truthy)
//! live in `mod.rs` and are pulled in via `use super::*`.
use super::*;

#[derive(Deserialize)]
pub(crate) struct CreateBody {
    #[serde(rename = "Image")]
    image: Option<String>,
    #[serde(rename = "Cmd")]
    cmd: Option<Vec<String>>,
    #[serde(rename = "Env")]
    env: Option<Vec<String>>,
    #[serde(rename = "Entrypoint")]
    entrypoint: Option<Vec<String>>,
    #[serde(rename = "Hostname")]
    hostname: Option<String>,
    #[serde(rename = "Tty")]
    tty: Option<bool>,
    #[serde(rename = "WorkingDir")]
    working_dir: Option<String>,
    #[serde(rename = "Labels")]
    labels: Option<HashMap<String, String>>,
    // `docker run --user U[:G]` — docker puts the "uid:gid" / "name" string in Config.User (top-level
    // of the create body, alongside Image/Cmd/Env). Stored on the Container and turned into DD_UID/DD_GID.
    #[serde(rename = "User")]
    user: Option<String>,
    #[serde(rename = "HostConfig")]
    host_config: Option<HostConfig>,
    // `docker create`/compose attach a container to one or more user-defined networks via the
    // top-level NetworkingConfig.EndpointsConfig (a map keyed by network name). HostConfig.NetworkMode
    // names the *primary* network; EndpointsConfig enumerates ALL of them (compose puts every
    // `networks:` entry of a service here). We join each so a multi-network service lands on them all.
    #[serde(rename = "NetworkingConfig")]
    networking_config: Option<NetworkingConfig>,
    // Config-level lifecycle fields (top of the create body, NOT under HostConfig): `--stop-signal`,
    // `--stop-timeout`, and `--health-*` (Healthcheck). Each overrides the image's; absent ⇒ inherit.
    #[serde(rename = "StopSignal")]
    stop_signal: Option<String>,
    #[serde(rename = "StopTimeout")]
    stop_timeout: Option<i64>,
    #[serde(rename = "Healthcheck")]
    healthcheck: Option<crate::model::HealthConfig>,
    // `Config.Volumes` — the docker CLI puts a bare `-v /path` (anonymous volume) HERE (a set of dirs),
    // the same channel as an image `VOLUME`. Each uncovered dir gets a fresh anonymous volume at run.
    #[serde(rename = "Volumes")]
    volumes: Option<HashMap<String, Value>>,
}

#[derive(Deserialize)]
pub(crate) struct NetworkingConfig {
    #[serde(rename = "EndpointsConfig")]
    endpoints_config: Option<HashMap<String, Value>>,
}

#[derive(Deserialize)]
pub(crate) struct HostConfig {
    #[serde(rename = "Binds")]
    binds: Option<Vec<String>>,
    #[serde(rename = "Memory")]
    memory: Option<i64>,
    #[serde(rename = "PidsLimit")]
    pids_limit: Option<i64>,
    // Resource fidelity: `--cpus` (NanoCpus), `--read-only` (ReadonlyRootfs), `--ulimit` (Ulimits).
    #[serde(rename = "NanoCpus")]
    nano_cpus: Option<i64>,
    #[serde(rename = "ReadonlyRootfs")]
    readonly_rootfs: Option<bool>,
    #[serde(rename = "Ulimits")]
    ulimits: Option<Vec<crate::model::Ulimit>>,
    #[serde(rename = "PortBindings")]
    port_bindings: Option<HashMap<String, Vec<PortBinding>>>,
    #[serde(rename = "NetworkMode")]
    network_mode: Option<String>,
    // HostConfig fidelity extras (parsed + persisted; round-tripped back through inspect).
    #[serde(rename = "RestartPolicy")]
    restart_policy: Option<RestartPolicy>,
    #[serde(rename = "CapAdd")]
    cap_add: Option<Vec<String>>,
    #[serde(rename = "CapDrop")]
    cap_drop: Option<Vec<String>>,
    #[serde(rename = "Devices")]
    devices: Option<Vec<DeviceMapping>>,
    #[serde(rename = "Mounts")]
    mounts: Option<Vec<Mount>>,
    // `--tmpfs DST[:opts]` (HostConfig.Tmpfs): a map of container-path -> mount options ("size=64m,mode=1777").
    // A fresh empty in-memory-equivalent mount. `--mount type=tmpfs` arrives via Mounts instead (folded in).
    #[serde(rename = "Tmpfs")]
    tmpfs: Option<HashMap<String, String>>,
    #[serde(rename = "Privileged")]
    privileged: Option<bool>,
    // `--security-opt` (Vec<String> like ["sandbox"], ["seccomp=untrusted"], ["no-new-privileges"]).
    // Parsed + persisted verbatim; an entry matching sandbox/untrusted opts into the JIT sentry (spawn_cfg).
    #[serde(rename = "SecurityOpt")]
    security_opt: Option<Vec<String>>,
    // `--rm` (HostConfig.AutoRemove): the daemon removes the container automatically once it exits.
    #[serde(rename = "AutoRemove")]
    auto_remove: Option<bool>,
}

#[derive(Deserialize, Clone)]
pub(crate) struct PortBinding {
    #[serde(rename = "HostPort")]
    host_port: Option<String>,
    // `docker -p 127.0.0.1:8080:80` sets HostIp so the publish is loopback-only (NOT world-reachable).
    // Empty/absent ⇒ 0.0.0.0. Previously dropped — the reason `127.0.0.1` publishes leaked to every iface.
    #[serde(rename = "HostIp")]
    host_ip: Option<String>,
}

/// Split a PortBindings key (`"<cport>/<proto>"`, e.g. `"9000/tcp"`) into (container-port, proto).
fn split_key(k: &str) -> (&str, &str) {
    k.split_once('/').unwrap_or((k, "tcp"))
}

pub(crate) fn publish_str(pb: &HashMap<String, Vec<PortBinding>>) -> String {
    let mut v = Vec::new();
    for (k, binds) in pb {
        let (cport, proto) = split_key(k);
        if cport.is_empty() {
            continue;
        }
        for b in binds {
            if let Some(hp) = &b.host_port {
                if !hp.is_empty() {
                    let ip = b.host_ip.as_deref().unwrap_or("");
                    v.push(format!("{ip}:{hp}:{cport}/{proto}"));
                }
            }
        }
    }
    v.join(",")
}

/// Like [`publish_str`] but AUTO-ASSIGNS a free host port for any binding with an empty `HostPort` —
/// docker's `-p <container>` / `-p 127.0.0.1::<container>` "publish to an ephemeral host port" form. The
/// daemon picks the port here (from the IANA dynamic range 49152-65535) so `docker port`/`ps`/inspect
/// report a concrete host port and the engine's `-p` host forwarder binds it. Ports already published by
/// existing containers are skipped to avoid intra-daemon collisions. Bindings with an explicit HostPort
/// are emitted verbatim (byte-identical to `publish_str`).
pub(crate) fn publish_str_alloc(pb: &HashMap<String, Vec<PortBinding>>, g: &Inner) -> String {
    let mut used: std::collections::HashSet<u16> = g
        .containers
        .values()
        .flat_map(|c| crate::containers::parse_publish(&c.publish))
        .map(|p| p.host_port)
        .collect();
    let mut next: u16 = 49152;
    let mut alloc = || -> u16 {
        while next < 65535 && used.contains(&next) {
            next += 1;
        }
        let p = next;
        used.insert(p);
        next = next.saturating_add(1);
        p
    };
    // Sort by container port so auto-assignment is deterministic (HashMap iteration order is not).
    let mut keys: Vec<&String> = pb.keys().collect();
    keys.sort();
    let mut v = Vec::new();
    for k in keys {
        let (cport, proto) = split_key(k);
        if cport.is_empty() {
            continue;
        }
        for b in &pb[k] {
            let hp = match &b.host_port {
                Some(h) if !h.is_empty() => h.clone(),
                _ => alloc().to_string(),
            };
            let ip = b.host_ip.as_deref().unwrap_or("");
            v.push(format!("{ip}:{hp}:{cport}/{proto}"));
        }
    }
    v.join(",")
}

/// Normalize a container mount target for dedup: strip a trailing slash (except root). `/data/` and
/// `/data` name the same mount point, so an image VOLUME at a `-v`-covered path isn't duplicated.
fn norm_dir(p: &str) -> String {
    let t = p.trim_end_matches('/');
    if t.is_empty() {
        "/".into()
    } else {
        t.to_string()
    }
}

/// Recursively copy the contents of `src` INTO `dst` (files, dirs, symlinks) — Moby's `populateVolumes`:
/// a freshly-created anonymous volume is seeded with the image's existing content at the mount point, so
/// a `VOLUME /var/lib/postgresql/data` over a populated image dir keeps those files instead of hiding
/// them behind an empty mount. Best-effort (never fails the create on an I/O error).
fn copy_dir_into(src: &std::path::Path, dst: &std::path::Path) {
    let Ok(rd) = std::fs::read_dir(src) else {
        return;
    };
    for e in rd.flatten() {
        let from = e.path();
        let to = dst.join(e.file_name());
        match e.file_type() {
            Ok(ft) if ft.is_dir() => {
                let _ = std::fs::create_dir_all(&to);
                copy_dir_into(&from, &to);
            }
            Ok(ft) if ft.is_symlink() => {
                if let Ok(t) = std::fs::read_link(&from) {
                    let _ = std::os::unix::fs::symlink(t, &to);
                }
            }
            _ => {
                let _ = std::fs::copy(&from, &to);
            }
        }
    }
}

/// Create an ANONYMOUS local volume (64-hex name, docker-style) backing container path `target`, seeding
/// it from the image's content at that path (populateVolumes). Returns the [`Vol`]; the caller registers
/// it + records the name in the container's `anon_volumes` (so `rm -v`/prune can reclaim it).
fn anon_volume(volumes_dir: &str, image_rootfs: &str, target: &str, cid: &str) -> Vol {
    let name = fake_id(&format!("anon:{cid}:{target}:{}", now_nanos()));
    let mountpoint = PathBuf::from(volumes_dir).join(&name);
    let _ = std::fs::create_dir_all(&mountpoint);
    let src = PathBuf::from(image_rootfs).join(target.trim_start_matches('/'));
    if src.is_dir() {
        copy_dir_into(&src, &mountpoint);
    }
    Vol {
        name,
        mountpoint: mountpoint.to_string_lossy().into_owned(),
        created_at: now_secs(),
        driver: "local".into(),
        options: HashMap::new(),
        labels: HashMap::new(),
    }
}

#[derive(Deserialize)]
pub(crate) struct CreateQ {
    name: Option<String>,
    platform: Option<String>,
}

pub(crate) async fn containers_create(
    State(a): State<App>,
    Query(cq): Query<CreateQ>,
    Json(body): Json<CreateBody>,
) -> Response {
    let image = body.image.unwrap_or_default();
    // Match the image by name and, when --platform is given, by arch. A platform mismatch returns 404 so
    // the docker CLI pulls the right arch (its default --pull=missing won't re-pull otherwise) and retries.
    let want_arch = platform_arch(cq.platform.as_deref());
    // On a miss, re-scan the images dir from disk before giving up: the image may be on disk (freshly
    // pulled/built) yet absent from the in-memory store, which would otherwise force a spurious re-pull.
    {
        let g = a.inner.lock().await;
        // Repository-aware presence: a bare `nginx` must NOT count a local `linuxserver/nginx`
        // as present (which would skip the rescan/pull and then run the wrong image). Compare the fully
        // qualified repository, not the bare basename.
        let present = g
            .images
            .iter()
            .filter(|i| ref_repo(&i.name) == ref_repo(&image))
            .any(|i| want_arch.map_or(true, |a| docker_arch(i.arch) == a));
        if !present {
            drop(g);
            rescan_images(&a).await;
        }
    }
    let mut g = a.inner.lock().await;
    // Restrict the store to the arch the user asked for (if any), then let `find_image` pick the single
    // best match deterministically (richest metadata wins; never an order-dependent duplicate).
    let candidates: Vec<Image> = g
        .images
        .iter()
        .filter(|i| want_arch.map_or(true, |a| docker_arch(i.arch) == a))
        .cloned()
        .collect();
    let img = match find_image(&candidates, &image).cloned() {
        Some(i) => i,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": format!("No such image: {image}")})),
            )
                .into_response()
        }
    };
    // Final argv = entrypoint ++ cmd (docker semantics). The entrypoint is the user's --entrypoint or the
    // IMAGE's ENTRYPOINT; a user --entrypoint resets CMD, but the image's own ENTRYPOINT still keeps the
    // image CMD. An empty Cmd falls back to the image default.
    let user_ep = body.entrypoint.is_some();
    let mut argv = body.entrypoint.unwrap_or_else(|| img.entrypoint.clone());
    let cmd = body.cmd.filter(|c| !c.is_empty()).unwrap_or_else(|| {
        if user_ep {
            vec![]
        } else {
            img.cmd.clone()
        }
    });
    argv.extend(cmd);
    if argv.is_empty() {
        argv = img.cmd.clone();
    }
    let cmd = argv;
    // env = image ENV then `docker run -e` (later wins); working dir = -w or the image WORKDIR.
    let mut env = img.env.clone();
    env.extend(body.env.unwrap_or_default());
    let working_dir = body
        .working_dir
        .filter(|w| !w.is_empty())
        .unwrap_or_else(|| img.workdir.clone());
    // Run user = `docker run --user` if given, else the image's default Config.User (dropped to DD_UID/DD_GID
    // in runtime.rs). Computed before `img` is partially moved into the Container below.
    let user = body
        .user
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| img.user.clone());
    let tty = body.tty.unwrap_or(false);
    // `docker run --name X` with a name already in use is a 409 Conflict (docker refuses to start a
    // second container under the same name). Match on the effective name (leading `/` stripped, as we
    // store it). An empty name (no --name) never conflicts.
    let want_name = cq
        .name
        .as_deref()
        .unwrap_or_default()
        .trim_start_matches('/')
        .to_string();
    if !want_name.is_empty() {
        if let Some(existing) = g.containers.values().find(|c| c.name == want_name) {
            return (StatusCode::CONFLICT, Json(json!({"message": format!(
                "Conflict. The container name \"/{want_name}\" is already in use by container \"{}\". \
                 You have to remove (or rename) that container to be able to reuse that name.", existing.id)}))).into_response();
        }
    }
    let id = new_id(&image);
    let hc = body.host_config;
    // Per-container copy-on-write upper layer over the read-only image rootfs (linux guests only; darwin
    // runs natively jailed and writes into its own rootfs). The guest's writes/creates/deletes land in
    // this private dir, so the shared image is never mutated. Reclaimed on `docker rm`/prune.
    let upper = if img.arch.os() == "darwin" {
        String::new()
    } else {
        let dir = dd_home().join("containers").join(&id).join("upper");
        // Probe up front that the upper is actually creatable AND writable, and FAIL LOUD (a stderr
        // diagnostic) if not — otherwise a non-writable upper degrades into silent per-write EPERM inside
        // the guest with no hint why. This is a bridge-topology footgun: the daemon runs on one host but the
        // JIT (which reads/writes this upper) runs mac-side, so the daemon's $HOME/.dd must land on a
        // filesystem the JIT host can write. We do NOT change behavior — on a real macOS host HOME is
        // correct and the probe passes silently; this only surfaces the misconfiguration.
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!(
                "dd-daemon: overlay upper not creatable at {}: {e} -- container writes will fail \
                (EPERM); ensure the daemon's $HOME/.dd is writable by the JIT host",
                dir.display()
            );
        } else {
            let probe = dir.join(".dd-write-probe");
            match std::fs::write(&probe, b"") {
                Ok(()) => {
                    let _ = std::fs::remove_file(&probe);
                }
                Err(e) => eprintln!(
                    "dd-daemon: overlay upper not writable at {}: {e} -- container writes \
                    will fail (EPERM); ensure the daemon's $HOME/.dd is writable by the JIT host",
                    dir.display()
                ),
            }
        }
        dir.to_string_lossy().into_owned()
    };
    // ---- Volumes/mounts: tmpfs, `--mount type=tmpfs`, bare `-v /path` + image `VOLUME` anon volumes ----
    // Moby wires every mount through one list; here we (1) fold tmpfs specs out of Binds/Mounts into the
    // `tmpfs` map, (2) turn a bare `-v /path` and each uncovered image `VOLUME` dir into an anonymous
    // volume seeded from the image (populateVolumes) so data dirs persist as a real volume rather than a
    // vanishing overlay upper. `covered` tracks targets an explicit mount already claims (no duplication).
    let img_rootfs = img.rootfs.clone();
    let mut binds = hc
        .as_ref()
        .and_then(|h| h.binds.clone())
        .unwrap_or_default();
    let mut mounts = hc
        .as_ref()
        .and_then(|h| h.mounts.clone())
        .unwrap_or_default();
    let mut tmpfs = hc
        .as_ref()
        .and_then(|h| h.tmpfs.clone())
        .unwrap_or_default();
    let mut anon_volumes: Vec<String> = Vec::new();
    // Fold `--mount type=tmpfs` (Type=tmpfs, empty Source) into the tmpfs map; drop them from `mounts`.
    mounts.retain(|m| {
        if m.typ == "tmpfs" {
            if !m.target.is_empty() {
                tmpfs.entry(m.target.clone()).or_default();
            }
            false
        } else {
            true
        }
    });
    // An ANONYMOUS volume mount (Type=volume with empty Source) — the shape the docker CLI often sends for
    // a bare `-v /path` and for `--mount type=volume,destination=/x` with no source. Materialize it into a
    // real anonymous volume seeded from the image (populateVolumes), so it persists/GCs like a named one.
    for m in mounts.iter_mut() {
        if m.typ == "volume" && m.source.is_empty() && !m.target.is_empty() {
            let v = anon_volume(&a.volumes_dir, &img_rootfs, &m.target, &id);
            let name = v.name.clone();
            g.volumes.push(v);
            crate::events::emit_event(
                &a.events,
                "volume",
                "create",
                &name,
                json!({"driver": "local"}),
            );
            anon_volumes.push(name.clone());
            m.source = name;
        }
    }
    let mut covered: std::collections::HashSet<String> = std::collections::HashSet::new();
    for m in &mounts {
        if !m.target.is_empty() {
            covered.insert(norm_dir(&m.target));
        }
    }
    for t in tmpfs.keys() {
        covered.insert(norm_dir(t));
    }
    // Bare `-v /path` (single field, absolute) ⇒ anonymous volume at /path; `name:/dst` and `/host:/dst`
    // (both contain ':') are real named/bind mounts, left verbatim (their dst is marked covered).
    let mut new_binds = Vec::with_capacity(binds.len());
    for b in binds.drain(..) {
        if !b.contains(':') && b.starts_with('/') {
            let v = anon_volume(&a.volumes_dir, &img_rootfs, &b, &id);
            let name = v.name.clone();
            g.volumes.push(v);
            crate::events::emit_event(
                &a.events,
                "volume",
                "create",
                &name,
                json!({"driver": "local"}),
            );
            covered.insert(norm_dir(&b));
            anon_volumes.push(name.clone());
            new_binds.push(format!("{name}:{b}"));
        } else {
            if let Some((_, dst, _)) = parse_bind(&b) {
                covered.insert(norm_dir(dst));
            }
            new_binds.push(b);
        }
    }
    let mut binds = new_binds;
    // Anonymous volumes from the image `VOLUME` set AND the create-body `Config.Volumes` (where the docker
    // CLI puts a bare `-v /path`): each uncovered dir gets a fresh anonymous volume seeded from the image.
    let mut anon_dirs: Vec<String> = img.img_volumes.clone();
    if let Some(cv) = body.volumes.as_ref() {
        for k in cv.keys() {
            anon_dirs.push(k.clone());
        }
    }
    for vdir in &anon_dirs {
        if vdir.is_empty() || covered.contains(&norm_dir(vdir)) {
            continue;
        }
        let v = anon_volume(&a.volumes_dir, &img_rootfs, vdir, &id);
        let name = v.name.clone();
        g.volumes.push(v);
        crate::events::emit_event(
            &a.events,
            "volume",
            "create",
            &name,
            json!({"driver": "local"}),
        );
        covered.insert(norm_dir(vdir));
        anon_volumes.push(name.clone());
        mounts.push(Mount {
            typ: "volume".into(),
            source: name,
            target: vdir.clone(),
            read_only: false,
        });
    }
    // Resolved stop signal / timeout / healthcheck: the create-body override, else the image's.
    let stop_signal = body
        .stop_signal
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| img.stop_signal.clone());
    let stop_timeout = body.stop_timeout.unwrap_or(0).max(0);
    let healthcheck = body.healthcheck.or_else(|| img.healthcheck.clone());
    let c = Container {
        id: id.clone(),
        image,
        rootfs: img.rootfs,
        upper,
        cmd,
        arch: Some(img.arch),
        binds: std::mem::take(&mut binds),
        // Effective hostname: Docker defaults an unset `--hostname` to the container's
        // 12-char short id. Resolve it HERE at create time so it is stored once and reported identically
        // everywhere (inspect Config.Hostname, the in-container `hostname`/uname, /etc/hostname) rather
        // than being defaulted differently — or left blank — on each path.
        hostname: body
            .hostname
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| id[..id.len().min(12)].to_string()),
        memory: hc.as_ref().and_then(|h| h.memory).unwrap_or(0),
        pids_limit: hc.as_ref().and_then(|h| h.pids_limit).unwrap_or(0),
        nano_cpus: hc.as_ref().and_then(|h| h.nano_cpus).unwrap_or(0),
        readonly_rootfs: hc.as_ref().and_then(|h| h.readonly_rootfs).unwrap_or(false),
        ulimits: hc
            .as_ref()
            .and_then(|h| h.ulimits.clone())
            .unwrap_or_default(),
        publish: hc
            .as_ref()
            .and_then(|h| h.port_bindings.as_ref())
            .map(|pb| publish_str_alloc(pb, &g))
            .unwrap_or_default(),
        created: now_secs(),
        tty,
        name: want_name,
        working_dir,
        env,
        user,
        labels: body.labels.unwrap_or_default(),
        network_mode: hc
            .as_ref()
            .and_then(|h| h.network_mode.clone())
            .unwrap_or_default(),
        // HostConfig fidelity extras: parse + persist verbatim (surfaced back in inspect HostConfig).
        // `--mount` entries (bind/volume) are additionally wired into the rootfs in spawn_cfg via the
        // same Volume mechanism as `-v`/Binds. CapAdd/CapDrop/Devices/Privileged are metadata (the JIT
        // doesn't enforce Linux capabilities/devices); RestartPolicy drives the spawn-time supervisor.
        restart_policy: hc
            .as_ref()
            .and_then(|h| h.restart_policy.clone())
            .unwrap_or_default(),
        cap_add: hc
            .as_ref()
            .and_then(|h| h.cap_add.clone())
            .unwrap_or_default(),
        cap_drop: hc
            .as_ref()
            .and_then(|h| h.cap_drop.clone())
            .unwrap_or_default(),
        devices: hc
            .as_ref()
            .and_then(|h| h.devices.clone())
            .unwrap_or_default(),
        mounts: std::mem::take(&mut mounts),
        privileged: hc.as_ref().and_then(|h| h.privileged).unwrap_or(false),
        security_opt: hc
            .as_ref()
            .and_then(|h| h.security_opt.clone())
            .unwrap_or_default(),
        auto_remove: hc.as_ref().and_then(|h| h.auto_remove).unwrap_or(false),
        // Lifecycle/volume fidelity (Moby §6/§8): resolved stop signal/timeout, tmpfs mounts, the anon
        // volumes this container owns (for `rm -v`/prune GC), and the resolved HEALTHCHECK.
        stop_signal,
        stop_timeout,
        tmpfs,
        anon_volumes,
        healthcheck,
        status: "created".into(),
        ..Default::default()
    };
    // Join the network now (fixes the bug where `docker run --network X` never added the container to
    // the network's membership/IPAM): pick the target network from --network, defaulting to `bridge`.
    let cname = endpoint_name(&c);
    let net_name = match c.network_mode.as_str() {
        "" | "default" | "bridge" => "bridge",
        "host" | "none" => "", // no L3 identity
        other => other,        // a user-defined network by name
    };
    if !net_name.is_empty() {
        join_network(&mut g.networks, net_name, &id, &cname);
    }
    // Additionally join every network named in NetworkingConfig.EndpointsConfig (compose lists all of a
    // service's networks here; NetworkMode only carries the primary). join_network is idempotent, so
    // re-joining the primary is a no-op, and unknown network names are skipped.
    if let Some(nc) = body
        .networking_config
        .as_ref()
        .and_then(|n| n.endpoints_config.as_ref())
    {
        for ep_name in nc.keys() {
            if !ep_name.is_empty() {
                join_network(&mut g.networks, ep_name, &id, &cname);
            }
        }
    }
    crate::events::emit_event(
        &a.events,
        "container",
        "create",
        &id,
        json!({"name": c.name, "image": c.image}),
    );
    g.containers.insert(id.clone(), c);
    save_state(&g, &a.state_path);
    (StatusCode::CREATED, Json(json!({"Id": id, "Warnings": []}))).into_response()
}

pub(crate) async fn containers_start(State(a): State<App>, Path(id): Path<String>) -> Response {
    let (c, vols, live) = {
        let mut g = a.inner.lock().await;
        let full = match resolve_cid(&g, &id) {
            Some(f) => f,
            None => return no_such(&id),
        };
        let c = match g.containers.get(&full).cloned() {
            Some(c) => c,
            None => return no_such(&id),
        };
        let live = g
            .live
            .entry(full.clone())
            .or_insert_with(|| Live::new(c.tty))
            .clone();
        // An explicit start clears the durable manual-stop flag (the container is deliberately up again).
        if let Some(cc) = g.containers.get_mut(&full) {
            cc.status = "running".into();
            cc.started_at = now_secs();
            cc.started_at_ns = now_nanos();
            cc.manually_stopped = false;
        }
        (c, g.volumes.clone(), live)
    };
    if std::env::var("DD_DEBUG").is_ok() {
        eprintln!("[start] {} cmd={:?}", &c.id[..12], c.cmd);
    }
    spawn_live(&a, &c, &vols, live).await;
    crate::events::emit_event(
        &a.events,
        "container",
        "start",
        &c.id,
        json!({"name": c.name, "image": c.image}),
    );
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub(crate) struct StopQ {
    t: Option<i64>,
    signal: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct KillQ {
    signal: Option<String>,
}

/// POST /containers/:id/stop?t=N&signal=SIG -- default signal SIGTERM, default t=10s.
pub(crate) async fn containers_stop(
    State(a): State<App>,
    Path(id): Path<String>,
    Query(q): Query<StopQ>,
) -> Response {
    let (def_sig, def_t) = resolve_stop_defaults(&a, &id).await;
    let sig = q
        .signal
        .as_deref()
        .map(|s| parse_signal(s, def_sig))
        .unwrap_or(def_sig);
    let t = q.t.unwrap_or(def_t).max(0);
    do_stop(&a, &id, sig, t).await
}

/// The `(signal, timeout)` a signal-less `docker stop`/`restart` uses for this container: its configured
/// StopSignal (image `Config.StopSignal` / `--stop-signal` — nginx SIGQUIT, postgres SIGINT) and
/// StopTimeout (`--stop-timeout`), each falling back to docker's defaults SIGTERM / 10s when unset. This
/// is the §8.3-3 repair: the stop path was hardcoded SIGTERM/10s and ignored both.
async fn resolve_stop_defaults(a: &App, id: &str) -> (i32, i64) {
    let g = a.inner.lock().await;
    resolve_cid(&g, id)
        .and_then(|f| g.containers.get(&f))
        .map(|c| {
            let s = if c.stop_signal.is_empty() {
                libc::SIGTERM
            } else {
                parse_signal(&c.stop_signal, libc::SIGTERM)
            };
            let t = if c.stop_timeout > 0 {
                c.stop_timeout
            } else {
                10
            };
            (s, t)
        })
        .unwrap_or((libc::SIGTERM, 10))
}

/// Signal a container's whole process group. The JIT leader is its own group leader (setpgid at spawn
/// in runtime.rs), so the host processes the guest forks inherit that pgid; `kill(-pgid, sig)` (killpg,
/// pgid == leader pid) reaches the leader AND every forked child, so a multi-process container dies
/// completely instead of leaving orphans. Only if the group signal fails (e.g. the leader is mid-
/// teardown) do we fall back to the leader pid alone. Mirrors freeze()'s group-signal pattern.
fn kill_group(pid: i32, sig: i32) {
    unsafe {
        if libc::kill(-pid, sig) != 0 {
            libc::kill(pid, sig);
        }
    }
}

/// POST /containers/:id/kill?signal=SIG -- default signal SIGKILL, delivered immediately.
pub(crate) async fn containers_kill(
    State(a): State<App>,
    Path(id): Path<String>,
    Query(q): Query<KillQ>,
) -> Response {
    let mut g = a.inner.lock().await;
    let Some(full) = resolve_cid(&g, &id) else {
        return no_such(&id);
    };
    let sig = q
        .signal
        .as_deref()
        .map(|s| parse_signal(s, libc::SIGKILL))
        .unwrap_or(libc::SIGKILL);
    if let Some(l) = g.live.get(&full) {
        l.stop_requested
            .store(true, std::sync::atomic::Ordering::SeqCst); // deliberate stop: no auto-restart
        if let Some(pid) = *l.pid.lock().unwrap() {
            kill_group(pid as i32, sig);
        } // whole group, not just the leader
    }
    crate::containers::ports::stop(&full); // free published host ports (docker kill releases the binding)
    if let Some(c) = g.containers.get_mut(&full) {
        c.status = "exited".into();
        c.finished_at = now_secs();
        c.finished_at_ns = now_nanos();
        c.manually_stopped = true;
    }
    let (cname, cimage) = g
        .containers
        .get(&full)
        .map(|c| (c.name.clone(), c.image.clone()))
        .unwrap_or_default();
    crate::events::emit_event(
        &a.events,
        "container",
        "kill",
        &full,
        json!({"name": cname, "image": cimage}),
    );
    save_state(&g, &a.state_path);
    StatusCode::NO_CONTENT.into_response()
}

/// restart: stop the live process (real signal, via the stop path) then spawn a FRESH `Live` so the
/// guest truly re-runs. We can't reuse `containers_start` here: its `g.live.entry(..).or_insert_with`
/// would return the OLD, spent `Live` (whose `started` flag is already set), and `spawn_live` no-ops on
/// an already-started `Live` — so the container would never actually re-spawn. `do_stop` set
/// `stop_requested` on that old `Live`, so when its process dies the RestartPolicy supervisor skips it
/// (a deliberate `docker restart` must not be double-counted as a crash); this handler owns the respawn.
/// The new `Live` starts with `stop_requested=false`, so a *future* crash still follows `--restart`.
pub(crate) async fn containers_restart(
    State(a): State<App>,
    Path(id): Path<String>,
    Query(q): Query<StopQ>,
) -> Response {
    let (def_sig, def_t) = resolve_stop_defaults(&a, &id).await;
    let sig = q
        .signal
        .as_deref()
        .map(|s| parse_signal(s, def_sig))
        .unwrap_or(def_sig);
    let t = q.t.unwrap_or(def_t).max(0);
    // Stop the running process (if any). `do_stop` blocks until the old reaper flips status to "exited"
    // (or the container had no live process), so its state writes are done before we install the new Live.
    let _ = do_stop(&a, &id, sig, t).await;
    let (c, vols, live) = {
        let mut g = a.inner.lock().await;
        let full = match resolve_cid(&g, &id) {
            Some(f) => f,
            None => return no_such(&id),
        };
        let c = match g.containers.get(&full).cloned() {
            Some(c) => c,
            None => return no_such(&id),
        };
        // Replace the spent Live with a fresh one (mirrors maybe_restart / start's spawn).
        let live = Live::new(c.tty);
        g.live.insert(full.clone(), live.clone());
        if let Some(cc) = g.containers.get_mut(&full) {
            cc.status = "running".into();
            cc.started_at = now_secs();
            cc.started_at_ns = now_nanos();
            cc.manually_stopped = false;
        }
        (c, g.volumes.clone(), live)
    };
    if std::env::var("DD_DEBUG").is_ok() {
        eprintln!("[restart] {} cmd={:?}", &c.id[..12], c.cmd);
    }
    spawn_live(&a, &c, &vols, live).await;
    crate::events::emit_event(
        &a.events,
        "container",
        "start",
        &c.id,
        json!({"name": c.name, "image": c.image}),
    );
    crate::events::emit_event(
        &a.events,
        "container",
        "restart",
        &c.id,
        json!({"name": c.name}),
    );
    StatusCode::NO_CONTENT.into_response()
}

// ---- container control: pause / unpause / rename ----------------------------
/// POST /containers/:id/(un)pause -- dd has no freezer cgroup, so it SIGSTOP/SIGCONTs the container's
/// whole process group (see `freeze`) and flips the recorded status.
pub(crate) async fn containers_pause(State(a): State<App>, Path(id): Path<String>) -> Response {
    freeze(a, id, true).await
}

pub(crate) async fn containers_unpause(State(a): State<App>, Path(id): Path<String>) -> Response {
    freeze(a, id, false).await
}

/// docker pause/unpause. macOS has no freezer cgroup, but the container runs in its own process group
/// (the JIT is the group leader; host processes the guest forks inherit that pgid -- see spawn_live), so
/// a single SIGSTOP/SIGCONT to the GROUP freezes/resumes the WHOLE container -- the main process AND any
/// forked children -- not just the leader. We signal the group via killpg (`kill(-pgid)`) and, only if
/// that fails (e.g. the leader is mid-teardown), fall back to the leader pid alone.
pub(crate) async fn freeze(a: App, id: String, pause: bool) -> Response {
    let mut g = a.inner.lock().await;
    let Some(full) = resolve_cid(&g, &id) else {
        return no_such(&id);
    };
    if let Some(pid) = g.live.get(&full).and_then(|l| *l.pid.lock().unwrap()) {
        let pid = pid as i32;
        let sig = if pause { libc::SIGSTOP } else { libc::SIGCONT };
        // pid is the group leader, so -pid is the container's process group id (pgid == leader pid).
        kill_group(pid, sig);
    }
    if let Some(c) = g.containers.get_mut(&full) {
        c.status = if pause {
            "paused".into()
        } else {
            "running".into()
        };
    }
    save_state(&g, &a.state_path);
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub(crate) struct RenameQ {
    name: Option<String>,
}

pub(crate) async fn containers_rename(
    State(a): State<App>,
    Path(id): Path<String>,
    Query(q): Query<RenameQ>,
) -> Response {
    let mut g = a.inner.lock().await;
    let Some(full) = resolve_cid(&g, &id) else {
        return no_such(&id);
    };
    if let Some(name) = q.name {
        if let Some(c) = g.containers.get_mut(&full) {
            c.name = name.trim_start_matches('/').to_string();
        }
    }
    save_state(&g, &a.state_path);
    StatusCode::NO_CONTENT.into_response()
}

/// POST /containers/:id/wait -- block until the container exits, then return {"StatusCode": n}. CRITICAL:
/// the docker `run` CLI sends this BEFORE /start and reads it concurrently, so we must flush the response
/// HEADERS immediately (200) and stream the JSON body only once the guest exits -- otherwise the CLI
/// blocks waiting for the response and never sends /start (a deadlock).
pub(crate) async fn containers_wait(State(a): State<App>, Path(id): Path<String>) -> Response {
    let (full, live, done_code) = {
        let g = a.inner.lock().await;
        let Some(full) = resolve_cid(&g, &id) else {
            return no_such(&id);
        };
        let live = g.live.get(&full).cloned();
        let done = g
            .containers
            .get(&full)
            .filter(|c| c.status == "exited")
            .map(|c| c.exit_code);
        (full.clone(), live, done)
    };
    let stream = futures_util::stream::once(async move {
        let code = if let Some(c) = done_code {
            c
        } else if let Some(live) = live {
            let mut rx = live.exit_rx.clone();
            loop {
                let cur = *rx.borrow();
                if let Some(c) = cur {
                    break c;
                }
                if rx.changed().await.is_err() {
                    break 0;
                }
            }
        } else {
            0
        };
        let _ = full;
        Ok::<_, std::io::Error>(format!("{{\"StatusCode\":{code}}}\n").into_bytes())
    });
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from_stream(stream))
        .unwrap()
}

#[derive(Deserialize)]
pub(crate) struct DeleteQ {
    force: Option<String>,
    v: Option<String>,
    link: Option<String>,
}

pub(crate) async fn containers_delete(
    State(a): State<App>,
    Path(id): Path<String>,
    Query(q): Query<DeleteQ>,
) -> Response {
    let force = q_truthy(&q.force);
    let mut g = a.inner.lock().await;
    let full = match resolve_cid(&g, &id) {
        Some(f) => f,
        None => return no_such(&id),
    };
    // `docker rm` of a running container without `-f` is a 409: docker refuses to remove a live
    // container and tells the user to stop it (or use `--force`). With `--force` we stop it first.
    let running = g
        .containers
        .get(&full)
        .map(|c| c.status == "running" || c.status == "paused")
        .unwrap_or(false);
    if running && !force {
        let short = &full[..12.min(full.len())];
        return (StatusCode::CONFLICT, Json(json!({"message": format!(
            "cannot remove a running container {short}: Stop the container before removing or force remove")}))).into_response();
    }
    // Removing a container cancels any pending RestartPolicy restart; with `--force` on a running
    // container we also SIGKILL the live process so the reaper doesn't resurrect/dangle it.
    if let Some(l) = g.live.get(&full) {
        l.stop_requested
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if force && running {
            if let Some(pid) = *l.pid.lock().unwrap() {
                kill_group(pid as i32, libc::SIGKILL);
            }
        } // whole group, not just the leader
    }
    crate::containers::ports::stop(&full); // free any published host ports before the container is gone
    let rm_vols = q_truthy(&q.v);
    if let Some(dc) = g.containers.remove(&full) {
        crate::events::emit_event(
            &a.events,
            "container",
            "destroy",
            &full,
            json!({"name": dc.name, "image": dc.image}),
        );
        // `docker rm -v`: reclaim this container's ANONYMOUS volumes (bare `-v /path` + image `VOLUME`
        // dirs) — Moby removes only anonymous volumes on rm, never named ones (mounts.go:removeMountPoints).
        if rm_vols {
            for name in &dc.anon_volumes {
                if let Some(v) = g.volumes.iter().find(|v| &v.name == name) {
                    let _ = std::fs::remove_dir_all(&v.mountpoint);
                }
                g.volumes.retain(|v| &v.name != name);
                crate::events::emit_event(
                    &a.events,
                    "volume",
                    "destroy",
                    name,
                    json!({"driver": "local"}),
                );
            }
        }
        // Reclaim any tmpfs scratch dirs this container owns (never persisted; always safe to drop).
        let _ = std::fs::remove_dir_all(dd_home().join("containers").join(&full).join("tmpfs"));
        // Drop the container from any network membership too.
        for n in g.networks.iter_mut() {
            leave_network(n, &full);
        }
        // Reclaim the container's private writable upper layer (Docker discards the writable layer on rm).
        // The shared image rootfs (the read-only lower) is never touched. Also drop its live IO plumbing
        // (log buffers + channels); otherwise `docker rm` leaks them.
        discard_container_layer(&dc.upper);
        g.live.remove(&full);
        save_state(&g, &a.state_path);
        StatusCode::NO_CONTENT.into_response()
    } else {
        no_such(&id)
    }
}
