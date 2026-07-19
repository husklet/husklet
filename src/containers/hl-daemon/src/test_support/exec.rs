//! exec create/inspect tests, archive missing-container 404 branches, and exec lifecycle flows.
use super::*;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;

// ---- 5. exec_create on paused -> 409 "is paused"; on exited -> 409 "is not running" -----------
#[tokio::test]
async fn exec_create_on_paused_is_409_is_paused() {
    let app = test_app();
    seed_container(&app, "c1", "paused").await;
    let body = axum::Json(serde_json::from_value(serde_json::json!({"Cmd":["ls"]})).unwrap());
    let resp = crate::containers::exec_create(State(app.clone()), Path("c1".into()), body).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = to_body_string(resp).await;
    assert!(body.contains("is paused"), "got: {body}");
    assert!(
        app.inner.lock().await.execs.is_empty(),
        "no exec must be recorded"
    );
}

#[tokio::test]
async fn exec_create_on_exited_is_409_not_running() {
    let app = test_app();
    seed_container(&app, "c1", "exited").await;
    let body = axum::Json(serde_json::from_value(serde_json::json!({"Cmd":["ls"]})).unwrap());
    let resp = crate::containers::exec_create(State(app.clone()), Path("c1".into()), body).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = to_body_string(resp).await;
    assert!(body.contains("is not running"), "got: {body}");
    assert!(app.inner.lock().await.execs.is_empty());
}

// ---- 17. exec_create on a RUNNING container -> 201 + exec recorded; empty cmd -> 400 ----------
#[tokio::test]
async fn exec_create_on_running_records_exec() {
    let app = test_app();
    seed_container(&app, "c1", "running").await;
    let body = axum::Json(serde_json::from_value(serde_json::json!({"Cmd":["ls","-la"]})).unwrap());
    let resp = crate::containers::exec_create(State(app.clone()), Path("c1".into()), body).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let v = to_body_json(resp).await;
    let exec_id = v["Id"]
        .as_str()
        .expect("exec create returns an Id")
        .to_string();
    assert!(!exec_id.is_empty());
    let g = app.inner.lock().await;
    let exec = g
        .execs
        .get(&exec_id)
        .expect("exec must be recorded under the returned Id");
    assert_eq!(exec.container_id, "c1");
    assert_eq!(exec.cmd, vec!["ls".to_string(), "-la".to_string()]);
    assert!(!exec.started, "a freshly created exec has not started");
}

#[tokio::test]
async fn exec_create_empty_cmd_is_400() {
    let app = test_app();
    seed_container(&app, "c1", "running").await;
    let body = axum::Json(serde_json::from_value(serde_json::json!({"Cmd":[]})).unwrap());
    let resp = crate::containers::exec_create(State(app.clone()), Path("c1".into()), body).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let msg = to_body_string(resp).await;
    assert!(msg.contains("No exec command specified"), "got: {msg}");
    assert!(
        app.inner.lock().await.execs.is_empty(),
        "no exec on a rejected create"
    );
}

// ---- 18. exec_inspect: docker exec-inspect JSON shape; missing -> 404 ------------------------
#[tokio::test]
async fn exec_inspect_wire_shape() {
    let app = test_app();
    seed_container(&app, "c1", "running").await;
    let body = axum::Json(
        serde_json::from_value(serde_json::json!({"Cmd":["ls","-la"],"Tty":true})).unwrap(),
    );
    let created = crate::containers::exec_create(State(app.clone()), Path("c1".into()), body).await;
    let exec_id = to_body_json(created).await["Id"]
        .as_str()
        .unwrap()
        .to_string();

    let resp = crate::containers::Execs::inspect(State(app.clone()), Path(exec_id.clone())).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v = to_body_json(resp).await;
    let obj = v.as_object().unwrap();
    for key in ["ID", "Running", "ExitCode", "ContainerID", "ProcessConfig"] {
        assert!(obj.contains_key(key), "exec inspect missing key {key}: {v}");
    }
    assert_eq!(v["ID"], exec_id);
    // No Live for this exec (start never ran) -> Running=false, ExitCode falls back to the record's 0.
    assert_eq!(v["Running"], false, "un-started exec is not Running");
    assert_eq!(v["ExitCode"], 0);
    assert_eq!(v["ContainerID"], "c1");
    // Nested ProcessConfig: docker's lowercase keys; entrypoint = argv[0], arguments = argv[1..].
    let pc = v["ProcessConfig"].as_object().unwrap();
    for key in ["tty", "privileged", "entrypoint", "arguments"] {
        assert!(pc.contains_key(key), "ProcessConfig missing key {key}");
    }
    assert_eq!(v["ProcessConfig"]["tty"], true, "Tty:true was recorded");
    assert_eq!(v["ProcessConfig"]["entrypoint"], "ls");
    assert_eq!(v["ProcessConfig"]["arguments"], serde_json::json!(["-la"]));
}

#[tokio::test]
async fn exec_inspect_missing_is_404() {
    let app = test_app();
    let resp = crate::containers::Execs::inspect(State(app.clone()), Path("noexec".into())).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let msg = to_body_string(resp).await;
    assert!(msg.contains("no such exec"), "got: {msg}");
}

// ---- 19. archive HEAD/GET on a MISSING container -> 404 (engine/fs-free branch only) ----------
#[tokio::test]
async fn archive_head_missing_container_is_404() {
    let app = test_app();
    let q: crate::archive::ArchiveQ =
        serde_json::from_value(serde_json::json!({"path": "/etc/hosts"})).unwrap();
    let resp =
        crate::archive::archive_head(State(app.clone()), Path("ghost".into()), Query(q)).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn archive_get_missing_container_is_404() {
    let app = test_app();
    let q: crate::archive::ArchiveQ =
        serde_json::from_value(serde_json::json!({"path": "/etc/hosts"})).unwrap();
    let resp =
        crate::archive::archive_get(State(app.clone()), Path("ghost".into()), Query(q)).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---- FLOW 10: exec lifecycle — two execs recorded distinctly; pre-start Running=false; 404 -----
// docker: each `exec create` mints a DISTINCT exec id (both persisted); `exec inspect` before any
// start reports Running=false; inspecting a bogus id is a 404. Drives create -> inspect -> create ->
// inspect(both) -> inspect(bogus). (exec_start hijacks/streams to the engine and is NOT driven.)
#[tokio::test]
async fn flow_exec_lifecycle_two_distinct_execs_prestart_and_404() {
    let app = test_app();
    seed_container(&app, "c1", "running").await;

    // Step 1: first exec_create -> 201 + id1.
    let r = crate::containers::exec_create(
        State(app.clone()),
        Path("c1".into()),
        axum::Json(serde_json::from_value(serde_json::json!({"Cmd":["ls"]})).unwrap()),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CREATED);
    let id1 = to_body_json(r).await["Id"].as_str().unwrap().to_string();

    // Step 2: inspect id1 BEFORE any start -> Running=false, ContainerID=c1.
    let r = crate::containers::Execs::inspect(State(app.clone()), Path(id1.clone())).await;
    assert_eq!(r.status(), StatusCode::OK);
    let v = to_body_json(r).await;
    assert_eq!(v["Running"], false, "un-started exec is not Running");
    assert_eq!(v["ContainerID"], "c1");

    // Step 3: a SECOND exec_create -> a DIFFERENT id; both are recorded.
    let r = crate::containers::exec_create(
        State(app.clone()),
        Path("c1".into()),
        axum::Json(serde_json::from_value(serde_json::json!({"Cmd":["pwd"]})).unwrap()),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CREATED);
    let id2 = to_body_json(r).await["Id"].as_str().unwrap().to_string();
    assert_ne!(id1, id2, "each exec create mints a distinct id");
    {
        let g = app.inner.lock().await;
        assert_eq!(g.execs.len(), 2, "both execs are recorded");
        assert_eq!(g.execs[&id1].cmd, vec!["ls".to_string()]);
        assert_eq!(g.execs[&id2].cmd, vec!["pwd".to_string()]);
    }

    // Step 4: both inspect cleanly and independently.
    for id in [&id1, &id2] {
        let r = crate::containers::Execs::inspect(State(app.clone()), Path(id.clone())).await;
        assert_eq!(r.status(), StatusCode::OK, "exec {id} inspects");
    }

    // Step 5: a bogus exec id is a 404.
    let r = crate::containers::Execs::inspect(State(app.clone()), Path("bogusexec".into())).await;
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
}

// ---- FLOW 18: exec whose container STOPS between create and inspect --------------------------
// docker: an `exec create` on a running container returns an id; if the container then stops, an
// `exec inspect` of that id still succeeds (Running=false); a FRESH `exec create` on the now-stopped
// container is a 409 ("is not running"). Drives create -> stop-the-container -> inspect -> create(409).
#[tokio::test]
async fn flow_exec_survives_container_stop_then_fresh_exec_409() {
    let app = test_app();
    seed_container(&app, "c1", "running").await;

    // Step 1: exec_create while running -> 201 + id.
    let r = crate::containers::exec_create(
        State(app.clone()),
        Path("c1".into()),
        axum::Json(serde_json::from_value(serde_json::json!({"Cmd":["sleep","1"]})).unwrap()),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CREATED);
    let exec_id = to_body_json(r).await["Id"].as_str().unwrap().to_string();

    // Step 2: the container stops (reaper writes back exited) out from under the recorded exec.
    set_status(&app, "c1", "exited").await;

    // Step 3: inspect the pre-existing exec id — still 200 with a sane shape (no panic/500). The exec
    // record outlives the container's running state; Running=false, ContainerID still points at c1.
    let r = crate::containers::Execs::inspect(State(app.clone()), Path(exec_id.clone())).await;
    assert_eq!(
        r.status(),
        StatusCode::OK,
        "inspecting a pre-created exec survives the container stop"
    );
    let v = to_body_json(r).await;
    assert_eq!(v["ID"], exec_id);
    assert_eq!(v["Running"], false, "the un-started exec is not Running");
    assert_eq!(v["ExitCode"], 0);
    assert_eq!(
        v["ContainerID"], "c1",
        "the exec still references its container"
    );
    assert_eq!(v["ProcessConfig"]["entrypoint"], "sleep");

    // Step 4: a FRESH exec_create on the now-exited container is a 409 ("is not running"); no record.
    let before = app.inner.lock().await.execs.len();
    let r = crate::containers::exec_create(
        State(app.clone()),
        Path("c1".into()),
        axum::Json(serde_json::from_value(serde_json::json!({"Cmd":["ls"]})).unwrap()),
    )
    .await;
    assert_eq!(
        r.status(),
        StatusCode::CONFLICT,
        "exec on a stopped container is 409"
    );
    let msg = to_body_string(r).await;
    assert!(
        msg.contains("is not running"),
        "409 body says not running: {msg}"
    );
    assert_eq!(
        app.inner.lock().await.execs.len(),
        before,
        "a rejected exec must NOT record a second exec"
    );
}

// ---- exec inspect full docker shape (CanRemove/OpenStd*/DetachKeys/Pid) ----------------------
#[tokio::test]
async fn exec_inspect_reports_full_docker_state_shape() {
    let app = test_app();
    seed_container(&app, "c1", "running").await;
    let created = crate::containers::exec_create(
        State(app.clone()),
        Path("c1".into()),
        axum::Json(serde_json::from_value(serde_json::json!({"Cmd":["ls"],"Tty":true})).unwrap()),
    )
    .await;
    let exec_id = to_body_json(created).await["Id"]
        .as_str()
        .unwrap()
        .to_string();
    let v =
        to_body_json(crate::containers::Execs::inspect(State(app.clone()), Path(exec_id)).await)
            .await;
    for key in [
        "CanRemove",
        "OpenStdin",
        "OpenStdout",
        "OpenStderr",
        "DetachKeys",
        "Pid",
    ] {
        assert!(
            v.as_object().unwrap().contains_key(key),
            "exec inspect missing {key}: {v}"
        );
    }
    assert_eq!(v["OpenStdout"], true);
    assert_eq!(v["CanRemove"], false);
}

// ---- exec_create emits a container exec_create event -----------------------------------------
#[tokio::test]
async fn exec_create_emits_exec_create_event() {
    let app = test_app();
    seed_container(&app, "c1", "running").await;
    let mut rx = app.events.subscribe();
    let _ = crate::containers::exec_create(
        State(app.clone()),
        Path("c1".into()),
        axum::Json(serde_json::from_value(serde_json::json!({"Cmd":["ls"]})).unwrap()),
    )
    .await;
    let mut saw = false;
    while let Ok(ev) = rx.try_recv() {
        if ev["Type"] == "container"
            && ev["Action"]
                .as_str()
                .unwrap_or("")
                .starts_with("exec_create")
        {
            saw = true;
        }
    }
    assert!(saw, "exec_create must emit a container exec_create event");
}

// ---- exec_start is single-use + rechecks parent state ----------------------------------------
#[tokio::test]
async fn exec_start_rejects_already_started_exec() {
    let app = test_app();
    seed_container(&app, "c1", "running").await;
    let created = crate::containers::exec_create(
        State(app.clone()),
        Path("c1".into()),
        axum::Json(serde_json::from_value(serde_json::json!({"Cmd":["sleep","5"]})).unwrap()),
    )
    .await;
    let exec_id = to_body_json(created).await["Id"]
        .as_str()
        .unwrap()
        .to_string();
    // Mark it already started (as a prior /start would).
    app.inner
        .lock()
        .await
        .execs
        .get_mut(&exec_id)
        .unwrap()
        .started = true;
    let req = axum::http::Request::builder()
        .body(axum::body::Body::from("{\"Detach\":true}"))
        .unwrap();
    let resp = crate::containers::exec_start(State(app.clone()), Path(exec_id), req).await;
    assert_eq!(
        resp.status(),
        StatusCode::CONFLICT,
        "a second start of one exec is 409"
    );
}

#[tokio::test]
async fn exec_start_rejects_when_parent_no_longer_running() {
    let app = test_app();
    seed_container(&app, "c1", "running").await;
    let created = crate::containers::exec_create(
        State(app.clone()),
        Path("c1".into()),
        axum::Json(serde_json::from_value(serde_json::json!({"Cmd":["ls"]})).unwrap()),
    )
    .await;
    let exec_id = to_body_json(created).await["Id"]
        .as_str()
        .unwrap()
        .to_string();
    // The parent stops between create and start.
    set_status(&app, "c1", "exited").await;
    let req = axum::http::Request::builder()
        .body(axum::body::Body::from("{\"Detach\":true}"))
        .unwrap();
    let resp = crate::containers::exec_start(State(app.clone()), Path(exec_id), req).await;
    assert_eq!(
        resp.status(),
        StatusCode::CONFLICT,
        "exec start on a stopped parent is 409"
    );
    let msg = to_body_string(resp).await;
    assert!(msg.contains("is not running"), "got: {msg}");
}
