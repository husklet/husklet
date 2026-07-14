use super::*;
use super::spawn::spawn_container;

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
    // Build the probe's container spec, then run it to completion via the typed one-shot capture. dd-jit
    // owns the spawn (piped stdio, combined stdout+stderr, reap); a timeout SIGKILLs it and yields (-1, …).
    let Some(container) = spawn_container(&temp, &app.volumes_dir, vols, None) else {
        return (-1, String::new());
    };
    let rt = match JitRuntime::new() {
        Ok(r) => r.cache_dir(crate::util::hl_home().join("pcache").to_string_lossy().into_owned()),
        Err(e) => return (-1, format!("probe runtime: {e}")),
    };
    let timeout_ns = if hcfg.timeout > 0 {
        hcfg.timeout
    } else {
        30_000_000_000
    };
    let timeout = std::time::Duration::from_nanos(timeout_ns as u64);
    match rt.output(&container, Some(timeout)).await {
        Ok((code, bytes)) => {
            let s: String = String::from_utf8_lossy(&bytes).chars().take(4096).collect();
            (code, s)
        }
        Err(e) => (-1, format!("probe: {e}")),
    }
}

/// The HEALTHCHECK monitor loop for one running container (§8.3-1). Probes every `interval` (default 30s),
/// maintaining inspect `State.Health`: exit 0 ⇒ healthy + streak reset; a non-zero probe increments the
/// failing streak and, once it reaches `retries` (default 3) AND the `start_period` grace has elapsed,
/// flips to unhealthy. Keeps the last 5 probe results in `Log[]`. Emits `health_status: …` events on a
/// transition. Exits when the container's process dies (this Live's `exit` fires) or it stops running.
pub(super) async fn health_monitor(
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
        let (next, streak) = apply_probe(&h.status, h.failing_streak, code, retries, in_start_period);
        h.status = next;
        h.failing_streak = streak;
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

/// Fold one probe result into `(status, failing_streak)` — the Docker HEALTHCHECK state machine.
///
/// A success (exit 0) makes the container `healthy` and clears the streak, even during the start
/// period. A failure DURING the start period is a documented grace: it neither counts toward
/// `--retries` nor changes the status (the container stays `starting`) — matching the `start_period`
/// field's own contract ("grace where a failure doesn't count"). Only AFTER the start period does a
/// failure increment the streak, and the container becomes `unhealthy` once the streak reaches
/// `retries`. Split out as a pure fn so the transition thresholds are unit-testable without a probe.
fn apply_probe(
    status: &str,
    streak: i64,
    code: i64,
    retries: i64,
    in_start_period: bool,
) -> (String, i64) {
    if code == 0 {
        ("healthy".to_string(), 0)
    } else if in_start_period {
        (status.to_string(), streak) // grace-window failure: uncounted, status unchanged
    } else {
        let streak = streak + 1;
        let next = if streak >= retries { "unhealthy" } else { status };
        (next.to_string(), streak)
    }
}

#[cfg(test)]
mod tests {
    use super::apply_probe;

    #[test]
    fn success_is_healthy_and_clears_streak() {
        assert_eq!(apply_probe("starting", 2, 0, 3, false), ("healthy".into(), 0));
        assert_eq!(apply_probe("unhealthy", 5, 0, 3, false), ("healthy".into(), 0));
        // a success DURING the start period is immediately healthy too.
        assert_eq!(apply_probe("starting", 0, 0, 3, true), ("healthy".into(), 0));
    }

    #[test]
    fn failures_during_start_period_are_not_counted() {
        // Regression: grace-window failures used to increment the streak, so accumulated start-period
        // failures flipped the container to `unhealthy` after a single real post-grace failure. During
        // the start period a failure must leave both status and streak untouched.
        assert_eq!(apply_probe("starting", 0, 1, 3, true), ("starting".into(), 0));
        assert_eq!(apply_probe("starting", 0, 1, 3, true), ("starting".into(), 0));
        // ...so the FIRST post-grace failure starts the streak at 1, NOT at 3 (== retries).
        assert_eq!(apply_probe("starting", 0, 1, 3, false), ("starting".into(), 1));
    }

    #[test]
    fn unhealthy_only_after_retries_consecutive_post_grace_failures() {
        let (s, n) = apply_probe("starting", 0, 1, 3, false);
        assert_eq!((s.as_str(), n), ("starting", 1));
        let (s, n) = apply_probe(&s, n, 1, 3, false);
        assert_eq!((s.as_str(), n), ("starting", 2));
        let (s, n) = apply_probe(&s, n, 1, 3, false);
        assert_eq!((s.as_str(), n), ("unhealthy", 3)); // 3rd counted failure == retries
    }
}
