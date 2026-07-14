//! POST /exec/:id/start -- run the exec (hijack/attach or detached).
use super::*;

/// POST /exec/:id/start body: `{"Detach": bool, "Tty": bool}`. Detach => run the exec in the
/// background and return 200 (no connection hijack).
#[derive(Deserialize, Default)]
pub(crate) struct ExecStartBody {
    #[serde(rename = "Detach", default)]
    detach: bool,
    #[serde(rename = "Tty", default)]
    #[allow(dead_code)] // wire-contract field: parsed from the exec-start body but not acted on here
    tty: bool,
}

/// POST /exec/:id/start -- run the exec command as a fresh JIT in the container's rootfs. With
/// `Detach=false` (the default) stream its IO over the hijacked connection (same path as attach);
/// with `Detach=true` spawn it in the background and return 200 immediately (no upgrade, no wait).
pub(crate) async fn exec_start(
    State(a): State<App>,
    Path(id): Path<String>,
    req: Request,
) -> Response {
    // Read the (small) JSON start body for Detach, keeping the request parts so the OnUpgrade
    // extension survives for the hijack path. `to_bytes` consumes the body; we rebuild an empty one.
    let (parts, body) = req.into_parts();
    let detach = axum::body::to_bytes(body, 64 * 1024)
        .await
        .ok()
        .and_then(|b| serde_json::from_slice::<ExecStartBody>(&b).ok())
        .map(|b| b.detach)
        .unwrap_or(false);
    let (temp, vols, live, tty) = {
        let mut g = a.inner.lock().await;
        let Some(exec) = g.execs.get(&id).cloned() else {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": format!("no such exec: {id}")})),
            )
                .into_response();
        };
        // An exec is SINGLE-USE: a second `/exec/{id}/start` must not spawn a duplicate process (which
        // could then race the first's Live entry). Docker returns 409 for an already-started exec.
        if exec.started {
            return conflict(format!("Container exec {id} is already running"));
        }
        let Some(c) = g.containers.get(&exec.container_id).cloned() else {
            return no_such(&exec.container_id);
        };
        // Re-validate the PARENT container's current state: it may have stopped/paused between exec create
        // and start. Docker rejects an exec start against a non-running container (the stale exec handle
        // must not run against a dead lifecycle).
        if c.status == "paused" {
            return conflict(format!(
                "Container {} is paused, unpause the container before exec",
                exec.container_id
            ));
        }
        if c.status != "running" {
            return conflict(format!("Container {} is not running", exec.container_id));
        }
        let mut temp = c; // share the container's rootfs/volumes/arch; distinct id -> own process
        temp.id = id.clone();
        // Share the TARGET container's loopback network: `docker exec` joins the container's netns, so the
        // exec'd process must reach 127.0.0.1 servers the container's init is listening on (redis-cli ping,
        // psql -h 127.0.0.1, etc.). Without this the id-derived HL_NETNS key would isolate the exec in its
        // own loopback and those connects would fail. `exec.container_id` is the parent container's id.
        temp.netns_key = Some(exec.container_id.clone());
        temp.cmd = exec.cmd.clone();
        temp.tty = exec.tty;
        // `docker exec -e/-w/-u`: the exec inherits the container's env and adds `-e` overrides (later
        // wins), `-w` overrides the working dir, and `-u U[:G]` overrides the run user. spawn_cfg reads
        // temp.env / temp.working_dir / temp.user (-> HL_UID/HL_GID), so set them on the temp here.
        temp.env.extend(exec.env.iter().cloned());
        if !exec.working_dir.is_empty() {
            temp.working_dir = exec.working_dir.clone();
        }
        if !exec.user.is_empty() {
            temp.user = exec.user.clone();
        }
        let live = Live::new();
        g.live.insert(id.clone(), live.clone());
        if let Some(e) = g.execs.get_mut(&id) {
            e.started = true;
        }
        // Docker `exec_start: <cmd>` event (Actor = the parent container). The matching `exec_die` is
        // emitted by the reaper when the exec process exits.
        crate::events::emit_event(
            &a.events,
            "container",
            &format!("exec_start: {}", exec.cmd.join(" ")),
            &exec.container_id,
            json!({"execID": id, "name": exec.container_id}),
        );
        (temp, g.volumes.clone(), live, exec.tty)
    };
    if detach {
        // Detached exec: spawn the process in the background (spawn_live already runs+reaps it in a
        // task) and return 200 immediately. No hijack, so the client doesn't block.
        spawn_live(&a, &temp, &vols, live).await;
        return StatusCode::OK.into_response();
    }
    let req = Request::from_parts(parts, Body::empty()); // carries OnUpgrade in extensions
    spawn_hijack_io(hyper::upgrade::on(req), live.clone(), tty); // subscribe before spawning
    spawn_live(&a, &temp, &vols, live).await;
    hijack_response()
}
