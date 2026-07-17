//! Cross-resource multi-step flows that span more than one domain (container + network + volume):
//! the interactions no single-resource test can see (create-joins-net/rm-leaves, anon-volume GC on
//! rm, and a named-volume + network teardown). Same rules as the per-domain flows: engine-free
//! handlers only; state asserted after every step; docker's contract stated inline, hl's asserted.
use super::*;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;

// ---- FLOW 6: container joins a net on create, and is DROPPED from it on rm (leave_network) ----
// docker: `run --network mynet` adds the container to the net's membership; `rm` removes that
// endpoint. Exercises create's join + delete's leave in one shared state — an interaction unit
// tests miss (a container could be left dangling in a net's membership after removal).
#[tokio::test]
async fn flow_container_network_membership_join_on_create_leave_on_rm() {
    let app = test_app();
    seed_image_rootfs(&app, "alpine", "/store/alpine-rootfs").await;
    crate::networks::networks_create(State(app.clone()), net_create_body("mynet")).await;

    // Create a container attached to `mynet` (NetworkMode).
    let q: crate::containers::CreateQ = serde_json::from_value(serde_json::json!({})).unwrap();
    let body = axum::Json(
        serde_json::from_value(serde_json::json!({
            "Image":"alpine",
            "HostConfig": {"NetworkMode":"mynet"}
        }))
        .unwrap(),
    );
    let r = crate::containers::containers_create(State(app.clone()), Query(q), body).await;
    assert_eq!(r.status(), StatusCode::CREATED);
    let cid = to_body_json(r).await["Id"].as_str().unwrap().to_string();

    // The container is now a member of `mynet`.
    assert!(
        net_members(&app, "mynet").await.contains(&cid),
        "create --network mynet must add the container to the net membership"
    );
    assert_eq!(
        net_endpoint_count(&app, "mynet").await,
        1,
        "endpoint allocated on create"
    );

    // Remove the container (created state -> engine-free delete).
    let dq: crate::containers::DeleteQ = serde_json::from_value(serde_json::json!({})).unwrap();
    let r = crate::containers::containers_delete(State(app.clone()), Path(cid.clone()), Query(dq))
        .await;
    assert_eq!(r.status(), StatusCode::NO_CONTENT);

    // rm must drop the endpoint from the net (no dangling membership).
    assert!(
        !net_members(&app, "mynet").await.contains(&cid),
        "rm must remove the container from the net membership"
    );
    assert_eq!(
        net_endpoint_count(&app, "mynet").await,
        0,
        "rm must free the endpoint too — the net is now empty and deletable"
    );
    // And the now-empty net is deletable (204), proving the leave was a true refcount decrement.
    let r = crate::networks::network_delete(State(app.clone()), Path("mynet".into())).await;
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
}

// ---- FLOW 7: anonymous volume is GC'd by `rm -v` but survives a plain `rm` (the leak fix) ------
// docker: an anonymous volume (bare `-v /data`) is reclaimed by `rm -v`, but a plain `rm` (no -v)
// leaves it behind (Moby removes only anonymous volumes, and only on -v). This is the exact class
// of the just-fixed anon-volume-leak-on-`--rm`; drive create -> rm both ways and assert the volume
// store transitions.
#[tokio::test]
async fn flow_anon_volume_reclaimed_by_rm_v_but_kept_by_plain_rm() {
    let app = test_app();
    seed_image_rootfs(&app, "alpine", "/store/alpine-rootfs").await;

    // Helper: create a container with a bare `-v /data` anonymous volume, return (cid, anon_name).
    async fn create_with_anon(app: &App) -> (String, String) {
        let q: crate::containers::CreateQ = serde_json::from_value(serde_json::json!({})).unwrap();
        let body = axum::Json(
            serde_json::from_value(serde_json::json!({
                "Image":"alpine",
                "HostConfig": {"Binds": ["/data"]}
            }))
            .unwrap(),
        );
        let r = crate::containers::containers_create(State(app.clone()), Query(q), body).await;
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
        // The anon volume is registered in the volume store.
        assert!(
            app.inner
                .lock()
                .await
                .volumes
                .iter()
                .any(|v| v.name == anon),
            "anon volume must be present in the store after create"
        );
        (cid, anon)
    }

    // Case A: plain `rm` (no -v) — the anonymous volume SURVIVES (docker keeps it).
    let (cid_a, anon_a) = create_with_anon(&app).await;
    let dq: crate::containers::DeleteQ = serde_json::from_value(serde_json::json!({})).unwrap();
    let r = crate::containers::containers_delete(State(app.clone()), Path(cid_a), Query(dq)).await;
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
    assert!(
        app.inner
            .lock()
            .await
            .volumes
            .iter()
            .any(|v| v.name == anon_a),
        "plain rm (no -v) must LEAVE the anonymous volume behind"
    );

    // Case B: `rm -v` — the anonymous volume is RECLAIMED (the leak fix).
    let (cid_b, anon_b) = create_with_anon(&app).await;
    let dq: crate::containers::DeleteQ =
        serde_json::from_value(serde_json::json!({"v":"true"})).unwrap();
    let r = crate::containers::containers_delete(State(app.clone()), Path(cid_b), Query(dq)).await;
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
    assert!(
        !app.inner
            .lock()
            .await
            .volumes
            .iter()
            .any(|v| v.name == anon_b),
        "rm -v must reclaim the container's anonymous volume (no leak)"
    );
}

// ---- FLOW 13: cross-resource teardown — named volume + network, then `rm` (no -v) --------------
// docker: `run -v myvol:/data --network mynet` binds a NAMED volume and joins the net; a plain `rm`
// (no -v) frees the network endpoint but KEEPS the named volume (docker removes only ANONYMOUS
// volumes, and only on `-v`). Drives create -> assert both refs -> rm -> assert endpoint freed +
// named volume kept + volume now deletable.
#[tokio::test]
async fn flow_teardown_named_volume_kept_network_endpoint_freed_on_rm() {
    let app = test_app();
    seed_image_rootfs(&app, "alpine", "/store/alpine-R").await;
    // A pre-existing NAMED volume and a user network.
    let r = crate::volumes::volumes_create(
        State(app.clone()),
        axum::Json(serde_json::from_value(serde_json::json!({"Name":"myvol"})).unwrap()),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CREATED);
    crate::networks::networks_create(State(app.clone()), net_create_body("mynet")).await;

    // Create a container bound to myvol AND attached to mynet.
    let r = crate::containers::containers_create(
        State(app.clone()),
        Query(create_q(serde_json::json!({}))),
        create_body(serde_json::json!({
            "Image":"alpine",
            "HostConfig": {"Binds":["myvol:/data"], "NetworkMode":"mynet"}
        })),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CREATED);
    let cid = to_body_json(r).await["Id"].as_str().unwrap().to_string();

    // Both cross-resource refs are live: net membership + endpoint, and the named volume is in-use.
    assert!(
        net_members(&app, "mynet").await.contains(&cid),
        "joined mynet"
    );
    assert_eq!(
        net_endpoint_count(&app, "mynet").await,
        1,
        "endpoint allocated"
    );
    // In-use proof: delete of the bound named volume is refused (409).
    let r = crate::volumes::volume_delete(State(app.clone()), Path("myvol".into())).await;
    assert_eq!(
        r.status(),
        StatusCode::CONFLICT,
        "bound named volume delete is 409"
    );
    assert!(
        app.inner
            .lock()
            .await
            .volumes
            .iter()
            .any(|v| v.name == "myvol"),
        "the refused delete leaves the volume in place"
    );

    // Plain `rm` (no -v) on the created (non-running) container -> 204, engine-free.
    let r = crate::containers::containers_delete(
        State(app.clone()),
        Path(cid.clone()),
        Query::<crate::containers::DeleteQ>(serde_json::from_value(serde_json::json!({})).unwrap()),
    )
    .await;
    assert_eq!(r.status(), StatusCode::NO_CONTENT);

    // Network endpoint freed (no dangling membership); NAMED volume KEPT (docker keeps it on plain rm).
    assert!(
        !net_members(&app, "mynet").await.contains(&cid),
        "left mynet on rm"
    );
    assert_eq!(
        net_endpoint_count(&app, "mynet").await,
        0,
        "endpoint freed on rm"
    );
    assert!(
        app.inner
            .lock()
            .await
            .volumes
            .iter()
            .any(|v| v.name == "myvol"),
        "a NAMED volume must survive a plain rm (only anon volumes are GC'd, and only on -v)"
    );

    // The now-unreferenced named volume is freely deletable (204) — proving the ref was truly released.
    let r = crate::volumes::volume_delete(State(app.clone()), Path("myvol".into())).await;
    assert_eq!(
        r.status(),
        StatusCode::NO_CONTENT,
        "freed named volume deletes"
    );
}
