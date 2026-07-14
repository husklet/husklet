//! Container run-state control: `start`, `stop`, `kill`, `restart`, and
//! `pause`/`unpause` (via `freeze`). Split out of the former `lifecycle.rs`;
//! behavior unchanged. `kill_group` lives here (the process-group signaller) and
//! is re-used by the delete path in `manage.rs`.
use super::super::*;

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
        // `docker start` on an already-running (or paused) container is a 304 Not Modified — a no-op
        // that must NOT re-spawn or reset started_at. Only a stopped container follows the start path.
        if c.status == "running" || c.status == "paused" {
            return StatusCode::NOT_MODIFIED.into_response();
        }
        let live = g
            .live
            .entry(full.clone())
            .or_insert_with(Live::new)
            .clone();
        // An explicit start clears the durable manual-stop flag (the container is deliberately up again).
        if let Some(cc) = g.containers.get_mut(&full) {
            cc.status = "running".into();
            cc.started_at = now_secs();
            cc.started_at_ns = now_nanos();
            cc.manually_stopped = false;
            // Install the initial `starting` health state SYNCHRONOUSLY as part of the start transition, so
            // an inspect immediately after start sees `State.Health.Status=starting`. The async health
            // monitor (spawned in spawn_live) otherwise leaves a timing gap where a poller sees running with
            // no health object yet, missing the initial health lifecycle.
            if cc.healthcheck.is_some() {
                cc.health = Some(crate::model::HealthState {
                    status: "starting".into(),
                    ..Default::default()
                });
            }
        }
        (c, g.volumes.clone(), live)
    };
    if std::env::var("HL_DEBUG").is_ok() {
        eprintln!("[start] {} cmd={:?}", &c.id[..12], c.cmd);
    }
    // Emit `start` BEFORE spawning: spawn_live launches the reaper on a concurrent task, and a very
    // short-lived container could otherwise fire `die` from that task before this handler emitted `start`,
    // giving event consumers an impossible die-before-start ordering. Emitting first guarantees the order.
    crate::events::emit_event(
        &a.events,
        "container",
        "start",
        &c.id,
        json!({"name": c.name, "image": c.image}),
    );
    spawn_live(&a, &c, &vols, live).await;
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
    resolve_get(&g, id)
        .map(|(_, c)| {
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
pub(super) fn kill_group(pid: i32, sig: i32) {
    unsafe {
        if libc::kill(-pid, sig) != 0 {
            libc::kill(pid, sig);
        }
    }
}

/// Whether a signal's DEFAULT disposition terminates the process. The handful that do NOT — child/urgent
/// notifications (SIGCHLD, SIGURG), job-control stop/continue (SIGSTOP, SIGTSTP, SIGTTIN, SIGTTOU,
/// SIGCONT), and window-resize (SIGWINCH) — leave a running container alive, so `docker kill` with one of
/// them must not fabricate an `exited` state. Everything else terminates (or core-dumps) by default.
pub(crate) fn signal_terminates_by_default(sig: i32) -> bool {
    !matches!(
        sig,
        libc::SIGCHLD
            | libc::SIGURG
            | libc::SIGSTOP
            | libc::SIGTSTP
            | libc::SIGTTIN
            | libc::SIGTTOU
            | libc::SIGCONT
            | libc::SIGWINCH
    )
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
    // `docker kill` on a non-running container is a 409 and mutates nothing (matches Moby: kill only
    // signals a live container; a stopped/exited one is rejected verbatim, no state change, no event).
    let running = g
        .containers
        .get(&full)
        .map(|c| c.status == "running" || c.status == "paused")
        .unwrap_or(false);
    if !running {
        return conflict(format!(
            "Cannot kill container: {id}: Container {id} is not running"
        ));
    }
    let sig = q
        .signal
        .as_deref()
        .map(|s| parse_signal(s, libc::SIGKILL))
        .unwrap_or(libc::SIGKILL);
    // A non-terminating signal (SIGWINCH/SIGCONT/SIGURG/SIGCHLD/…) does NOT kill the process, so the
    // container must stay running — only a signal whose default disposition is "terminate" transitions it
    // to exited. Without this, `docker kill --signal SIGWINCH` fabricated an `exited` state (and freed the
    // host ports) while the guest was still alive.
    let terminates = signal_terminates_by_default(sig);
    if let Some(l) = g.live.get(&full) {
        if terminates {
            l.stop_requested
                .store(true, std::sync::atomic::Ordering::SeqCst); // deliberate stop: no auto-restart
        }
        if let Some(pid) = *l.pid.lock().unwrap() {
            kill_group(pid as i32, sig);
        } // whole group, not just the leader
    }
    if !terminates {
        // Signal delivered, but state is unchanged (Docker still returns 204). The reaper will flip the
        // container to exited if the process actually dies later.
        return StatusCode::NO_CONTENT.into_response();
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
        let live = Live::new();
        g.live.insert(full.clone(), live.clone());
        if let Some(cc) = g.containers.get_mut(&full) {
            cc.status = "running".into();
            cc.started_at = now_secs();
            cc.started_at_ns = now_nanos();
            cc.manually_stopped = false;
        }
        (c, g.volumes.clone(), live)
    };
    if std::env::var("HL_DEBUG").is_ok() {
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

// ---- container control: pause / unpause -------------------------------------
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
/// Whether `docker pause`/`unpause` is allowed from the container's current status. Pause requires a
/// running container; unpause requires a paused one. Anything else (created/exited/restarting) is a 409
/// — otherwise pausing an exited container would fake a `paused` state with no live process behind it,
/// and a later unpause would mark it `running` again.
pub(crate) fn freeze_allowed(status: &str, pause: bool) -> bool {
    if pause {
        status == "running"
    } else {
        status == "paused"
    }
}

pub(crate) async fn freeze(a: App, id: String, pause: bool) -> Response {
    let mut g = a.inner.lock().await;
    let Some(full) = resolve_cid(&g, &id) else {
        return no_such(&id);
    };
    let status = g
        .containers
        .get(&full)
        .map(|c| c.status.clone())
        .unwrap_or_default();
    if !freeze_allowed(&status, pause) {
        return conflict(format!(
            "Container {id} is not {}",
            if pause { "running" } else { "paused" }
        ));
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    // "Ignored Kill Signals Fabricate Container Exit" (P1): only terminating signals may transition a
    // container to exited; non-terminating ones (SIGWINCH/SIGCONT/…) must leave it running.
    #[test]
    fn only_terminating_signals_end_a_container() {
        for sig in [libc::SIGKILL, libc::SIGTERM, libc::SIGINT, libc::SIGQUIT, libc::SIGHUP] {
            assert!(signal_terminates_by_default(sig), "sig {sig} should terminate");
        }
        for sig in [
            libc::SIGWINCH,
            libc::SIGCONT,
            libc::SIGCHLD,
            libc::SIGURG,
            libc::SIGSTOP,
            libc::SIGTSTP,
            libc::SIGTTIN,
            libc::SIGTTOU,
        ] {
            assert!(!signal_terminates_by_default(sig), "sig {sig} must NOT fabricate exit");
        }
    }

    // "Pause/Unpause Can Fake State On Non-Live Containers" (P1): pause needs a running container,
    // unpause needs a paused one; anything else is a 409, never a fake state flip.
    #[test]
    fn pause_only_from_running_unpause_only_from_paused() {
        assert!(freeze_allowed("running", true), "pause a running container");
        assert!(!freeze_allowed("exited", true), "pause an exited container must be rejected");
        assert!(!freeze_allowed("created", true), "pause a created container must be rejected");
        assert!(!freeze_allowed("paused", true), "double-pause must be rejected");

        assert!(freeze_allowed("paused", false), "unpause a paused container");
        assert!(!freeze_allowed("running", false), "unpause a running container must be rejected");
        assert!(!freeze_allowed("exited", false), "unpause an exited container must be rejected");
    }
}
