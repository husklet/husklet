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
        let new_name = name.trim_start_matches('/').to_string();
        // A rename onto a name already held by a DIFFERENT container is a 409 (docker keeps names
        // unique). Without this the field was overwritten, yielding two containers with the same name
        // and an ambiguous resolve_cid.
        if g.containers.values().any(|c| c.id != full && c.name == new_name) {
            return conflict(format!(
                "Conflict. The container name \"/{new_name}\" is already in use"
            ));
        }
        // Re-alias the container's network endpoints so `network inspect` and the live DNS `.names`
        // track the rename (endpoints are keyed by container id, not name).
        crate::networks::rename_endpoints(&mut g.networks, &full, &new_name);
        let old_name = g
            .containers
            .get(&full)
            .map(|c| c.name.clone())
            .unwrap_or_default();
        let image = if let Some(c) = g.containers.get_mut(&full) {
            c.name = new_name.clone();
            c.image.clone()
        } else {
            String::new()
        };
        // Emit `container/rename` so event-stream mirrors learn the new name without re-polling inspect.
        // Docker's rename event carries the old and new names in the actor attributes.
        crate::events::emit_event(
            &a.events,
            "container",
            "rename",
            &full,
            json!({"name": new_name, "oldName": old_name, "image": image}),
        );
    }
    save_state(&g, &a.state_path);
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub(crate) struct WaitQ {
    /// `docker wait --condition`: `not-running` (default), `next-exit`, or `removed`.
    condition: Option<String>,
}

/// POST /containers/:id/wait -- block until the container reaches the requested `condition`, then return
/// {"StatusCode": n}. CRITICAL: the docker `run` CLI sends this BEFORE /start and reads it concurrently,
/// so we flush the response HEADERS immediately (200) and stream the JSON body only once the condition is
/// met -- otherwise the CLI blocks waiting for the response and never sends /start (a deadlock).
///
/// Conditions:
///   - `not-running` (default) / `next-exit`: block until the container EXITS. A `created` container that
///     has never started must NOT return a fake `StatusCode:0` immediately (orchestrators would treat a
///     never-started container as completed) — we poll until it starts and exits.
///   - `removed`: block until the container no longer exists (a `docker rm`), so cleanup orchestration
///     doesn't race a still-present container.
pub(crate) async fn containers_wait(
    State(a): State<App>,
    Path(id): Path<String>,
    Query(q): Query<WaitQ>,
) -> Response {
    let full = {
        let g = a.inner.lock().await;
        match resolve_cid(&g, &id) {
            Some(f) => f,
            None => return no_such(&id),
        }
    };
    let condition = q.condition.unwrap_or_default();
    let stream = futures_util::stream::once(async move {
        let code = wait_for_condition(&a, &full, &condition).await;
        Ok::<_, std::io::Error>(format!("{{\"StatusCode\":{code}}}\n").into_bytes())
    });
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from_stream(stream))
        .unwrap()
}

/// Block until a wait condition is satisfied, returning the container's exit code (0 if never recorded).
/// Polls daemon state (re-locking each tick, never holding the lock across an await) so it observes a
/// concurrent start/exit/remove; when a `Live` exists it waits on its exit signal for prompt wakeup.
async fn wait_for_condition(a: &App, full: &str, condition: &str) -> i64 {
    let removed = condition == "removed";
    let mut last_code = 0;
    loop {
        let (present, exited, code, live) = {
            let g = a.inner.lock().await;
            match g.containers.get(full) {
                Some(c) => (
                    true,
                    c.status == "exited" || c.status == "dead",
                    c.exit_code,
                    g.live.get(full).cloned(),
                ),
                None => (false, false, last_code, None),
            }
        };
        if removed {
            if !present {
                return last_code; // container gone — condition met
            }
            if exited {
                last_code = code; // remember the exit code to report once it's actually removed
            }
        } else {
            if !present {
                return last_code; // removed out from under us — nothing left to wait on
            }
            if exited {
                return code;
            }
        }
        // Prefer the exit watch (prompt) when a live process exists; otherwise poll (created container
        // that hasn't started yet, or a `removed` wait after exit). Cap the wait so removal is re-checked.
        if let Some(live) = live {
            let mut rx = live.exit_rx.clone();
            let _ = tokio::time::timeout(std::time::Duration::from_millis(100), rx.changed()).await;
        } else {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct DeleteQ {
    force: Option<String>,
    v: Option<String>,
    #[allow(dead_code)] // wire-contract query param (`?link=`): accepted so the query deserializes, not applied
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
        return conflict(format!(
            "cannot remove a running container {short}: Stop the container before removing or force remove"
        ));
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
        // The shared image rootfs (the read-only lower) is never touched. If that cleanup FAILS, restore
        // the container to state (retryable) and return an error rather than orphaning the layer while
        // reporting a successful remove.
        if let Err(e) = discard_container_layer(&dc.upper) {
            g.containers.insert(full.clone(), dc);
            return server_error(format!("failed to remove container writable layer: {e}"));
        }
        // Also drop its live IO plumbing (log buffers + channels); otherwise `docker rm` leaks them.
        g.live.remove(&full);
        save_state(&g, &a.state_path);
        StatusCode::NO_CONTENT.into_response()
    } else {
        no_such(&id)
    }
}
