//! `POST /containers/create` — the container CREATE handler. The stateless pieces
//! it uses were split into siblings (behavior unchanged, pure file reshaping):
//!   - `dto`     — the create-body/host-config deserialize DTOs.
//!   - `ports`   — published-port string assembly (`publish_str`/`publish_str_alloc`).
//!   - `volumes` — anonymous-volume seeding (populateVolumes: `anon_volume` +
//!     `copy_dir_into` + `norm_dir`).
//! Each is re-exported so `crate::containers::<name>` resolves exactly as before.
use super::super::*;

mod argv;
mod dto;
mod ports;
mod volumes;

pub(crate) use {dto::*, ports::*};
use argv::*;
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
    // Reject a create whose selected image rootfs has DISAPPEARED from the store: recording a container
    // that points at a gone rootfs (then returning 201 + emitting container/create) is worse than a clean
    // 404. Scoped to STORE-managed rootfs paths (under images_dir) so bundled/host images — and test
    // fixtures that seed images with synthetic rootfs paths — are unaffected.
    if !img.rootfs.is_empty()
        && std::path::Path::new(&img.rootfs).starts_with(&a.images_dir)
        && !std::path::Path::new(&img.rootfs).exists()
    {
        return no_such_image(&image);
    }
    // Final argv = entrypoint ++ cmd (docker semantics). The entrypoint is the user's --entrypoint or the
    // IMAGE's ENTRYPOINT; a user --entrypoint resets CMD, but the image's own ENTRYPOINT still keeps the
    // image CMD. An empty Cmd falls back to the image default.
    let cmd = resolve_argv(
        body.entrypoint.clone(),
        body.cmd.clone(),
        &img.entrypoint,
        &img.cmd,
    );
    // Docker inspect reports Config.Entrypoint / Config.Cmd SPLIT — keep the resolved parts (not the merged
    // launch argv) for inspect/commit fidelity. Mirrors resolve_argv: a user --entrypoint resets the cmd
    // part unless the user also gave a cmd; otherwise the image's entrypoint keeps the image cmd.
    let entrypoint_cfg = body
        .entrypoint
        .clone()
        .unwrap_or_else(|| img.entrypoint.clone());
    let cmd_config = body
        .cmd
        .clone()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| {
            if body.entrypoint.is_some() {
                Vec::new()
            } else {
                img.cmd.clone()
            }
        });
    // env = image ENV then `docker run -e` (later wins); working dir = -w or the image WORKDIR.
    // Dedup last-wins so inspect/state don't expose a stale image value that the runtime already overrides
    // (the guest launch env dedups the same way): `-e FOO=run` over image `FOO=image` yields one `FOO=run`.
    let env = dedup_env_last_wins(img.env.iter().cloned().chain(body.env.unwrap_or_default()));
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
    // Atomic network validation: a create attached to a MISSING user network must fail 404 BEFORE any
    // state mutation or event — previously the join silently no-oped and the daemon recorded a partial
    // container (plus anon volumes + events) and returned 201.
    {
        let nm = hc
            .as_ref()
            .and_then(|h| h.network_mode.clone())
            .unwrap_or_default();
        let mut wanted: Vec<String> = match nm.as_str() {
            "" | "default" | "bridge" | "host" | "none" => Vec::new(),
            other => vec![other.to_string()],
        };
        if let Some(nc) = body
            .networking_config
            .as_ref()
            .and_then(|n| n.endpoints_config.as_ref())
        {
            wanted.extend(nc.keys().filter(|k| !k.is_empty()).cloned());
        }
        for w in &wanted {
            if !g.networks.iter().any(|n| n.name == *w) {
                return no_such_network(w);
            }
        }
    }
    // Per-container copy-on-write upper layer over the read-only image rootfs (linux guests only; darwin
    // runs natively jailed and writes into its own rootfs). The guest's writes/creates/deletes land in
    // this private dir, so the shared image is never mutated. Reclaimed on `docker rm`/prune.
    let upper = if img.arch.os() == "darwin" {
        String::new()
    } else {
        let dir = hl_home().join("containers").join(&id).join("upper");
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
    // If this container will materialize ANY anonymous volume, the volumes root must be a usable directory
    // — otherwise `create_dir_all(<volumes_dir>/<name>)` silently no-ops and we'd record volumes whose
    // mountpoints don't exist (and a container that can't mount them). Fail the create up front instead.
    let needs_anon = mounts
        .iter()
        .any(|m| m.typ == "volume" && m.source.is_empty() && !m.target.is_empty())
        || binds.iter().any(|b| !b.contains(':') && b.starts_with('/'))
        || !img.img_volumes.is_empty()
        || body.volumes.as_ref().map_or(false, |v| !v.is_empty());
    if needs_anon
        && (std::fs::create_dir_all(&a.volumes_dir).is_err()
            || !std::path::Path::new(&a.volumes_dir).is_dir())
    {
        return server_error(format!(
            "cannot create anonymous volumes: volumes root {} is not a usable directory",
            a.volumes_dir
        ));
    }
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
            bind_options: None,
        });
    }
    // Resolved stop signal / timeout / healthcheck: the create-body override, else the image's.
    let stop_signal = body
        .stop_signal
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| img.stop_signal.clone());
    let stop_timeout = body.stop_timeout.unwrap_or(0).max(0);
    // Resolve the healthcheck (create-body override else the image's), but DISABLE it when the effective
    // Test is `["NONE"]` or empty — docker's `--no-healthcheck` / `Healthcheck.Test=["NONE"]` turns the
    // probe OFF. Storing it as `Some` made spawn start a monitor that reported fake "healthy" state.
    let healthcheck = body
        .healthcheck
        .or_else(|| img.healthcheck.clone())
        .filter(|h| !matches!(h.test.first().map(String::as_str), Some("NONE") | None));
    // Names of anon volumes materialized above — kept for rollback if the durable save fails below.
    let anon_names = anon_volumes.clone();
    let c = Container {
        id: id.clone(),
        image,
        rootfs: img.rootfs,
        upper,
        cmd,
        entrypoint: entrypoint_cfg,
        cmd_config,
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
        // Interactive stdio flags — persisted for inspect fidelity (attach/exec reconstruction).
        open_stdin: body.open_stdin.unwrap_or(false),
        stdin_once: body.stdin_once.unwrap_or(false),
        name: want_name,
        working_dir,
        env,
        user,
        // Container labels INHERIT the image's, with create-body labels overriding same-key entries
        // (docker semantics). Dropping the inherited set broke label selectors on run-from-image.
        labels: {
            let mut l = img.labels.clone();
            l.extend(body.labels.unwrap_or_default());
            l
        },
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
        // `Config.Domainname` (UTS domain) — metadata + inspect fidelity.
        domainname: body.domainname.unwrap_or_default(),
        // `Config.ExposedPorts` — image EXPOSE ports plus create-body ExposedPorts keys (deduped, sorted).
        exposed_ports: {
            let mut ps: std::collections::BTreeSet<String> =
                img.exposed_ports.iter().cloned().collect();
            if let Some(ep) = body.exposed_ports {
                ps.extend(ep.into_keys());
            }
            ps.into_iter().collect()
        },
        // Logging / DNS / device-request fidelity — accepted, persisted, round-tripped through inspect.
        log_config: hc.as_ref().and_then(|h| h.log_config.clone()),
        dns: hc.as_ref().and_then(|h| h.dns.clone()).unwrap_or_default(),
        dns_search: hc
            .as_ref()
            .and_then(|h| h.dns_search.clone())
            .unwrap_or_default(),
        dns_options: hc
            .as_ref()
            .and_then(|h| h.dns_options.clone())
            .unwrap_or_default(),
        extra_hosts: hc
            .as_ref()
            .and_then(|h| h.extra_hosts.clone())
            .unwrap_or_default(),
        device_requests: hc
            .as_ref()
            .and_then(|h| h.device_requests.clone())
            .unwrap_or_default(),
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
    // Parse a network's requested static IP (EndpointsConfig[name].IPAMConfig.IPv4Address) and DNS
    // aliases (.Aliases) so a `docker network connect --ip`/compose `ipv4_address`/`aliases` is honored.
    let ep_settings = |name: &str| -> (Option<String>, Vec<String>) {
        body.networking_config
            .as_ref()
            .and_then(|n| n.endpoints_config.as_ref())
            .and_then(|m| m.get(name))
            .map(|v| {
                let ip = v
                    .get("IPAMConfig")
                    .and_then(|i| i.get("IPv4Address"))
                    .and_then(|s| s.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string());
                let aliases = v
                    .get("Aliases")
                    .and_then(|a| a.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                (ip, aliases)
            })
            .unwrap_or((None, Vec::new()))
    };
    if !net_name.is_empty() {
        let (req_ip, aliases) = ep_settings(net_name);
        join_network_ex(&mut g.networks, net_name, &id, &cname, req_ip.as_deref(), &aliases);
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
                let (req_ip, aliases) = ep_settings(ep_name);
                join_network_ex(
                    &mut g.networks,
                    ep_name,
                    &id,
                    &cname,
                    req_ip.as_deref(),
                    &aliases,
                );
            }
        }
    }
    // Build the create-event attributes now (flatten labels so `--filter label=...` selects it), but do
    // NOT emit yet: the event must represent DURABLE state. Insert, persist (checked), then emit.
    let mut cre_attrs = serde_json::Map::new();
    cre_attrs.insert("name".into(), json!(c.name));
    cre_attrs.insert("image".into(), json!(c.image));
    for (k, v) in &c.labels {
        cre_attrs.insert(k.clone(), json!(v));
    }
    g.containers.insert(id.clone(), c);
    // Persist BEFORE emitting the create event / returning 201. If the state save fails, roll back the
    // whole partial create (container, anon volumes, network endpoints) and fail — a `201` + create event
    // must never describe state that vanishes on restart.
    if let Err(e) = save_state_checked(&g, &a.state_path) {
        g.containers.remove(&id);
        for name in &anon_names {
            g.volumes.retain(|v| &v.name != name);
        }
        for n in g.networks.iter_mut() {
            leave_network(n, &id);
        }
        return server_error(format!("failed to persist container state: {e}"));
    }
    crate::events::emit_event(&a.events, "container", "create", &id, Value::Object(cre_attrs));
    (
        StatusCode::CREATED,
        Json(crate::api::CreateResponse {
            id,
            warnings: vec![],
        }),
    )
        .into_response()
}
