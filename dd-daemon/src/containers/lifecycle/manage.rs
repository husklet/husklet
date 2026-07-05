#![allow(unused_imports, dead_code)]
//! Container management: `rename`, `wait`, and `delete` (`docker rm`). Split out
//! of the former `lifecycle.rs`; behavior unchanged. The force-delete path reuses
//! `kill_group` from the sibling `run` module.
use super::super::*;
use super::run::kill_group;

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
