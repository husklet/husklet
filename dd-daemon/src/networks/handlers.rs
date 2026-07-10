use crate::model::*;
use crate::util::*;
use crate::prelude::*;

use super::ipam::*;

pub(crate) async fn networks_list(State(a): State<App>) -> Json<Vec<crate::api::NetworkJson>> {
    let g = a.inner.lock().await;
    Json(g.networks.iter().map(net_json).collect::<Vec<_>>())
}

#[derive(Deserialize)]
pub(crate) struct NetCreateBody {
    #[serde(rename = "Name")]
    name: Option<String>,
    #[serde(rename = "Driver")]
    driver: Option<String>,
}

pub(crate) async fn networks_create(
    State(a): State<App>,
    Json(body): Json<NetCreateBody>,
) -> Response {
    let name = body
        .name
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| format!("net_{}", &fake_id("n")[..8]));
    let mut g = a.inner.lock().await;
    if g.networks.iter().any(|n| n.name == name) {
        return conflict(format!("network {name} already exists"));
    }
    let (subnet, gateway) = alloc_subnet(&g.networks);
    let n = Net {
        id: fake_id(&format!("net-{name}")),
        name,
        driver: body.driver.unwrap_or_else(|| "bridge".into()),
        scope: "local".into(),
        containers: vec![],
        created: now_secs(),
        subnet,
        gateway,
        endpoints: HashMap::new(),
    };
    let id = n.id.clone();
    let ev_name = n.name.clone();
    let ev_driver = n.driver.clone();
    g.networks.push(n);
    save_state(&g, &a.state_path);
    crate::events::emit_event(
        &a.events,
        "network",
        "create",
        &id,
        json!({"name": ev_name, "type": ev_driver}),
    );
    (
        StatusCode::CREATED,
        Json(crate::api::NetworkCreateResponse {
            id,
            warning: String::new(),
        }),
    )
        .into_response()
}

pub(crate) async fn network_inspect(State(a): State<App>, Path(id): Path<String>) -> Response {
    match a
        .inner
        .lock()
        .await
        .networks
        .iter()
        .find(|n| net_matches(n, &id))
    {
        Some(n) => Json(net_json(n)).into_response(),
        None => no_such_network(&id),
    }
}

pub(crate) async fn network_delete(State(a): State<App>, Path(id): Path<String>) -> Response {
    let mut g = a.inner.lock().await;
    if g.networks
        .iter()
        .any(|n| net_matches(n, &id) && is_predefined(&n.name))
    {
        return forbidden("predefined network cannot be removed");
    }
    // A user network with connected containers can't be removed — docker refuses with 403 until every
    // endpoint is disconnected. `containers` is populated by join_network / cleared by leave_network.
    if let Some(n) = g
        .networks
        .iter()
        .find(|n| net_matches(n, &id) && !n.containers.is_empty())
    {
        return forbidden(format!(
            "network {name} id {nid} has active endpoints",
            name = n.name,
            nid = n.id
        ));
    }
    let removed = g
        .networks
        .iter()
        .find(|n| net_matches(n, &id))
        .map(|n| (n.id.clone(), n.name.clone(), n.driver.clone()));
    let before = g.networks.len();
    g.networks.retain(|n| !net_matches(n, &id));
    if g.networks.len() != before {
        save_state(&g, &a.state_path);
        if let Some((rid, rname, rdriver)) = removed {
            crate::events::emit_event(
                &a.events,
                "network",
                "destroy",
                &rid,
                json!({"name": rname, "type": rdriver}),
            );
        }
        StatusCode::NO_CONTENT.into_response()
    } else {
        no_such_network(&id)
    }
}

#[derive(Deserialize)]
pub(crate) struct NetAttachBody {
    #[serde(rename = "Container")]
    container: Option<String>,
}

pub(crate) async fn network_connect(
    State(a): State<App>,
    Path(id): Path<String>,
    Json(b): Json<NetAttachBody>,
) -> Response {
    let req = b.container.unwrap_or_default();
    let mut g = a.inner.lock().await;
    // Docker order: the network must exist (404 network), then the CONTAINER must exist (404 container).
    // The old code fell back to `(req, req)` on a container miss and joined anyway, inserting a PHANTOM
    // endpoint (200 instead of 404) that then made the network permanently undeletable (403). Resolve
    // the net name first, then the full cid/name (both immutable borrows end before the mutable join).
    let net_name = match g.networks.iter().find(|n| net_matches(n, &id)) {
        Some(n) => n.name.clone(),
        None => return no_such_network(&id),
    };
    let (cid, cname) = match resolve_get(&g, &req).map(|(f, c)| (f, endpoint_name(c))) {
        Some(t) => t,
        None => return no_such(&req),
    };
    let net_id = g
        .networks
        .iter()
        .find(|n| n.name == net_name)
        .map(|n| n.id.clone())
        .unwrap_or_default();
    join_network(&mut g.networks, &net_name, &cid, &cname);
    save_state(&g, &a.state_path);
    // `network/connect` so event mirrors track endpoint attach (Actor is the network; the attached
    // container id is an attribute, mirroring docker's connect/disconnect events).
    crate::events::emit_event(
        &a.events,
        "network",
        "connect",
        &net_id,
        json!({"name": net_name, "container": cid}),
    );
    StatusCode::OK.into_response()
}

pub(crate) async fn network_disconnect(
    State(a): State<App>,
    Path(id): Path<String>,
    Json(b): Json<NetAttachBody>,
) -> Response {
    let req = b.container.unwrap_or_default();
    let mut g = a.inner.lock().await;
    // Docker order (mirrors connect): the network must exist (404 network), then the CONTAINER must
    // exist (404 container). The old code fell back to the raw request string on a container miss and
    // returned 200, so disconnecting a nonexistent container looked successful to cleanup tooling.
    if !g.networks.iter().any(|n| net_matches(n, &id)) {
        return no_such_network(&id);
    }
    let cid = match resolve_cid(&g, &req) {
        Some(c) => c,
        None => return no_such(&req),
    };
    let mut net_id = String::new();
    let mut net_name = String::new();
    if let Some(n) = g.networks.iter_mut().find(|n| net_matches(n, &id)) {
        net_id = n.id.clone();
        net_name = n.name.clone();
        leave_network(n, &cid);
    }
    save_state(&g, &a.state_path);
    crate::events::emit_event(
        &a.events,
        "network",
        "disconnect",
        &net_id,
        json!({"name": net_name, "container": cid}),
    );
    StatusCode::OK.into_response()
}

/// `POST /networks/prune` — `docker network prune`. Removes user-defined networks with no attached
/// containers (never the predefined bridge/host/none).
pub(crate) async fn networks_prune(State(a): State<App>) -> Json<crate::api::NetworksPruneReport> {
    let mut g = a.inner.lock().await;
    // Collect (id, name, driver) so we can both report the names and emit a destroy event per network.
    let pruned: Vec<(String, String, String)> = g
        .networks
        .iter()
        .filter(|n| !is_predefined(&n.name) && n.containers.is_empty())
        .map(|n| (n.id.clone(), n.name.clone(), n.driver.clone()))
        .collect();
    let pruned_names: Vec<String> = pruned.iter().map(|(_, n, _)| n.clone()).collect();
    g.networks.retain(|n| !pruned_names.contains(&n.name));
    save_state(&g, &a.state_path);
    // Emit `network/destroy` for each pruned network so event mirrors drop them (docker parity).
    for (nid, nname, ndriver) in &pruned {
        crate::events::emit_event(
            &a.events,
            "network",
            "destroy",
            nid,
            json!({"name": nname, "type": ndriver}),
        );
    }
    Json(crate::api::NetworksPruneReport {
        networks_deleted: pruned_names,
    })
}
