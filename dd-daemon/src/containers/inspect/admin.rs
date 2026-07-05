#![allow(unused_imports, dead_code)]
//! Docker-conformance teardown/maintenance endpoints: `prune`, `update`, `export`.
use super::super::*;
use super::diff::discard_container_layer;

/// `POST /containers/prune` — `docker container prune`. Removes exited (non-running) containers and
/// reports what was deleted.
pub(crate) async fn containers_prune(State(a): State<App>) -> Json<Value> {
    let mut g = a.inner.lock().await;
    let dead: Vec<String> = g
        .containers
        .iter()
        .filter(|(_, c)| c.status != "running" && c.status != "paused")
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
    Json(json!({"ContainersDeleted": dead, "SpaceReclaimed": 0}))
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
        Some(_) => Json(json!({"Warnings": []})).into_response(),
        None => no_such(&id),
    }
}

/// `GET /containers/{id}/export` — `docker export`. Streams a tar of the container rootfs.
pub(crate) async fn containers_export(State(a): State<App>, Path(id): Path<String>) -> Response {
    let rootfs = {
        let g = a.inner.lock().await;
        match resolve_cid(&g, &id)
            .and_then(|f| g.containers.get(&f))
            .map(|c| c.rootfs.clone())
        {
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
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"message": "export failed"})),
        )
            .into_response(),
    }
}
