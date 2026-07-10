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

    // Step 5: update with a Memory limit -> 200, and the new limit is now PERSISTED (inspect reflects it).
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
        v["HostConfig"]["Memory"], 16000000,
        "docker update now persists the Memory limit and inspect reflects it"
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

// ---- docker logs --since/--until filters PER CHUNK, not per coalesced run --------------------
// Regression: the replay coalesced all adjacent same-stream chunks into one run keeping only the
// FIRST chunk's timestamp, then applied --since/--until to that single ts — so a busy single-stream
// container (all output in one coalesced run stamped at the first write) returned everything or
// nothing. The window must be applied to each chunk's own emit time before coalescing.
#[tokio::test]
async fn logs_since_until_filter_per_chunk_not_per_coalesced_run() {
    async fn body_bytes(resp: axum::response::Response) -> Vec<u8> {
        axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec()
    }
    let has = |b: &[u8], needle: &[u8]| b.windows(needle.len()).any(|w| w == needle);

    let app = test_app();
    seed_container(&app, "c1", "exited").await;
    // two stdout writes at t=100 ("early") and t=200 ("late") — the same-stream, different-time case.
    {
        let live = Live::new();
        live.log_chunks
            .lock()
            .await
            .extend([(100i64, 1u8, b"early\n".to_vec()), (200i64, 1u8, b"late\n".to_vec())]);
        app.inner.lock().await.live.insert("c1".into(), live);
    }

    // --since 150: keep only the t=200 line (before the fix, the whole run stamped t=100 was dropped).
    let resp = crate::containers::containers_logs(
        State(app.clone()),
        Path("c1".into()),
        Query(serde_json::from_value(serde_json::json!({ "since": "150" })).unwrap()),
    )
    .await;
    let body = body_bytes(resp).await;
    assert!(has(&body, b"late"), "since=150 must keep the t=200 line");
    assert!(!has(&body, b"early"), "since=150 must drop the t=100 line");

    // --until 150: keep only the t=100 line (before the fix, the whole run was kept, leaking "late").
    let resp = crate::containers::containers_logs(
        State(app.clone()),
        Path("c1".into()),
        Query(serde_json::from_value(serde_json::json!({ "until": "150" })).unwrap()),
    )
    .await;
    let body = body_bytes(resp).await;
    assert!(has(&body, b"early"), "until=150 must keep the t=100 line");
    assert!(!has(&body, b"late"), "until=150 must drop the t=200 line");
}

// ---- Inspect round-trip cluster: Config/HostConfig/NetworkSettings fidelity ------------------
// docker create accepts a large set of Config/HostConfig fields that inspect must round-trip. These
// were silently dropped (AutoRemove, split Entrypoint/Cmd, WorkingDir/User/Domainname, Tty/OpenStdin/
// StdinOnce, ExposedPorts, LogConfig, Dns/DnsSearch/DnsOptions/ExtraHosts, DeviceRequests, NetworkMode,
// bind propagation, inherited image labels/env dedup). One create+inspect asserts they all survive.
async fn seed_rich_image(app: &App) {
    let mut g = app.inner.lock().await;
    g.images.push(Image {
        name: "richimg".into(),
        rootfs: "/store/rich".into(),
        entrypoint: vec!["/entry".into()],
        cmd: vec!["imgcmd".into()],
        env: vec!["FOO=image".into(), "BAR=base".into()],
        workdir: "/img/wd".into(),
        user: "imguser".into(),
        exposed_ports: vec!["5432/tcp".into()],
        labels: std::collections::HashMap::from([
            ("com.example.inherited".to_string(), "yes".to_string()),
            ("over".to_string(), "image".to_string()),
        ]),
        ..Default::default()
    });
}

#[tokio::test]
async fn container_inspect_round_trips_full_create_config() {
    let app = test_app();
    seed_rich_image(&app).await;
    let q: crate::containers::CreateQ =
        serde_json::from_value(serde_json::json!({"name":"rich"})).unwrap();
    let body = axum::Json(
        serde_json::from_value(serde_json::json!({
            "Image": "richimg",
            "Entrypoint": ["/entry"],
            "Cmd": ["--serve"],
            "Domainname": "example.test",
            "WorkingDir": "/srv/app",
            "User": "1001:1002",
            "Tty": true,
            "OpenStdin": true,
            "StdinOnce": true,
            "Env": ["FOO=run"],
            "ExposedPorts": {"8080/tcp": {}},
            "Labels": {"over": "run"},
            "HostConfig": {
                "NetworkMode": "none",
                "AutoRemove": true,
                "LogConfig": {"Type": "json-file", "Config": {"max-size": "10m"}},
                "Dns": ["9.9.9.9"],
                "DnsSearch": ["corp.test"],
                "DnsOptions": ["ndots:2"],
                "ExtraHosts": ["db:10.9.0.2"],
                "DeviceRequests": [{"Driver": "nvidia", "Count": -1}],
                "Mounts": [{"Type": "bind", "Source": "/h", "Target": "/c",
                            "BindOptions": {"Propagation": "rshared"}}]
            }
        }))
        .unwrap(),
    );
    let r = crate::containers::containers_create(State(app.clone()), Query(q), body).await;
    assert_eq!(r.status(), StatusCode::CREATED);

    let resp = crate::containers::containers_inspect(State(app.clone()), Path("rich".into()))
        .await
        .into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = to_body_json(resp).await;
    let cfg = &v["Config"];
    // Entrypoint/Cmd split (not collapsed into one argv).
    assert_eq!(cfg["Entrypoint"], serde_json::json!(["/entry"]));
    assert_eq!(cfg["Cmd"], serde_json::json!(["--serve"]));
    assert_eq!(cfg["WorkingDir"], "/srv/app");
    assert_eq!(cfg["User"], "1001:1002");
    assert_eq!(cfg["Domainname"], "example.test");
    assert_eq!(cfg["Tty"], true);
    assert_eq!(cfg["OpenStdin"], true);
    assert_eq!(cfg["StdinOnce"], true);
    // ExposedPorts merges image EXPOSE (5432) and create-body (8080), reported as `{}` values.
    assert!(cfg["ExposedPorts"]["8080/tcp"].is_object());
    assert!(cfg["ExposedPorts"]["5432/tcp"].is_object());
    // Env dedups last-wins: image FOO=image is replaced by run FOO=run (exactly one FOO=).
    let envs: Vec<String> = cfg["Env"].as_array().unwrap().iter()
        .map(|e| e.as_str().unwrap().to_string()).collect();
    assert_eq!(envs.iter().filter(|e| e.starts_with("FOO=")).count(), 1, "one FOO= after dedup");
    assert!(envs.contains(&"FOO=run".to_string()));
    assert!(envs.contains(&"BAR=base".to_string()));
    // Inherited image label survives; create-body label overrides same key.
    assert_eq!(cfg["Labels"]["com.example.inherited"], "yes");
    assert_eq!(cfg["Labels"]["over"], "run");

    let hc = &v["HostConfig"];
    assert_eq!(hc["NetworkMode"], "none");
    assert_eq!(hc["AutoRemove"], true);
    assert_eq!(hc["LogConfig"]["Type"], "json-file");
    assert_eq!(hc["LogConfig"]["Config"]["max-size"], "10m");
    assert_eq!(hc["Dns"], serde_json::json!(["9.9.9.9"]));
    assert_eq!(hc["DnsSearch"], serde_json::json!(["corp.test"]));
    assert_eq!(hc["DnsOptions"], serde_json::json!(["ndots:2"]));
    assert_eq!(hc["ExtraHosts"], serde_json::json!(["db:10.9.0.2"]));
    assert_eq!(hc["DeviceRequests"][0]["Driver"], "nvidia");
    // Bind propagation round-trips through HostConfig.Mounts[].BindOptions.
    assert_eq!(hc["Mounts"][0]["BindOptions"]["Propagation"], "rshared");
    // Exposed-but-unpublished ports appear as null bindings in NetworkSettings.Ports.
    assert!(v["NetworkSettings"]["Ports"].as_object().unwrap().contains_key("8080/tcp"));
}

#[tokio::test]
async fn container_create_honors_endpoint_static_ip_and_aliases() {
    let app = test_app();
    seed_image_rootfs(&app, "alpine", "/store/alpine").await;
    // A user network the container will attach to with a static IP + aliases.
    {
        let mut g = app.inner.lock().await;
        g.networks.push(Net {
            id: "net-frontend".into(),
            name: "frontend".into(),
            driver: "bridge".into(),
            scope: "local".into(),
            containers: vec![],
            created: 0,
            subnet: "172.18.0.0/16".into(),
            gateway: "172.18.0.1".into(),
            endpoints: std::collections::HashMap::new(),
        });
    }
    let q: crate::containers::CreateQ =
        serde_json::from_value(serde_json::json!({"name":"web"})).unwrap();
    let body = axum::Json(
        serde_json::from_value(serde_json::json!({
            "Image": "alpine",
            "HostConfig": {"NetworkMode": "frontend"},
            "NetworkingConfig": {"EndpointsConfig": {"frontend": {
                "IPAMConfig": {"IPv4Address": "172.18.0.77"},
                "Aliases": ["web", "api"]
            }}}
        }))
        .unwrap(),
    );
    let r = crate::containers::containers_create(State(app.clone()), Query(q), body).await;
    assert_eq!(r.status(), StatusCode::CREATED);
    let g = app.inner.lock().await;
    let net = g.networks.iter().find(|n| n.name == "frontend").unwrap();
    let ep = net.endpoints.values().next().expect("endpoint recorded");
    assert_eq!(ep.ip, "172.18.0.77", "requested static IP honored");
    assert!(ep.aliases.contains(&"web".to_string()));
    assert!(ep.aliases.contains(&"api".to_string()));
}

// ---- docker top on a stopped container is 409 ------------------------------------------------
#[tokio::test]
async fn top_on_stopped_container_is_409() {
    let app = test_app();
    seed_container(&app, "stopped00000", "exited").await;
    let resp = crate::containers::containers_top(State(app.clone()), Path("stopped00000".into())).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT, "top on a stopped container is a conflict");
    // A running container still returns the synthetic process row (200).
    seed_container(&app, "running00000", "running").await;
    let ok = crate::containers::containers_top(State(app.clone()), Path("running00000".into())).await;
    assert_eq!(ok.status(), StatusCode::OK);
}

// ---- inspect Dead boolean agrees with a dead status ------------------------------------------
#[tokio::test]
async fn inspect_dead_status_sets_dead_boolean() {
    let app = test_app();
    seed_container(&app, "deadone00000", "dead").await;
    let v = to_body_json(
        crate::containers::containers_inspect(State(app.clone()), Path("deadone00000".into()))
            .await
            .into_response(),
    )
    .await;
    assert_eq!(v["State"]["Status"], "dead");
    assert_eq!(v["State"]["Dead"], true, "State.Dead must agree with a dead status");
}

// ---- wait on a created container blocks until it exits (no fake StatusCode:0) -----------------
#[tokio::test]
async fn wait_on_created_container_blocks_until_exit() {
    let app = test_app();
    seed_container(&app, "waitcreated0", "created").await;
    let waitq: crate::containers::WaitQ = serde_json::from_value(serde_json::json!({})).unwrap();
    let app2 = app.clone();
    let handle = tokio::spawn(async move {
        let resp = crate::containers::containers_wait(State(app2), Path("waitcreated0".into()), Query(waitq)).await;
        axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap().to_vec()
    });
    // It must NOT complete while the container is still `created`.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert!(!handle.is_finished(), "wait must block a created container, not return immediately");
    // Now the container exits with code 7 -> wait completes with that code.
    {
        let mut g = app.inner.lock().await;
        let c = g.containers.get_mut("waitcreated0").unwrap();
        c.status = "exited".into();
        c.exit_code = 7;
    }
    let body = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await
        .expect("wait completes once exited").unwrap();
    assert_eq!(String::from_utf8_lossy(&body).trim(), "{\"StatusCode\":7}");
}

// ---- wait condition=removed does not complete while the container still exists ----------------
#[tokio::test]
async fn wait_condition_removed_blocks_until_removed() {
    let app = test_app();
    seed_container(&app, "waitremoved0", "exited").await;
    let waitq: crate::containers::WaitQ =
        serde_json::from_value(serde_json::json!({"condition":"removed"})).unwrap();
    let app2 = app.clone();
    let handle = tokio::spawn(async move {
        let resp = crate::containers::containers_wait(State(app2), Path("waitremoved0".into()), Query(waitq)).await;
        axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap().to_vec()
    });
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert!(!handle.is_finished(), "condition=removed must not complete while the container exists");
    // Remove it -> wait completes.
    app.inner.lock().await.containers.remove("waitremoved0");
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await
        .expect("wait completes once the container is removed");
}

// ---- failed spawn persists the terminal exit state (survives a daemon restart) ----------------
// live_fail is the failed-spawn path (no engine needed): it must mark the container exited/127 AND
// persist that, so a reload doesn't resurrect it as running. Engine-free: we call live_fail directly.
#[tokio::test]
async fn failed_spawn_persists_terminal_exit_state() {
    let app = test_app();
    seed_container(&app, "failspawn000", "running").await;
    let live = crate::model::Live::new();
    let ok = crate::runtime::live_fail(&app, "failspawn000", &live, "boom".into()).await;
    assert!(!ok, "live_fail returns false (spawn failed)");
    let g = app.inner.lock().await;
    let c = g.containers.get("failspawn000").unwrap();
    assert_eq!(c.status, "exited", "failed spawn marks the container exited");
    assert_eq!(c.exit_code, 127);
    // The persisted state file exists and reflects the terminal status (not `running`).
    let raw = std::fs::read_to_string(&app.state_path).expect("state persisted");
    let saved: serde_json::Value = serde_json::from_str(&raw).expect("state is valid JSON");
    let status = saved["containers"][0]["status"].as_str().unwrap_or("");
    assert_eq!(status, "exited", "terminal state must be durably saved, not left running");
}

// ---- durability/atomicity: create rollback + missing-network 404 -----------------------------
/// An app whose `state_path` points at an existing directory, so `save_state_checked` (write temp +
/// rename ONTO a dir) always fails — used to exercise the durable-save failure/rollback paths.
fn app_with_failing_state() -> App {
    let app = test_app();
    App { state_path: app.volumes_dir.clone(), ..app } // volumes_dir is a dir -> rename onto it fails
}

#[tokio::test]
async fn create_with_missing_network_is_404_and_records_nothing() {
    let app = test_app();
    seed_image_rootfs(&app, "alpine", "/store/alpine-R").await;
    let q: crate::containers::CreateQ = serde_json::from_value(serde_json::json!({})).unwrap();
    let body = axum::Json(
        serde_json::from_value(serde_json::json!({
            "Image": "alpine",
            "HostConfig": {"NetworkMode": "does-not-exist"}
        }))
        .unwrap(),
    );
    let mut rx = app.events.subscribe();
    let r = crate::containers::containers_create(State(app.clone()), Query(q), body).await;
    assert_eq!(r.status(), StatusCode::NOT_FOUND, "create on a missing network is 404");
    let g = app.inner.lock().await;
    assert!(g.containers.is_empty(), "no partial container is recorded");
    assert!(g.volumes.is_empty(), "no partial volumes are recorded");
    assert!(rx.try_recv().is_err(), "no create/volume event emitted on the failed create");
}

#[tokio::test]
async fn create_rolls_back_and_500s_when_state_cannot_persist() {
    let app = app_with_failing_state();
    seed_image_rootfs(&app, "alpine", "/store/alpine-R").await;
    let q: crate::containers::CreateQ = serde_json::from_value(serde_json::json!({})).unwrap();
    let body = axum::Json(serde_json::from_value(serde_json::json!({"Image":"alpine"})).unwrap());
    let mut rx = app.events.subscribe();
    let r = crate::containers::containers_create(State(app.clone()), Query(q), body).await;
    assert_eq!(r.status(), StatusCode::INTERNAL_SERVER_ERROR, "a failed durable save fails the create");
    assert!(app.inner.lock().await.containers.is_empty(), "the partial container is rolled back");
    // No container/create event before durable state.
    let mut saw_create = false;
    while let Ok(ev) = rx.try_recv() {
        if ev["Type"] == "container" && ev["Action"] == "create" { saw_create = true; }
    }
    assert!(!saw_create, "no container/create event when the state save failed");
}

// ---- failed spawn removes the spent Live so a retry re-spawns (not a stale 204 no-op) ---------
#[tokio::test]
async fn failed_spawn_removes_spent_live_entry() {
    let app = test_app();
    seed_container(&app, "spent0000000", "running").await;
    let live = crate::model::Live::new();
    // Simulate what containers_start does: install the Live under the container id, mark it started.
    app.inner.lock().await.live.insert("spent0000000".into(), live.clone());
    live.started.store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = crate::runtime::live_fail(&app, "spent0000000", &live, "boom".into()).await;
    assert!(
        !app.inner.lock().await.live.contains_key("spent0000000"),
        "a failed spawn must drop the spent Live so a second start creates a fresh one"
    );
}

// ---- health `starting` state is installed synchronously at start --------------------------------
#[tokio::test]
async fn start_installs_starting_health_state_synchronously() {
    let app = test_app();
    seed_container(&app, "healthstart0", "created").await;
    // Give it a healthcheck (as create would resolve).
    {
        let mut g = app.inner.lock().await;
        let c = g.containers.get_mut("healthstart0").unwrap();
        c.healthcheck = Some(crate::model::HealthConfig {
            test: vec!["CMD-SHELL".into(), "true".into()],
            ..Default::default()
        });
    }
    // start spawns (which fails with no engine and reaps to exited), but the `starting` health object is
    // installed BEFORE spawn as part of the start transition, so it is present afterward.
    let _ = crate::containers::containers_start(State(app.clone()), Path("healthstart0".into())).await;
    let g = app.inner.lock().await;
    let c = g.containers.get("healthstart0").unwrap();
    let h = c.health.as_ref().expect("health object installed at start");
    assert_eq!(h.status, "starting", "health must be visible as starting from the start transition");
}

// ---- create rejects an image whose (store-managed) rootfs has disappeared --------------------
#[tokio::test]
async fn create_rejects_image_with_missing_store_rootfs() {
    let app = test_app();
    // An image whose rootfs is UNDER images_dir but does not exist on disk (store entry vanished).
    let gone = std::path::Path::new(&app.images_dir).join("gone/rootfs").to_string_lossy().into_owned();
    {
        let mut g = app.inner.lock().await;
        g.images.push(Image { name: "ghost:1".into(), rootfs: gone, ..Default::default() });
    }
    let q: crate::containers::CreateQ = serde_json::from_value(serde_json::json!({})).unwrap();
    let body = create_body(serde_json::json!({"Image":"ghost:1"}));
    let mut rx = app.events.subscribe();
    let r = crate::containers::containers_create(State(app.clone()), Query(q), body).await;
    assert_eq!(r.status(), StatusCode::NOT_FOUND, "create on a missing store rootfs is 404");
    assert!(app.inner.lock().await.containers.is_empty(), "no container recorded");
    assert!(rx.try_recv().is_err(), "no create event on the rejected create");
}

// ---- create fails when the volumes root can't host anonymous volumes -------------------------
#[tokio::test]
async fn create_fails_when_anon_volume_root_is_unusable() {
    let app0 = test_app();
    // Point the volumes root at a FILE so anon-volume dirs can't be created.
    let volfile = std::path::Path::new(&app0.images_dir).join("not-a-dir");
    std::fs::write(&volfile, b"x").unwrap();
    let app = App { volumes_dir: volfile.to_string_lossy().into_owned(), ..app0 };
    // An image with an image VOLUME so create must materialize an anon volume.
    {
        let mut g = app.inner.lock().await;
        g.images.push(Image { name: "voly:1".into(), rootfs: "/store/voly-R".into(),
            img_volumes: vec!["/data".into()], ..Default::default() });
    }
    let q: crate::containers::CreateQ = serde_json::from_value(serde_json::json!({})).unwrap();
    let body = create_body(serde_json::json!({"Image":"voly:1"}));
    let r = crate::containers::containers_create(State(app.clone()), Query(q), body).await;
    assert_eq!(r.status(), StatusCode::INTERNAL_SERVER_ERROR, "unusable volumes root fails the create");
    let g = app.inner.lock().await;
    assert!(g.containers.is_empty(), "no container recorded");
    assert!(g.volumes.is_empty(), "no phantom volume recorded");
}
