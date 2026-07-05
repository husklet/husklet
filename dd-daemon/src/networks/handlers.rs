#![allow(unused_imports, dead_code)]
use crate::archive::*;
use crate::build::*;
use crate::containers::*;
use crate::images::*;
use crate::model::*;
use crate::registry::{Client, Credentials, ImageRef};
use crate::runtime::*;
use crate::system::*;
use crate::util::*;
use crate::volumes::*;
use crate::prelude::*;
use ddjit::{Guest, PortMap, SpawnConfig, Volume};

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
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"message": "predefined network cannot be removed"})),
        )
            .into_response();
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
    // Resolve to a full container id + its reported name before mutating networks (avoids borrowing
    // `g.networks` mutably while `g.containers` is borrowed immutably).
    let (cid, cname) = match resolve_get(&g, &req).map(|(f, c)| (f, endpoint_name(c))) {
        Some(t) => t,
        None => (req.clone(), req.clone()),
    };
    let net_name = match g.networks.iter().find(|n| net_matches(n, &id)) {
        Some(n) => n.name.clone(),
        None => return no_such_network(&id),
    };
    join_network(&mut g.networks, &net_name, &cid, &cname);
    save_state(&g, &a.state_path);
    StatusCode::OK.into_response()
}

pub(crate) async fn network_disconnect(
    State(a): State<App>,
    Path(id): Path<String>,
    Json(b): Json<NetAttachBody>,
) -> Response {
    let req = b.container.unwrap_or_default();
    let mut g = a.inner.lock().await;
    let cid = resolve_cid(&g, &req).unwrap_or(req);
    let r = match g.networks.iter_mut().find(|n| net_matches(n, &id)) {
        Some(n) => {
            leave_network(n, &cid);
            StatusCode::OK.into_response()
        }
        None => return no_such_network(&id),
    };
    save_state(&g, &a.state_path);
    r
}

/// `POST /networks/prune` — `docker network prune`. Removes user-defined networks with no attached
/// containers (never the predefined bridge/host/none).
pub(crate) async fn networks_prune(State(a): State<App>) -> Json<crate::api::NetworksPruneReport> {
    let mut g = a.inner.lock().await;
    let pruned: Vec<String> = g
        .networks
        .iter()
        .filter(|n| !is_predefined(&n.name) && n.containers.is_empty())
        .map(|n| n.name.clone())
        .collect();
    g.networks.retain(|n| !pruned.contains(&n.name));
    save_state(&g, &a.state_path);
    Json(crate::api::NetworksPruneReport {
        networks_deleted: pruned,
    })
}
