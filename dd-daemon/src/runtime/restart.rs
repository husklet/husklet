#![allow(unused_imports, dead_code)]
use super::*;
use super::spawn::spawn_live;

/// Apply the container's `--restart` policy after an exit. Restarts on `always`/`unless-stopped`
/// (any exit) or `on-failure` (non-zero exit, up to MaximumRetryCount). `no`/empty never restarts.
/// A short backoff avoids a tight crash-loop. Spawns a fresh [`Live`] (the old one is spent) and
/// re-enters [`spawn_live`], whose reaper re-applies this policy on the next exit.
pub(super) fn maybe_restart<'a>(
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
