//! Container lifecycle / error-path / wire-shape tests + container-only multi-step flows.
use super::*;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;

/// Build a `RenameQ` query (`?name=...`) for `containers_rename`.
fn rename_q(name: &str) -> crate::containers::RenameQ {
    serde_json::from_value(serde_json::json!({ "name": name })).unwrap()
}

// ---- 1. containers_kill on an exited container -> 409, state UNCHANGED ------------------------
#[tokio::test]
async fn kill_exited_container_is_409_and_unchanged() {
    let app = test_app();
    seed_container(&app, "c1", "exited").await;
    let resp = crate::containers::containers_kill(
        State(app.clone()),
        Path("c1".into()),
        Query(empty_q()),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let g = app.inner.lock().await;
    let c = &g.containers["c1"];
    assert_eq!(c.status, "exited", "status must be untouched");
    assert_eq!(c.finished_at, 1000, "finished_at must not be rewritten");
}

// ---- 3. containers_stop (do_stop) on an already-stopped container -> 304, state unchanged -----
#[tokio::test]
async fn stop_already_exited_is_304_and_unchanged() {
    let app = test_app();
    seed_container(&app, "c1", "exited").await;
    let resp = crate::containers::containers_stop(
        State(app.clone()),
        Path("c1".into()),
        Query(empty_q()),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
    let g = app.inner.lock().await;
    let c = &g.containers["c1"];
    assert_eq!(c.status, "exited");
    assert_eq!(c.finished_at, 1000, "finished_at must not be rewritten");
}

// ---- 4. containers_start on already-running / already-paused -> 304, started_at not reset -----
#[tokio::test]
async fn start_already_running_is_304_and_unchanged() {
    let app = test_app();
    seed_container(&app, "c1", "running").await;
    let resp =
        crate::containers::containers_start(State(app.clone()), Path("c1".into())).await;
    assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
    let g = app.inner.lock().await;
    let c = &g.containers["c1"];
    assert_eq!(c.status, "running");
    assert_eq!(c.started_at, 500, "started_at must not be reset");
}

#[tokio::test]
async fn start_already_paused_is_304_and_unchanged() {
    let app = test_app();
    seed_container(&app, "c1", "paused").await;
    let resp =
        crate::containers::containers_start(State(app.clone()), Path("c1".into())).await;
    assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
    let g = app.inner.lock().await;
    let c = &g.containers["c1"];
    assert_eq!(c.status, "paused");
    assert_eq!(c.started_at, 500);
}

// ---- 6. containers_json (no `all`) INCLUDES a paused container (ps-paused fix) ----------------
#[tokio::test]
async fn containers_json_includes_paused_without_all() {
    let app = test_app();
    seed_container(&app, "prunning", "running").await;
    seed_container(&app, "ppaused", "paused").await;
    seed_container(&app, "pexited", "exited").await;
    let axum::Json(list) =
        crate::containers::containers_json(State(app.clone()), Query(empty_q())).await;
    let ids: Vec<&str> = list.iter().map(|c| c.id.as_str()).collect();
    assert!(ids.contains(&"prunning"), "running must be listed");
    assert!(ids.contains(&"ppaused"), "paused must be listed (ps-paused fix)");
    assert!(
        !ids.contains(&"pexited"),
        "exited must NOT be listed without all"
    );
}

// ---- 9. containers_json wire shape: per-row docker keys + Names leading-slash ----------------
#[tokio::test]
async fn containers_json_wire_shape_and_all_filter() {
    let app = test_app();
    seed_container(&app, "crunning0000", "running").await;
    seed_container(&app, "cpaused00000", "paused").await;
    seed_container(&app, "cexited00000", "exited").await;

    // Default (no `all`): running + paused only (exited hidden).
    let axum::Json(def) =
        crate::containers::containers_json(State(app.clone()), Query(empty_q())).await;
    assert_eq!(def.len(), 2, "default ps lists running+paused, not exited");

    // `all=true`: every container.
    let all_q: crate::containers::PsQ =
        serde_json::from_value(serde_json::json!({"all": "true"})).unwrap();
    let axum::Json(all) =
        crate::containers::containers_json(State(app.clone()), Query(all_q)).await;
    assert_eq!(all.len(), 3, "all=true lists every container");

    // Serialize to the wire and assert the exact docker keys/casing on each row.
    let v = serde_json::to_value(&all).unwrap();
    let row = &v.as_array().unwrap()[0];
    let obj = row.as_object().unwrap();
    for key in [
        "Id", "Names", "Image", "State", "Status", "Ports", "Labels", "Command", "Created",
        "Mounts", "ExitCode",
    ] {
        assert!(obj.contains_key(key), "row missing wire key {key}: {row}");
    }
    // `--size` was NOT requested → SizeRw/SizeRootFs omitted (docker omits the keys).
    assert!(!obj.contains_key("SizeRw"), "SizeRw must be omitted without --size");
    assert!(!obj.contains_key("SizeRootFs"));
    // Names is an array whose sole entry carries a leading '/'.
    let names = row["Names"].as_array().unwrap();
    assert_eq!(names.len(), 1);
    assert!(
        names[0].as_str().unwrap().starts_with('/'),
        "Names entry must start with '/': {}",
        names[0]
    );
    assert!(row["Ports"].is_array(), "Ports is an array");
    assert!(row["Image"].as_str().unwrap() == "alpine");
}

// ---- 10. containers_inspect wire shape: top-level + nested State keys, Name '/' -------------
#[tokio::test]
async fn containers_inspect_wire_shape() {
    let app = test_app();
    seed_container(&app, "inspectme000", "exited").await;
    let resp = crate::containers::containers_inspect(State(app.clone()), Path("inspectme000".into()))
        .await
        .into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = to_body_json(resp).await;
    let obj = v.as_object().unwrap();
    for key in [
        "Id", "Name", "State", "Config", "HostConfig", "NetworkSettings", "Mounts", "Image",
        "Created", "RestartCount",
    ] {
        assert!(obj.contains_key(key), "inspect missing top-level key {key}");
    }
    assert!(
        v["Name"].as_str().unwrap().starts_with('/'),
        "inspect Name must start with '/'"
    );
    // Nested State carries the docker booleans + Status/ExitCode for an exited container.
    let state = v["State"].as_object().unwrap();
    for key in [
        "Status", "Running", "Paused", "Restarting", "OOMKilled", "Dead", "ExitCode", "Pid",
        "StartedAt", "FinishedAt", "Error",
    ] {
        assert!(state.contains_key(key), "State missing key {key}");
    }
    assert_eq!(v["State"]["Status"], "exited");
    assert_eq!(v["State"]["Running"], false, "exited container is not Running");
    // Config/HostConfig/NetworkSettings are objects (not null).
    assert!(v["Config"].is_object());
    assert!(v["HostConfig"].is_object());
    assert!(v["NetworkSettings"]["Ports"].is_object() || v["NetworkSettings"]["Ports"].is_null());
}

#[tokio::test]
async fn containers_inspect_missing_is_404() {
    let app = test_app();
    let resp = crate::containers::containers_inspect(State(app.clone()), Path("nope".into()))
        .await
        .into_response();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---- FLOW 3: container name conflict — second `--name web` is 409, state stays single ---------
// docker: `create --name web` twice is a 409 Conflict; the daemon does NOT record a second
// container. `containers_create` is engine-free (records state + allocates the overlay upper dir,
// never spawns), so it is driven directly here.
#[tokio::test]
async fn flow_container_name_conflict_keeps_single() {
    let app = test_app();
    seed_image_rootfs(&app, "alpine", "/store/alpine-rootfs").await;

    let create_web = |app: App| async move {
        let q: crate::containers::CreateQ =
            serde_json::from_value(serde_json::json!({"name":"web"})).unwrap();
        let body =
            axum::Json(serde_json::from_value(serde_json::json!({"Image":"alpine"})).unwrap());
        crate::containers::containers_create(State(app), Query(q), body).await
    };

    // Step 1: first create — 201, exactly one container named `web`.
    let r = create_web(app.clone()).await;
    assert_eq!(r.status(), StatusCode::CREATED, "first --name web creates");
    assert_eq!(
        app.inner.lock().await.containers.values().filter(|c| c.name == "web").count(),
        1,
        "one container named web after the first create"
    );

    // Step 2: second create with the same name — 409, still exactly one.
    let r = create_web(app.clone()).await;
    assert_eq!(r.status(), StatusCode::CONFLICT, "duplicate --name web is 409");
    let msg = to_body_string(r).await;
    assert!(msg.contains("already in use"), "409 body names the conflict: {msg}");
    assert_eq!(
        app.inner.lock().await.containers.values().filter(|c| c.name == "web").count(),
        1,
        "a rejected duplicate must NOT record a second container"
    );
}

// ---- FLOW 11: create --name -> rename -> inspect/lookup by new name; update returns 200 --------
// docker: `rename web app` frees `web` and binds `app`; inspect shows `/app`; lookup by `app` works;
// lookup by the OLD `web` fails. `docker update` returns 200 (a `{Warnings}` envelope). *** dd NOTE:
// update is a documented no-op — it does NOT persist the new resource limit; docker DOES reflect the
// new Memory in a later inspect. Asserted below as a soft divergence (flagged, not a hard bug). ***
#[tokio::test]
async fn flow_container_rename_then_inspect_lookup_and_update_noop() {
    let app = test_app();
    seed_image_rootfs(&app, "alpine", "/store/alpine-R").await;

    // Step 1: create --name web -> 201.
    let r = crate::containers::containers_create(
        State(app.clone()),
        Query(create_q(serde_json::json!({"name":"web"}))),
        create_body(serde_json::json!({"Image":"alpine"})),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CREATED);
    let cid = to_body_json(r).await["Id"].as_str().unwrap().to_string();
    assert_eq!(app.inner.lock().await.containers[&cid].name, "web");

    // Step 2: rename web -> app (204); the name field updates.
    let r = crate::containers::containers_rename(
        State(app.clone()),
        Path("web".into()),
        Query(rename_q("app")),
    )
    .await;
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
    assert_eq!(app.inner.lock().await.containers[&cid].name, "app", "name rebound to app");

    // Step 3: inspect (by new name AND by id) shows the new Name with the leading '/'.
    let r = crate::containers::containers_inspect(State(app.clone()), Path("app".into()))
        .await
        .into_response();
    assert_eq!(r.status(), StatusCode::OK, "lookup by the NEW name resolves");
    assert_eq!(to_body_json(r).await["Name"], "/app");
    // The OLD name no longer resolves (docker frees it on rename) -> 404.
    let r = crate::containers::containers_inspect(State(app.clone()), Path("web".into()))
        .await
        .into_response();
    assert_eq!(r.status(), StatusCode::NOT_FOUND, "old name `web` is freed by rename");

    // Step 4: containers_json (all) finds it by the new name.
    let all_q: crate::containers::PsQ =
        serde_json::from_value(serde_json::json!({"all":"true"})).unwrap();
    let axum::Json(list) =
        crate::containers::containers_json(State(app.clone()), Query(all_q)).await;
    let v = serde_json::to_value(&list).unwrap();
    let found = v.as_array().unwrap().iter().any(|row| {
        row["Names"]
            .as_array()
            .map(|ns| ns.iter().any(|n| n == "/app"))
            .unwrap_or(false)
    });
    assert!(found, "ps lists the container under its new name /app");

    // Step 5: update with a Memory limit -> 200. dd does NOT persist it (documented no-op).
    let r = crate::containers::containers_update(
        State(app.clone()),
        Path(cid.clone()),
        axum::body::Bytes::from_static(b"{\"Memory\":16000000}"),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK, "update returns 200 {{Warnings}}");
    let r = crate::containers::containers_inspect(State(app.clone()), Path(cid.clone()))
        .await
        .into_response();
    let v = to_body_json(r).await;
    assert_eq!(
        v["HostConfig"]["Memory"], 0,
        "NOTE/divergence: dd's update is a no-op — Memory stays 0; docker would reflect 16000000"
    );
}

// ---- FLOW 12: rename onto an EXISTING name — dd accepts a DUPLICATE (docker rejects with 409) ---
// docker contract: `docker rename web app` when `app` is already taken is a 409 Conflict; the rename
// is refused and both names stay distinct. *** DIVERGENCE (bug): dd's `containers_rename` never checks
// for a target-name collision — it silently overwrites, leaving TWO containers named `app`. A later
// `resolve_cid("app")` then resolves to an arbitrary one. Flagged, not fixed. ***
#[tokio::test]
async fn flow_rename_onto_existing_name_creates_duplicate_divergence() {
    let app = test_app();
    seed_image_rootfs(&app, "alpine", "/store/alpine-R").await;

    // Two distinct containers: `web` and `app`.
    let mk = |app: App, name: &'static str| async move {
        let r = crate::containers::containers_create(
            State(app.clone()),
            Query(create_q(serde_json::json!({ "name": name }))),
            create_body(serde_json::json!({"Image":"alpine"})),
        )
        .await;
        assert_eq!(r.status(), StatusCode::CREATED);
        to_body_json(r).await["Id"].as_str().unwrap().to_string()
    };
    let cid_web = mk(app.clone(), "web").await;
    let _cid_app = mk(app.clone(), "app").await;

    // Rename `web` -> `app` (already taken) is a 409 (docker keeps names unique); `web` is unchanged.
    let r = crate::containers::containers_rename(
        State(app.clone()),
        Path("web".into()),
        Query(rename_q("app")),
    )
    .await;
    assert_eq!(
        r.status(),
        StatusCode::CONFLICT,
        "rename onto a taken name is 409"
    );
    // No duplicate: still exactly one `app`, and `web` kept its name.
    let g = app.inner.lock().await;
    assert_eq!(g.containers.values().filter(|c| c.name == "app").count(), 1, "no duplicate name");
    assert_eq!(g.containers[&cid_web].name, "web", "the refused rename left web unchanged");
}

// ---- FLOW 15: restart-policy container, `stop` sets the DURABLE manual-stop flag -----------------
// docker: stopping a container that has a `--restart` policy records a manual-stop so the supervisor
// won't auto-restart it (Moby's HasBeenManuallyStopped). Engine-free: a "running" container with NO
// live process — `do_stop` finds no pid, skips the signal/wait, and just flips the recorded state.
#[tokio::test]
async fn flow_restart_policy_stop_sets_manual_stop_flag() {
    let app = test_app();
    // Seed a RUNNING container with `--restart always` and NO live IO plumbing (so stop is engine-free).
    {
        let mut g = app.inner.lock().await;
        g.containers.insert(
            "rc".into(),
            Container {
                id: "rc".into(),
                image: "alpine".into(),
                status: "running".into(),
                started_at: 500,
                restart_policy: crate::model::RestartPolicy {
                    name: "always".into(),
                    max_retry: 0,
                },
                ..Default::default()
            },
        );
    }
    // Pre-state: not manually stopped.
    assert!(!app.inner.lock().await.containers["rc"].manually_stopped);

    // stop -> 204, no live pid to signal, so it just records the stop.
    let r = crate::containers::containers_stop(
        State(app.clone()),
        Path("rc".into()),
        Query(empty_q()),
    )
    .await;
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
    let g = app.inner.lock().await;
    let c = &g.containers["rc"];
    assert_eq!(c.status, "exited", "stop flips a running container to exited");
    assert!(
        c.manually_stopped,
        "stop sets the DURABLE manual-stop flag so `--restart always` won't auto-resurrect it"
    );
    assert_ne!(c.finished_at, 0, "finished_at recorded on stop");
    // The restart policy itself is preserved (a future explicit start still honors it).
    assert_eq!(c.restart_policy.name, "always", "restart policy is not cleared by stop");
}

// ---- FLOW 16: `container prune` removes ONLY the exited/created ones, keeps the running one -------
// docker: `POST /containers/prune` (== `docker container prune`) removes every non-running,
// non-paused container and returns their ids in `ContainersDeleted`; a running container is kept.
// Drives seed(3) -> prune -> assert the deleted SET -> ps(all=1) reflects the removal.
#[tokio::test]
async fn flow_containers_prune_removes_only_stopped_keeps_running() {
    let app = test_app();
    seed_container(&app, "runid0000000", "running").await;
    seed_container(&app, "exitidA00000", "exited").await;
    seed_container(&app, "exitidB00000", "exited").await;

    // Prune: reclaims exactly the two exited ids; the running one is untouched.
    let axum::Json(rep) = crate::containers::containers_prune(State(app.clone())).await;
    let deleted: std::collections::HashSet<&str> =
        rep.containers_deleted.iter().map(|s| s.as_str()).collect();
    assert_eq!(deleted.len(), 2, "exactly two containers pruned: {deleted:?}");
    assert!(deleted.contains("exitidA00000"), "exited A reported deleted: {deleted:?}");
    assert!(deleted.contains("exitidB00000"), "exited B reported deleted: {deleted:?}");
    assert!(
        !deleted.contains("runid0000000"),
        "a RUNNING container must never be in the pruned set: {deleted:?}"
    );

    // In-memory state: only the running container remains.
    {
        let g = app.inner.lock().await;
        assert!(g.containers.contains_key("runid0000000"), "running container kept");
        assert!(!g.containers.contains_key("exitidA00000"), "exited A gone");
        assert!(!g.containers.contains_key("exitidB00000"), "exited B gone");
    }

    // ps (all=1) reflects the removal: a single row, the running container.
    let all_q: crate::containers::PsQ =
        serde_json::from_value(serde_json::json!({"all": "true"})).unwrap();
    let axum::Json(list) =
        crate::containers::containers_json(State(app.clone()), Query(all_q)).await;
    assert_eq!(list.len(), 1, "ps all=1 lists only the surviving running container");
    assert_eq!(list[0].id, "runid0000000");
}

// ---- FLOW 20: a `--name` freed by `rm` can be REUSED (no stale name reservation) --------------
// docker: `create --name web` then `rm web` FREES the name; a second `create --name web` is a 201
// (the name is only reserved while the container exists). Drives create -> delete -> create(same name).
#[tokio::test]
async fn flow_container_name_reusable_after_remove() {
    let app = test_app();
    seed_image_rootfs(&app, "alpine", "/store/alpine-R").await;

    let create_web = |app: App| async move {
        crate::containers::containers_create(
            State(app),
            Query(create_q(serde_json::json!({"name":"web"}))),
            create_body(serde_json::json!({"Image":"alpine"})),
        )
        .await
    };

    // Step 1: first `--name web` -> 201.
    let r = create_web(app.clone()).await;
    assert_eq!(r.status(), StatusCode::CREATED, "first --name web creates");
    let cid1 = to_body_json(r).await["Id"].as_str().unwrap().to_string();
    assert_eq!(app.inner.lock().await.containers[&cid1].name, "web");

    // Step 2: rm web (created state -> engine-free delete) -> 204; the name is freed.
    let r = crate::containers::containers_delete(
        State(app.clone()),
        Path("web".into()),
        Query::<crate::containers::DeleteQ>(serde_json::from_value(serde_json::json!({})).unwrap()),
    )
    .await;
    assert_eq!(r.status(), StatusCode::NO_CONTENT, "rm web succeeds");
    assert_eq!(
        app.inner.lock().await.containers.values().filter(|c| c.name == "web").count(),
        0,
        "no container named web after the rm"
    );

    // Step 3: create `--name web` AGAIN -> 201 (the freed name is reusable), a NEW distinct id.
    let r = create_web(app.clone()).await;
    assert_eq!(
        r.status(),
        StatusCode::CREATED,
        "a --name freed by rm must be reusable (no stale 409 reservation)"
    );
    let cid2 = to_body_json(r).await["Id"].as_str().unwrap().to_string();
    assert_ne!(cid1, cid2, "the recreated container is a fresh distinct id");
    let g = app.inner.lock().await;
    assert_eq!(
        g.containers.values().filter(|c| c.name == "web").count(),
        1,
        "exactly one container named web (the recreated one)"
    );
    assert!(g.containers.contains_key(&cid2), "the recreated web is present");
}
