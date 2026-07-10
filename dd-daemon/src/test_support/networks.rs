//! Network create/delete/connect/inspect tests + network-centric multi-step flows.
use super::*;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;

// ---- 2. network_delete: connected user net -> 403, predefined -> 403, empty user -> 204 ------
#[tokio::test]
async fn network_delete_with_connected_container_is_403() {
    let app = test_app();
    seed_network(&app, "mynet", /*with_container=*/ true).await;
    let resp =
        crate::networks::network_delete(State(app.clone()), Path("mynet".into())).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert!(
        app.inner.lock().await.networks.iter().any(|n| n.name == "mynet"),
        "network must remain"
    );
}

#[tokio::test]
async fn network_delete_predefined_bridge_is_403() {
    let app = test_app();
    seed_predefined_bridge(&app).await;
    let resp =
        crate::networks::network_delete(State(app.clone()), Path("bridge".into())).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert!(app.inner.lock().await.networks.iter().any(|n| n.name == "bridge"));
}

#[tokio::test]
async fn network_delete_empty_user_net_is_204_and_removed() {
    let app = test_app();
    seed_network(&app, "mynet", /*with_container=*/ false).await;
    let resp =
        crate::networks::network_delete(State(app.clone()), Path("mynet".into())).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert!(
        !app.inner.lock().await.networks.iter().any(|n| n.name == "mynet"),
        "network must be gone"
    );
}

// ---- 7. networks_create -> 201 + present; duplicate name -> 409 ------------------------------
#[tokio::test]
async fn network_create_then_duplicate() {
    let app = test_app();
    let body = axum::Json(serde_json::from_value(serde_json::json!({"Name":"net1"})).unwrap());
    let resp = crate::networks::networks_create(State(app.clone()), body).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    assert!(app.inner.lock().await.networks.iter().any(|n| n.name == "net1"));

    let body2 = axum::Json(serde_json::from_value(serde_json::json!({"Name":"net1"})).unwrap());
    let resp2 = crate::networks::networks_create(State(app.clone()), body2).await;
    assert_eq!(resp2.status(), StatusCode::CONFLICT);
    assert_eq!(
        app.inner.lock().await.networks.iter().filter(|n| n.name == "net1").count(),
        1,
        "duplicate must not be inserted"
    );
}

// ---- 13. network_inspect: object key fields; missing -> 404 ---------------------------------
#[tokio::test]
async fn network_inspect_wire_shape() {
    let app = test_app();
    seed_network(&app, "mynet", /*with_container=*/ false).await;
    let resp = crate::networks::network_inspect(State(app.clone()), Path("mynet".into()))
        .await
        .into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = to_body_json(resp).await;
    for key in [
        "Id", "Name", "Driver", "Scope", "Containers", "Created", "EnableIPv6", "Internal",
        "IPAM",
    ] {
        assert!(v.as_object().unwrap().contains_key(key), "network missing key {key}");
    }
    assert_eq!(v["Name"], "mynet");
    assert_eq!(v["Driver"], "bridge");
    assert_eq!(v["Scope"], "local");
    assert_eq!(v["IPAM"]["Config"][0]["Subnet"], "172.20.0.0/16");
    assert_eq!(v["IPAM"]["Config"][0]["Gateway"], "172.20.0.1");
}

#[tokio::test]
async fn network_inspect_missing_is_404() {
    let app = test_app();
    let resp = crate::networks::network_inspect(State(app.clone()), Path("ghost".into()))
        .await
        .into_response();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---- FLOW 1: network endpoint refcount, connect → 403-on-delete → disconnect → 204 -------------
// docker: a user network with a connected endpoint refuses removal (403 "has active endpoints")
// until every container is disconnected; then delete is a 204. Locks the endpoint-refcount fix.
#[tokio::test]
async fn network_connect_missing_container_is_404_no_phantom() {
    // Regression: connecting a NONEXISTENT container must 404 (docker "No such container"), not
    // silently insert a phantom endpoint — which used to return 200 and make the network permanently
    // undeletable (403 forever). Also: a missing NETWORK still 404s first.
    let app = test_app();
    let r = crate::networks::networks_create(State(app.clone()), net_create_body("mynet")).await;
    assert_eq!(r.status(), StatusCode::CREATED);

    let r = crate::networks::network_connect(
        State(app.clone()),
        Path("mynet".into()),
        net_attach_body("ghost"),
    )
    .await;
    assert_eq!(r.status(), StatusCode::NOT_FOUND, "connect of a missing container is 404");
    assert_eq!(net_members(&app, "mynet").await.len(), 0, "no phantom membership");
    assert_eq!(net_endpoint_count(&app, "mynet").await, 0, "no phantom endpoint");

    // The network stays deletable (the phantom endpoint had made this a permanent 403).
    let r = crate::networks::network_delete(State(app.clone()), Path("mynet".into())).await;
    assert_eq!(r.status(), StatusCode::NO_CONTENT, "net with no real endpoints deletes");

    // A missing NETWORK is still resolved first -> 404 network.
    let r = crate::networks::network_connect(
        State(app.clone()),
        Path("nope".into()),
        net_attach_body("also-ghost"),
    )
    .await;
    assert_eq!(r.status(), StatusCode::NOT_FOUND, "connect to a missing network is 404");
}

// "Network Disconnect Missing Container Returns OK" (P2): disconnecting a NONEXISTENT container must 404
// (docker "No such container"), not silently 200 — otherwise cleanup tooling believes it detached a
// container that was never present.
#[tokio::test]
async fn network_disconnect_missing_container_is_404_noop() {
    let app = test_app();
    let r = crate::networks::networks_create(State(app.clone()), net_create_body("mynet")).await;
    assert_eq!(r.status(), StatusCode::CREATED);

    let r = crate::networks::network_disconnect(
        State(app.clone()),
        Path("mynet".into()),
        net_attach_body("ghost"),
    )
    .await;
    assert_eq!(r.status(), StatusCode::NOT_FOUND, "disconnect of a missing container is 404");

    // A missing NETWORK is still resolved first -> 404 network.
    let r = crate::networks::network_disconnect(
        State(app.clone()),
        Path("nope".into()),
        net_attach_body("also-ghost"),
    )
    .await;
    assert_eq!(r.status(), StatusCode::NOT_FOUND, "disconnect on a missing network is 404");
}

#[tokio::test]
async fn flow_network_endpoint_refcount_lifecycle() {
    let app = test_app();
    seed_container(&app, "c1", "running").await;

    // Step 1: create the network — 201, present, no members yet.
    let r = crate::networks::networks_create(State(app.clone()), net_create_body("mynet")).await;
    assert_eq!(r.status(), StatusCode::CREATED);
    assert_eq!(net_members(&app, "mynet").await.len(), 0, "fresh net has no members");
    assert_eq!(net_endpoint_count(&app, "mynet").await, 0);

    // Step 2: connect c1 — 200, c1 now in membership AND endpoints.
    let r = crate::networks::network_connect(
        State(app.clone()),
        Path("mynet".into()),
        net_attach_body("c1"),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(net_members(&app, "mynet").await, vec!["c1".to_string()], "c1 joined");
    assert_eq!(net_endpoint_count(&app, "mynet").await, 1, "endpoint allocated");

    // Step 3: delete while an endpoint is attached — 403, network survives (the refcount gate).
    let r = crate::networks::network_delete(State(app.clone()), Path("mynet".into())).await;
    assert_eq!(r.status(), StatusCode::FORBIDDEN, "delete with an endpoint is 403");
    assert!(
        app.inner.lock().await.networks.iter().any(|n| n.name == "mynet"),
        "refused delete must leave the network in place"
    );

    // Step 4: disconnect c1 — 200, membership AND endpoints both cleared.
    let r = crate::networks::network_disconnect(
        State(app.clone()),
        Path("mynet".into()),
        net_attach_body("c1"),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(net_members(&app, "mynet").await.len(), 0, "membership cleared on disconnect");
    assert_eq!(
        net_endpoint_count(&app, "mynet").await,
        0,
        "endpoint freed on disconnect (not just membership) — the refcount must reach zero"
    );

    // Step 5: delete now that the last endpoint is gone — 204, network removed.
    let r = crate::networks::network_delete(State(app.clone()), Path("mynet".into())).await;
    assert_eq!(r.status(), StatusCode::NO_CONTENT, "delete after disconnect is 204");
    assert!(
        !app.inner.lock().await.networks.iter().any(|n| n.name == "mynet"),
        "network gone after its last endpoint left"
    );
}

// ---- FLOW 4: duplicate net (409), predefined delete (403), prune selectivity -----------------
// docker: creating a network name twice is a 409; the predefined bridge/host/none can never be
// removed (403); `network prune` reclaims ONLY empty user networks — never a predefined one, never
// one with an attached endpoint.
#[tokio::test]
async fn flow_network_duplicate_predefined_and_prune_selectivity() {
    let app = test_app();
    // Start from the three predefined networks (bridge/host/none), as a live daemon would.
    seed_predefined_bridge(&app).await;

    // Duplicate create: first `mynet` 201, second 409, count stays 1.
    let r = crate::networks::networks_create(State(app.clone()), net_create_body("mynet")).await;
    assert_eq!(r.status(), StatusCode::CREATED);
    let r = crate::networks::networks_create(State(app.clone()), net_create_body("mynet")).await;
    assert_eq!(r.status(), StatusCode::CONFLICT, "duplicate network name is 409");
    assert_eq!(
        app.inner.lock().await.networks.iter().filter(|n| n.name == "mynet").count(),
        1,
        "the duplicate must not be inserted"
    );

    // Predefined delete: `bridge` is 403 and stays.
    let r = crate::networks::network_delete(State(app.clone()), Path("bridge".into())).await;
    assert_eq!(r.status(), StatusCode::FORBIDDEN, "predefined bridge delete is 403");
    assert!(app.inner.lock().await.networks.iter().any(|n| n.name == "bridge"));

    // Add an EMPTY user net (prunable) and a BUSY user net (an endpoint -> not prunable).
    crate::networks::networks_create(State(app.clone()), net_create_body("emptyuser"))
        .await;
    seed_network(&app, "busyuser", /*with_container=*/ true).await;

    // Prune: reclaims `emptyuser` + `mynet` only; keeps bridge/host/none AND the busy net.
    let axum::Json(report) = crate::networks::networks_prune(State(app.clone())).await;
    let pruned: std::collections::HashSet<&str> =
        report.networks_deleted.iter().map(|s| s.as_str()).collect();
    assert!(pruned.contains("emptyuser"), "empty user net is pruned: {pruned:?}");
    assert!(pruned.contains("mynet"), "the other empty user net is pruned: {pruned:?}");
    assert!(!pruned.contains("busyuser"), "a net with an endpoint is NOT pruned");
    for p in ["bridge", "host", "none"] {
        assert!(!pruned.contains(p), "predefined {p} is never pruned");
    }
    let names: std::collections::HashSet<String> = app
        .inner
        .lock()
        .await
        .networks
        .iter()
        .map(|n| n.name.clone())
        .collect();
    assert!(names.contains("bridge") && names.contains("host") && names.contains("none"));
    assert!(names.contains("busyuser"), "busy net survives prune");
    assert!(!names.contains("emptyuser") && !names.contains("mynet"), "empty user nets gone");
}

// ---- FLOW 5: lifecycle EVENTS fire across create/delete (bus read in-test via try_recv) -------
// docker: create/destroy publish `network create` / `network destroy` on the events bus.
// `emit_event` short-circuits when `receiver_count()==0`, so we subscribe FIRST; a broadcast
// Receiver + non-blocking `try_recv` drains without the /events streaming handler (no deadlock).
#[tokio::test]
async fn flow_events_emitted_across_network_lifecycle() {
    let app = test_app();
    let mut rx = app.events.subscribe(); // must precede any emit (bus skips with 0 receivers)

    let r = crate::networks::networks_create(State(app.clone()), net_create_body("evnet")).await;
    assert_eq!(r.status(), StatusCode::CREATED);
    let r = crate::networks::network_delete(State(app.clone()), Path("evnet".into())).await;
    assert_eq!(r.status(), StatusCode::NO_CONTENT);

    // Drain everything the bus buffered for us.
    let mut seen: Vec<(String, String)> = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        seen.push((
            ev["Type"].as_str().unwrap_or("").to_string(),
            ev["Action"].as_str().unwrap_or("").to_string(),
        ));
    }
    assert!(
        seen.contains(&("network".into(), "create".into())),
        "a network/create event must fire: {seen:?}"
    );
    assert!(
        seen.contains(&("network".into(), "destroy".into())),
        "a network/destroy event must fire: {seen:?}"
    );
}

// ---- FLOW 17: network connect IDEMPOTENCY + double disconnect --------------------------------
// docker contract: a SECOND `network connect` of the same container is a 403/500 ("endpoint ...
// already exists in network"); a `network disconnect` of a container that is NOT attached is a
// 500 ("is not connected to network"). dd is LENIENT on both (join/leave are idempotent). The
// SAFETY property under test is the refcount: a double-connect must NOT leak a second endpoint that
// a single disconnect then cannot clear. Drives connect x2 -> disconnect -> disconnect(again).
#[tokio::test]
async fn flow_network_connect_idempotent_no_refcount_leak() {
    let app = test_app();
    seed_container(&app, "c1", "running").await;
    let r = crate::networks::networks_create(State(app.clone()), net_create_body("mynet")).await;
    assert_eq!(r.status(), StatusCode::CREATED);

    // Step 1: connect c1 — 200, one endpoint.
    let r = crate::networks::network_connect(
        State(app.clone()),
        Path("mynet".into()),
        net_attach_body("c1"),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(net_endpoint_count(&app, "mynet").await, 1, "first connect allocates one endpoint");
    assert_eq!(net_members(&app, "mynet").await, vec!["c1".to_string()]);

    // Step 2: connect c1 AGAIN. docker would 403/500; dd is idempotent (200). CRITICAL: the endpoint
    // count must STAY 1 — a leak here (count 2) would need two disconnects to clear (a refcount bug).
    let r = crate::networks::network_connect(
        State(app.clone()),
        Path("mynet".into()),
        net_attach_body("c1"),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK, "dd re-connect is idempotent 200 (docker would 403/500)");
    assert_eq!(
        net_endpoint_count(&app, "mynet").await,
        1,
        "re-connect must NOT leak a second endpoint (idempotent join keyed by cid)"
    );
    assert_eq!(
        net_members(&app, "mynet").await,
        vec!["c1".to_string()],
        "re-connect must NOT duplicate the membership entry"
    );

    // Step 3: disconnect once — fully clears (endpoint AND membership reach zero in ONE call).
    let r = crate::networks::network_disconnect(
        State(app.clone()),
        Path("mynet".into()),
        net_attach_body("c1"),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(
        net_endpoint_count(&app, "mynet").await,
        0,
        "a SINGLE disconnect fully clears the endpoint (no leaked refcount from the double-connect)"
    );
    assert_eq!(net_members(&app, "mynet").await.len(), 0, "membership cleared in one disconnect");

    // Step 4: disconnect AGAIN, already gone. docker: 500 "is not connected"; dd: idempotent 200,
    // state unchanged. Soft divergence (lenient) — flagged, not a state-corruption bug.
    let r = crate::networks::network_disconnect(
        State(app.clone()),
        Path("mynet".into()),
        net_attach_body("c1"),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK, "dd double-disconnect is idempotent 200 (docker would 500)");
    assert_eq!(net_endpoint_count(&app, "mynet").await, 0, "still zero endpoints");

    // The now-empty net is deletable (proving the refcount truly reached zero).
    let r = crate::networks::network_delete(State(app.clone()), Path("mynet".into())).await;
    assert_eq!(r.status(), StatusCode::NO_CONTENT, "empty net deletes (refcount reached zero)");
}
