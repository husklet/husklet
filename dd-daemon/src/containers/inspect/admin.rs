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
    let candidates: Vec<String> = g
        .containers
        .iter()
        .filter(|(_, c)| container_is_prunable(&c.status))
        .map(|(id, _)| id.clone())
        .collect();
    let mut dead: Vec<String> = Vec::new();
    for id in &candidates {
        // Reclaim each pruned container's private writable upper layer (mirrors `docker rm`). If that
        // cleanup FAILS, keep the container in state (retryable) and skip it — don't drop state while
        // orphaning the layer.
        let upper = g.containers.get(id).map(|c| c.upper.clone()).unwrap_or_default();
        if discard_container_layer(&upper).is_err() {
            continue;
        }
        // Prune is equivalent to `docker rm` for eligible containers: drop the container from every
        // network's membership/endpoints (else the network keeps a phantom endpoint and becomes
        // undeletable — 403 "has active endpoints") and emit a `container/destroy` event so event-stream
        // mirrors learn of the removal, exactly as the explicit remove path does.
        let removed = g.containers.remove(id);
        for n in g.networks.iter_mut() {
            leave_network(n, id);
        }
        g.live.remove(id);
        if let Some(dc) = removed {
            crate::events::emit_event(
                &a.events,
                "container",
                "destroy",
                id,
                json!({"name": dc.name, "image": dc.image}),
            );
        }
        dead.push(id.clone());
    }
    save_state(&g, &a.state_path);
    Json(crate::api::ContainersPruneReport {
        containers_deleted: dead,
        space_reclaimed: 0,
    })
}

/// `docker update` request body (the resource subset dd tracks). Every field is optional — a `docker
/// update` is a partial update, so an omitted field leaves the stored value unchanged.
#[derive(serde::Deserialize, Default)]
pub(crate) struct ContainerUpdateBody {
    #[serde(rename = "Memory")]
    memory: Option<i64>,
    #[serde(rename = "PidsLimit")]
    pids_limit: Option<i64>,
    #[serde(rename = "NanoCpus")]
    nano_cpus: Option<i64>,
    #[serde(rename = "RestartPolicy")]
    restart_policy: Option<crate::model::RestartPolicy>,
}

/// Apply a `docker update` to a container's stored resource config. dd doesn't live-patch cgroups, but
/// the new limits must be persisted so inspect reports them and the next start honors them — previously
/// the whole body was ignored and inspect kept reporting the create-time values (e.g. Memory=0).
pub(crate) fn apply_update(c: &mut crate::model::Container, req: &ContainerUpdateBody) {
    if let Some(m) = req.memory {
        c.memory = m;
    }
    if let Some(p) = req.pids_limit {
        c.pids_limit = p;
    }
    if let Some(n) = req.nano_cpus {
        c.nano_cpus = n;
    }
    if let Some(rp) = &req.restart_policy {
        c.restart_policy = rp.clone();
    }
}

/// `POST /containers/{id}/update` — `docker update`. Persists the requested memory/pids/cpu/restart-policy
/// changes to container state (dd does not live-patch cgroups) and returns the `{Warnings}` envelope.
pub(crate) async fn containers_update(
    State(a): State<App>,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    let req: ContainerUpdateBody = serde_json::from_slice(&body).unwrap_or_default();
    let mut g = a.inner.lock().await;
    let Some(full) = resolve_cid(&g, &id) else {
        return no_such(&id);
    };
    if let Some(c) = g.containers.get_mut(&full) {
        apply_update(c, &req);
    }
    save_state(&g, &a.state_path);
    Json(crate::api::ContainerUpdateResponse { warnings: vec![] }).into_response()
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

    // "Container Update Drops Resource Body" (P1): docker update must persist the sent memory/pids/cpu/
    // restart-policy, not silently return 200 with unchanged state.
    #[test]
    fn container_update_applies_resource_body() {
        let mut c = crate::model::Container::default();
        let req = ContainerUpdateBody {
            memory: Some(67_108_864),
            pids_limit: Some(42),
            nano_cpus: Some(1_500_000_000),
            restart_policy: Some(crate::model::RestartPolicy {
                name: "on-failure".into(),
                max_retry: 3,
            }),
        };
        apply_update(&mut c, &req);
        assert_eq!(c.memory, 67_108_864);
        assert_eq!(c.pids_limit, 42);
        assert_eq!(c.nano_cpus, 1_500_000_000);
        assert_eq!(c.restart_policy.name, "on-failure");
        assert_eq!(c.restart_policy.max_retry, 3);
    }

    // A partial update leaves omitted fields unchanged.
    #[test]
    fn container_update_is_partial() {
        let mut c = crate::model::Container {
            memory: 100,
            pids_limit: 5,
            ..Default::default()
        };
        apply_update(
            &mut c,
            &ContainerUpdateBody { pids_limit: Some(9), ..Default::default() },
        );
        assert_eq!(c.memory, 100, "omitted Memory stays unchanged");
        assert_eq!(c.pids_limit, 9, "sent PidsLimit is applied");
    }
}
