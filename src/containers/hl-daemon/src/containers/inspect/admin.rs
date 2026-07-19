//! Docker-conformance teardown/maintenance endpoints: `prune`, `update`, `export`.
use super::super::*;
use super::diff::Overlay;

/// Whether a container in `status` is eligible for `docker container prune`. Prune reclaims STOPPED
/// containers (created / exited / dead) but must NOT remove ones still active from the user's point of
/// view: `running`, `paused`, or `restarting` — a restarting container is waiting on restart-policy
/// backoff, and pruning it silently cancels the scheduled restart.
impl Container {
    pub(crate) fn is_prunable(&self) -> bool {
        !matches!(self.status.as_str(), "running" | "paused" | "restarting")
    }
}

/// `POST /containers/prune` — `docker container prune`. Removes exited (non-running) containers and
/// reports what was deleted.
impl Containers {
    pub(crate) async fn prune(State(a): State<App>) -> Json<crate::api::ContainersPruneReport> {
        let mut g = a.inner.lock().await;
        let candidates: Vec<String> = g
            .containers
            .iter()
            .filter(|(_, container)| container.is_prunable())
            .map(|(id, _)| id.clone())
            .collect();
        let mut dead: Vec<String> = Vec::new();
        for id in &candidates {
            // Reclaim each pruned container's private writable upper layer (mirrors `docker rm`). If that
            // cleanup FAILS, keep the container in state (retryable) and skip it — don't drop state while
            // orphaning the layer.
            let upper = g
                .containers
                .get(id)
                .map(|c| c.upper.clone())
                .unwrap_or_default();
            if (Overlay {
                upper: &upper,
                rootfs: "",
            })
            .discard()
            .is_err()
            {
                continue;
            }
            // Prune is equivalent to `docker rm` for eligible containers: drop the container from every
            // network's membership/endpoints (else the network keeps a phantom endpoint and becomes
            // undeletable — 403 "has active endpoints") and emit a `container/destroy` event so event-stream
            // mirrors learn of the removal, exactly as the explicit remove path does.
            let removed = g.containers.remove(id);
            for n in g.networks.iter_mut() {
                n.leave(id);
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
        Store::save(&g, &a.state_path);
        Json(crate::api::ContainersPruneReport {
            containers_deleted: dead,
            space_reclaimed: 0,
        })
    }
}

/// `docker update` request body (the resource subset hl tracks). Every field is optional — a `docker
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

/// Apply a `docker update` to a container's stored resource config. hl doesn't live-patch cgroups, but
/// the new limits must be persisted so inspect reports them and the next start honors them — previously
/// the whole body was ignored and inspect kept reporting the create-time values (e.g. Memory=0).
impl Container {
    pub(crate) fn apply_update(&mut self, req: &ContainerUpdateBody) {
        if let Some(m) = req.memory {
            self.memory = m;
        }
        if let Some(p) = req.pids_limit {
            self.pids_limit = p;
        }
        if let Some(n) = req.nano_cpus {
            self.nano_cpus = n;
        }
        if let Some(rp) = &req.restart_policy {
            self.restart_policy = rp.clone();
        }
    }
}

/// `POST /containers/{id}/update` — `docker update`. Persists the requested memory/pids/cpu/restart-policy
/// changes to container state (hl does not live-patch cgroups) and returns the `{Warnings}` envelope.
pub(crate) async fn containers_update(
    State(a): State<App>,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    let req: ContainerUpdateBody = serde_json::from_slice(&body).unwrap_or_default();
    let mut g = a.inner.lock().await;
    let Some(full) = ContainerId::resolve(&g, &id) else {
        return ErrorMessage::no_such(&id);
    };
    if let Some(c) = g.containers.get_mut(&full) {
        c.apply_update(&req);
    }
    Store::save(&g, &a.state_path);
    Json(crate::api::ContainerUpdateResponse { warnings: vec![] }).into_response()
}

/// `GET /containers/{id}/export` — `docker export`. Streams a tar of the container FILESYSTEM (the image
/// rootfs with the container's writable upper layer merged on top), not the bare lower image — otherwise
/// the export silently drops every write the container made.
impl Containers {
    pub(crate) async fn export(State(a): State<App>, Path(id): Path<String>) -> Response {
        let (rootfs, upper) = {
            let g = a.inner.lock().await;
            match ContainerId::get(&g, &id).map(|(_, c)| (c.rootfs.clone(), c.upper.clone())) {
                Some(r) => r,
                None => return ErrorMessage::no_such(&id),
            }
        };
        // With no writable upper (darwin / legacy), tar the rootfs directly. With one, materialize the merged
        // view into a temp dir (lower copied, then upper overlaid so container writes win) and tar THAT.
        let merged_tmp = if !upper.is_empty() && std::path::Path::new(&upper).is_dir() {
            let tmp = hl_home().join("export-tmp").join(&id[..id.len().min(16)]);
            let _ = std::fs::remove_dir_all(&tmp);
            if std::fs::create_dir_all(&tmp).is_err() {
                return ErrorMessage::server_error("export: failed to stage merged rootfs");
            }
            let cp_lower = std::process::Command::new("cp")
                .arg("-a")
                .arg(format!("{rootfs}/."))
                .arg(&tmp)
                .status();
            if !matches!(cp_lower, Ok(s) if s.success()) {
                let _ = std::fs::remove_dir_all(&tmp);
                return ErrorMessage::server_error("export: failed to stage lower rootfs");
            }
            let _ = std::process::Command::new("cp")
                .arg("-a")
                .arg(format!("{upper}/."))
                .arg(&tmp)
                .status();
            Some(tmp)
        } else {
            None
        };
        let tar_dir = merged_tmp
            .as_ref()
            .map(|t| t.to_string_lossy().into_owned())
            .unwrap_or_else(|| rootfs.clone());
        let out = std::process::Command::new("tar")
            .arg("cf")
            .arg("-")
            .arg("-C")
            .arg(&tar_dir)
            .arg(".")
            .output();
        if let Some(t) = &merged_tmp {
            let _ = std::fs::remove_dir_all(t);
        }
        match out {
            Ok(o) if o.status.success() => Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "application/x-tar")
                .body(Body::from(o.stdout))
                .unwrap(),
            _ => ErrorMessage::server_error("export failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // "Container Prune Deletes Restarting Containers" (P1): prune reclaims stopped containers but must
    // keep running/paused/restarting ones (a restarting container is waiting on restart-policy backoff).
    #[test]
    fn prune_keeps_active_containers_including_restarting() {
        let mut container = Container::default();
        for status in ["exited", "created", "dead"] {
            container.status = status.into();
            assert!(container.is_prunable());
        }
        for status in ["running", "paused", "restarting"] {
            container.status = status.into();
            assert!(!container.is_prunable());
        }
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
        c.apply_update(&req);
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
        c.apply_update(&ContainerUpdateBody {
            pids_limit: Some(9),
            ..Default::default()
        });
        assert_eq!(c.memory, 100, "omitted Memory stays unchanged");
        assert_eq!(c.pids_limit, 9, "sent PidsLimit is applied");
    }
}
