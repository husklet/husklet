//! `POST /containers/create` — the container CREATE handler. The stateless pieces
//! it uses were split into siblings (behavior unchanged, pure file reshaping):
//!   - `dto`     — the create-body/host-config deserialize DTOs.
//!   - `ports`   — published-port string assembly (`publish_str`/`publish_str_alloc`).
//!   - `volumes` — anonymous-volume seeding (populateVolumes: `anon_volume` +
//!     `copy_dir_into` + `norm_dir`).
//! Each is re-exported so `crate::containers::<name>` resolves exactly as before.
use super::super::*;

mod dto;
mod ports;
mod volumes;

pub(crate) use {dto::*, ports::*};
use volumes::*;

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
        None => return no_such_image(&image),
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
            return conflict(format!(
                "Conflict. The container name \"/{want_name}\" is already in use by container \"{}\". \
                 You have to remove (or rename) that container to be able to reuse that name.",
                existing.id
            ));
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
    (
        StatusCode::CREATED,
        Json(crate::api::CreateResponse {
            id,
            warnings: vec![],
        }),
    )
        .into_response()
}
