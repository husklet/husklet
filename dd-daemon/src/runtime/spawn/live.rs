use super::*;
use super::super::health::health_monitor;
use super::super::restart::maybe_restart;
use super::net::write_net_names;

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
    let mut launched = launched;
    let io_handles = std::mem::take(&mut launched.io_handles);

    *live.pid.lock().unwrap() = Some(launched.pid); // remember the pid so pause can SIGSTOP/SIGCONT it
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
        let code = launched.wait().await;
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
                // `--rm` acts as `rm -v` for ANONYMOUS volumes (bare `-v /path` + image `VOLUME` dirs) —
                // Moby removes them with the container on exit; named volumes are kept. Mirrors the
                // explicit `rm -v` path in containers/lifecycle/manage.rs.
                for name in &dc.anon_volumes {
                    if let Some(v) = g.volumes.iter().find(|v| &v.name == name) {
                        let _ = std::fs::remove_dir_all(&v.mountpoint);
                    }
                    g.volumes.retain(|v| &v.name != name);
                    crate::events::emit_event(
                        &app.events,
                        "volume",
                        "destroy",
                        name,
                        serde_json::json!({"driver": "local"}),
                    );
                }
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
