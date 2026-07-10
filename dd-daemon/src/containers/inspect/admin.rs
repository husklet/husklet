//! Docker-conformance teardown/maintenance endpoints: `prune`, `update`, `export`.
use super::super::*;
use super::diff::discard_container_layer;

/// Whether a container in `status` is eligible for `docker container prune`. Prune reclaims STOPPED
/// containers (created / exited / dead) but must NOT remove ones still active from the user's point of
/// view: `running`, `paused`, or `restarting` — a restarting container is waiting on restart-policy
/// backoff, and pruning it silently cancels the scheduled restart.
pub(crate) fn container_is_prunable(status: &str) -> bool {
    !matches!(status, "running" | "paused" | "restarting")
}

/// `POST /containers/prune` — `docker container prune`. Removes exited (non-running) containers and
/// reports what was deleted.
pub(crate) async fn containers_prune(State(a): State<App>) -> Json<crate::api::ContainersPruneReport> {
    let mut g = a.inner.lock().await;
    let dead: Vec<String> = g
        .containers
        .iter()
        .filter(|(_, c)| container_is_prunable(&c.status))
        .map(|(id, _)| id.clone())
        .collect();
    for id in &dead {
        // Reclaim each pruned container's private writable upper layer (mirrors `docker rm`).
        if let Some(c) = g.containers.get(id) {
            discard_container_layer(&c.upper.clone());
        }
        g.containers.remove(id);
        g.live.remove(id);
    }
    save_state(&g, &a.state_path);
    Json(crate::api::ContainersPruneReport {
        containers_deleted: dead,
        space_reclaimed: 0,
    })
}

/// `POST /containers/{id}/update` — `docker update`. dd does not apply live resource limits; accept
/// the request and return the conformant `{Warnings}` envelope.
pub(crate) async fn containers_update(
    State(a): State<App>,
    Path(id): Path<String>,
    _body: axum::body::Bytes,
) -> Response {
    let g = a.inner.lock().await;
    match resolve_cid(&g, &id) {
        Some(_) => Json(crate::api::ContainerUpdateResponse { warnings: vec![] }).into_response(),
        None => no_such(&id),
    }
}

/// `GET /containers/{id}/export` — `docker export`. Streams a tar of the container rootfs.
pub(crate) async fn containers_export(State(a): State<App>, Path(id): Path<String>) -> Response {
    let rootfs = {
        let g = a.inner.lock().await;
        match resolve_get(&g, &id).map(|(_, c)| c.rootfs.clone()) {
            Some(r) => r,
            None => return no_such(&id),
        }
    };
    match std::process::Command::new("tar")
        .arg("cf")
        .arg("-")
        .arg("-C")
        .arg(&rootfs)
        .arg(".")
        .output()
    {
        Ok(o) if o.status.success() => Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/x-tar")
            .body(Body::from(o.stdout))
            .unwrap(),
        _ => server_error("export failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // "Container Prune Deletes Restarting Containers" (P1): prune reclaims stopped containers but must
    // keep running/paused/restarting ones (a restarting container is waiting on restart-policy backoff).
    #[test]
    fn prune_keeps_active_containers_including_restarting() {
        assert!(container_is_prunable("exited"));
        assert!(container_is_prunable("created"));
        assert!(container_is_prunable("dead"));
        assert!(!container_is_prunable("running"));
        assert!(!container_is_prunable("paused"));
        assert!(!container_is_prunable("restarting"));
    }
}
