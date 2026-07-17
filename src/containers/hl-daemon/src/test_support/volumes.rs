//! Volume create/delete/inspect tests + volume refcount / prune multi-step flows.
use super::*;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;

/// Directly insert a container that binds a volume by name (`-v <vol>:/data`) — the shape
/// `volume_in_use` scans. Kept minimal so a flow can assert refcount without going through create.
async fn seed_container_binding_volume(app: &App, id: &str, vol: &str) {
    let mut g = app.inner.lock().await;
    g.containers.insert(
        id.to_string(),
        Container {
            id: id.to_string(),
            status: "exited".into(),
            binds: vec![format!("{vol}:/data")],
            ..Default::default()
        },
    );
}

// ---- 8. volumes_create -> 201; volume_delete in-use -> 409; free -> 204 ----------------------
#[tokio::test]
async fn volume_create_is_201_and_present() {
    let app = test_app();
    let body = axum::Json(serde_json::from_value(serde_json::json!({"Name":"vol1"})).unwrap());
    let resp = crate::volumes::volumes_create(State(app.clone()), body).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    assert!(app
        .inner
        .lock()
        .await
        .volumes
        .iter()
        .any(|v| v.name == "vol1"));
}

#[tokio::test]
async fn volume_delete_in_use_is_409() {
    let app = test_app();
    seed_volume(&app, "vol1", /*in_use=*/ true).await;
    let resp = crate::volumes::volume_delete(State(app.clone()), Path("vol1".into())).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    assert!(
        app.inner
            .lock()
            .await
            .volumes
            .iter()
            .any(|v| v.name == "vol1"),
        "in-use volume must remain"
    );
}

#[tokio::test]
async fn volume_delete_free_is_204_and_removed() {
    let app = test_app();
    seed_volume(&app, "vol1", /*in_use=*/ false).await;
    let resp = crate::volumes::volume_delete(State(app.clone()), Path("vol1".into())).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert!(
        !app.inner
            .lock()
            .await
            .volumes
            .iter()
            .any(|v| v.name == "vol1"),
        "free volume must be removed"
    );
}

// ---- 14. volume_inspect: object key fields; missing -> 404 ----------------------------------
#[tokio::test]
async fn volume_inspect_wire_shape() {
    let app = test_app();
    seed_volume(&app, "vol1", /*in_use=*/ false).await;
    let resp = crate::volumes::volume_inspect(State(app.clone()), Path("vol1".into()))
        .await
        .into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = to_body_json(resp).await;
    for key in [
        "Name",
        "Driver",
        "Mountpoint",
        "CreatedAt",
        "Scope",
        "Labels",
        "Options",
    ] {
        assert!(
            v.as_object().unwrap().contains_key(key),
            "volume missing key {key}"
        );
    }
    assert_eq!(v["Name"], "vol1");
    assert_eq!(v["Driver"], "local");
    assert_eq!(v["Mountpoint"], "/mp/vol1");
    assert_eq!(v["Scope"], "local");
}

#[tokio::test]
async fn volume_inspect_missing_is_404() {
    let app = test_app();
    let resp = crate::volumes::volume_inspect(State(app.clone()), Path("ghost".into()))
        .await
        .into_response();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---- FLOW 2: volume in-use refcount, 409-while-bound → 204-once-free --------------------------
// docker: removing a volume a container still references is a 409 ("volume is in use"); once no
// container references it, remove is a 204.
#[tokio::test]
async fn flow_volume_in_use_then_free() {
    let app = test_app();

    // Step 1: create v1 — 201, present.
    let body = axum::Json(serde_json::from_value(serde_json::json!({"Name":"v1"})).unwrap());
    let r = crate::volumes::volumes_create(State(app.clone()), body).await;
    assert_eq!(r.status(), StatusCode::CREATED);
    assert!(app
        .inner
        .lock()
        .await
        .volumes
        .iter()
        .any(|v| v.name == "v1"));

    // Step 2: a container binds v1 by name.
    seed_container_binding_volume(&app, "user1", "v1").await;

    // Step 3: delete while bound — 409, volume survives.
    let r = crate::volumes::volume_delete(State(app.clone()), Path("v1".into())).await;
    assert_eq!(
        r.status(),
        StatusCode::CONFLICT,
        "in-use volume delete is 409"
    );
    assert!(
        app.inner
            .lock()
            .await
            .volumes
            .iter()
            .any(|v| v.name == "v1"),
        "a refused (in-use) delete must not drop the volume"
    );

    // Step 4: drop the referencing container.
    app.inner.lock().await.containers.remove("user1");

    // Step 5: delete now that nothing binds it — 204, volume removed.
    let r = crate::volumes::volume_delete(State(app.clone()), Path("v1".into())).await;
    assert_eq!(
        r.status(),
        StatusCode::NO_CONTENT,
        "freed volume delete is 204"
    );
    assert!(
        !app.inner
            .lock()
            .await
            .volumes
            .iter()
            .any(|v| v.name == "v1"),
        "volume gone once its last reference is removed"
    );
}

// ---- FLOW 14: volume prune selectivity — in-use kept, free reclaimed; free the user, prune again -
// docker: `volume prune` reclaims ONLY volumes no container references; an in-use volume is kept; once
// its last referencing container is gone, a second prune reclaims it. Drives create x2 -> bind v1 ->
// prune -> drop user -> prune.
#[tokio::test]
async fn flow_volume_prune_in_use_kept_then_reclaimed() {
    let app = test_app();
    for name in ["v1", "v2"] {
        let r = crate::volumes::volumes_create(
            State(app.clone()),
            axum::Json(serde_json::from_value(serde_json::json!({ "Name": name })).unwrap()),
        )
        .await;
        assert_eq!(r.status(), StatusCode::CREATED);
    }
    // A container binds v1 by name; v2 is free.
    seed_container_binding_volume(&app, "user1", "v1").await;

    // Prune #1: only v2 reclaimed; v1 kept (in use).
    let axum::Json(rep) = crate::volumes::volumes_prune(State(app.clone())).await;
    let pruned: std::collections::HashSet<&str> =
        rep.volumes_deleted.iter().map(|s| s.as_str()).collect();
    assert!(pruned.contains("v2"), "free v2 reclaimed: {pruned:?}");
    assert!(!pruned.contains("v1"), "in-use v1 kept: {pruned:?}");
    {
        let g = app.inner.lock().await;
        assert!(
            g.volumes.iter().any(|v| v.name == "v1"),
            "v1 survives prune #1"
        );
        assert!(
            !g.volumes.iter().any(|v| v.name == "v2"),
            "v2 gone after prune #1"
        );
    }

    // Drop the referencing container, then prune #2: v1 now reclaimed.
    app.inner.lock().await.containers.remove("user1");
    let axum::Json(rep) = crate::volumes::volumes_prune(State(app.clone())).await;
    assert!(
        rep.volumes_deleted.iter().any(|s| s == "v1"),
        "v1 reclaimed once its last reference is gone: {:?}",
        rep.volumes_deleted
    );
    assert!(
        !app.inner
            .lock()
            .await
            .volumes
            .iter()
            .any(|v| v.name == "v1"),
        "v1 removed after prune #2"
    );
}

// ---- FLOW 19: `volume prune` keeps a still-referenced ANON volume, reclaims it after `rm` -------
// docker: `volume prune` (no filter) reclaims UNUSED volumes including dangling anonymous ones, but
// NEVER one still referenced by a container — even a STOPPED one. Drives create(-v /data) -> stop ->
// prune(kept) -> rm(no -v) -> prune(reclaimed). The anon vol survives while the container exists.
#[tokio::test]
async fn flow_volume_prune_keeps_referenced_anon_reclaims_after_rm() {
    let app = test_app();
    seed_image_rootfs(&app, "alpine", "/store/alpine-R").await;

    // Create a container with a bare `-v /data` anonymous volume, then stop it (exited).
    let r = crate::containers::containers_create(
        State(app.clone()),
        Query(create_q(serde_json::json!({}))),
        create_body(serde_json::json!({
            "Image":"alpine",
            "HostConfig": {"Binds": ["/data"]}
        })),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CREATED);
    let cid = to_body_json(r).await["Id"].as_str().unwrap().to_string();
    let anon = {
        let g = app.inner.lock().await;
        let c = &g.containers[&cid];
        assert_eq!(
            c.anon_volumes.len(),
            1,
            "a bare -v /data yields one anon volume"
        );
        c.anon_volumes[0].clone()
    };
    set_status(&app, &cid, "exited").await;
    assert!(
        app.inner
            .lock()
            .await
            .volumes
            .iter()
            .any(|v| v.name == anon),
        "the anon volume is registered in the store"
    );

    // Prune #1 while a (stopped) container still references the anon volume — it is KEPT.
    let axum::Json(rep) = crate::volumes::volumes_prune(State(app.clone())).await;
    assert!(
        !rep.volumes_deleted.iter().any(|s| s == &anon),
        "a still-referenced anon volume must NOT be pruned (even for a stopped container): {:?}",
        rep.volumes_deleted
    );
    assert!(
        app.inner
            .lock()
            .await
            .volumes
            .iter()
            .any(|v| v.name == anon),
        "the anon volume survives prune #1 while its container exists"
    );

    // Plain `rm` (no -v): the container goes, the anon volume is LEFT behind (now dangling/unused).
    let r = crate::containers::containers_delete(
        State(app.clone()),
        Path(cid.clone()),
        Query::<crate::containers::DeleteQ>(serde_json::from_value(serde_json::json!({})).unwrap()),
    )
    .await;
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
    assert!(
        app.inner
            .lock()
            .await
            .volumes
            .iter()
            .any(|v| v.name == anon),
        "plain rm (no -v) leaves the anon volume dangling — still present pre-prune"
    );

    // Prune #2: now nothing references it, so the dangling anon volume is reclaimed.
    let axum::Json(rep) = crate::volumes::volumes_prune(State(app.clone())).await;
    assert!(
        rep.volumes_deleted.iter().any(|s| s == &anon),
        "the now-unreferenced dangling anon volume is reclaimed by prune #2: {:?}",
        rep.volumes_deleted
    );
    assert!(
        !app.inner
            .lock()
            .await
            .volumes
            .iter()
            .any(|v| v.name == anon),
        "anon volume gone after prune #2"
    );
}
