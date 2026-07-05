//! Container lifecycle HTTP handlers, split into cohesive submodules:
//!   - `lifecycle` — create/start/stop/kill/restart/pause/unpause/rename/wait/delete
//!   - `exec`      — attach + the `/exec` create/start/inspect flow + PTY resize
//!   - `inspect`   — top/stats/logs/inspect/list (`ps`) + prune/changes/update/export
//!
//! This module keeps the shared request structs/helpers (parse_bind, parse_signal,
//! do_stop, q_truthy, ports_json/ports_map_json) and re-exports every handler with
//! `pub(crate) use`, so the public path `crate::containers::<handler>` (used by the
//! router in main.rs and every `use crate::containers::*` site) is unchanged.
use crate::images::*;
use crate::model::*;
use crate::networks::*;
use crate::runtime::*;
use crate::util::*;
use crate::prelude::*;

mod exec;
mod inspect;
mod lifecycle;
mod parse;
pub(crate) mod ports;
pub(crate) use exec::*;
pub(crate) use inspect::*;
pub(crate) use lifecycle::*;
pub(crate) use parse::*;

/// Signal a container's whole process group. The JIT leader is its own group leader (setpgid at spawn
/// in runtime.rs), so the host processes the guest forks inherit that pgid; `kill(-pgid, sig)` (killpg,
/// pgid == leader pid) reaches the leader AND every forked child, so a multi-process container dies
/// completely instead of leaving orphans. Only if the group signal fails (e.g. the leader is mid-
/// teardown) do we fall back to the leader pid alone. Mirrors lifecycle.rs's `kill_group`.
fn kill_group(pid: i32, sig: i32) {
    unsafe {
        if libc::kill(-pid, sig) != 0 {
            libc::kill(pid, sig);
        }
    }
}

/// stop: deliver a REAL signal to the live JIT process (same mechanism as pause's
/// `libc::kill(pid, SIGSTOP)`), wait up to `t` seconds for the guest to exit on its own, then
/// SIGKILL if it's still alive. Containers with no live process keep the old mark-exited behavior.
/// The wait polls the reaper-maintained container status without holding the inner lock across the
/// `tokio::time::sleep`, and is bounded by `t` so the handler never hangs indefinitely.
async fn do_stop(a: &App, id: &str, sig: i32, t: i64) -> Response {
    // resolve + grab the live pid, then release the lock before any waiting.
    let (full, pid) = {
        let g = a.inner.lock().await;
        let Some(full) = resolve_cid(&g, id) else {
            return no_such(id);
        };
        // `docker stop` on an already-stopped container is a 304 Not Modified — a no-op that must NOT
        // rewrite finished_at or emit a `stop` event. Only a running/paused/restarting container is
        // signalled. (containers_restart ignores this return value and spawns a fresh Live regardless,
        // so a stopped container still restarts — the 304 only short-circuits the stop half.)
        let active = g
            .containers
            .get(&full)
            .map(|c| c.status == "running" || c.status == "paused" || c.status == "restarting")
            .unwrap_or(false);
        if !active {
            return StatusCode::NOT_MODIFIED.into_response();
        }
        // Mark a deliberate stop so the RestartPolicy supervisor won't auto-restart this container.
        let pid = g
            .live
            .get(&full)
            .map(|l| {
                l.stop_requested
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                *l.pid.lock().unwrap()
            })
            .flatten();
        (full, pid)
    };
    if let Some(pid) = pid {
        kill_group(pid as i32, sig); // whole process group, not just the leader
                                     // give the guest up to `t` seconds to exit on its own; the spawn reaper (runtime.rs) flips
                                     // status to "exited" when the process dies, so poll that rather than racing on pid reuse.
        let mut waited = 0i64;
        loop {
            let exited = {
                let g = a.inner.lock().await;
                g.containers
                    .get(&full)
                    .map(|c| c.status == "exited")
                    .unwrap_or(true)
            };
            if exited {
                break;
            }
            if waited >= t * 1000 {
                kill_group(pid as i32, libc::SIGKILL);
                break;
            } // group SIGKILL, not just the leader
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            waited += 100;
        }
    }
    // Release the published-port host listeners — `docker stop` frees the binding (a later container may
    // re-publish the same host port). Idempotent + no-op when nothing was published.
    ports::stop(&full);
    // mark exited (as before); the reaper sets the real exit_code when the signalled process dies.
    // `manually_stopped` is the DURABLE (persisted) equivalent of Moby's HasBeenManuallyStopped: it
    // survives a daemon restart so `unless-stopped` won't resurrect a container the user stopped (the
    // in-memory `Live.stop_requested` set above is lost across a restart; this is not). §8.3-5.
    let mut g = a.inner.lock().await;
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
        "stop",
        &full,
        json!({"name": cname, "image": cimage}),
    );
    save_state(&g, &a.state_path);
    StatusCode::NO_CONTENT.into_response()
}
