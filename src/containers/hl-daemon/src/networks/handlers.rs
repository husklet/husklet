use crate::model::*;
use crate::prelude::*;
use crate::util::*;

use super::Networks;

impl Networks {
    pub(crate) async fn list(State(a): State<App>) -> Json<Vec<crate::api::NetworkJson>> {
        let g = a.inner.lock().await;
        Json(
            g.networks
                .iter()
                .map(|network: &Net| network.json())
                .collect::<Vec<_>>(),
        )
    }
}

#[derive(Deserialize)]
pub(crate) struct NetCreateBody {
    #[serde(rename = "Name")]
    name: Option<String>,
    #[serde(rename = "Driver")]
    driver: Option<String>,
}

impl Networks {
    pub(crate) async fn create(State(a): State<App>, Json(body): Json<NetCreateBody>) -> Response {
        let name = body
            .name
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| format!("net_{}", &Digest::fake("n")[..8]));
        let mut g = a.inner.lock().await;
        if g.networks.iter().any(|n| n.name == name) {
            return ErrorMessage::conflict(format!("network {name} already exists"));
        }
        let (subnet, gateway) = Net::alloc_subnet(&g.networks);
        let n = Net {
            id: Digest::fake(&format!("net-{name}")),
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
        // Persist BEFORE returning 201: if the state save fails, roll back the in-memory network and fail —
        // a `201 Created` must not describe a network that vanishes on the next daemon restart.
        if let Err(e) = Store::save_checked(&g, &a.state_path) {
            g.networks.retain(|nn| nn.id != id);
            return ErrorMessage::server_error(format!("failed to persist network state: {e}"));
        }
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
}

impl Networks {
    pub(crate) async fn inspect(State(a): State<App>, Path(id): Path<String>) -> Response {
        match a
            .inner
            .lock()
            .await
            .networks
            .iter()
            .find(|n| n.matches(&id))
        {
            Some(n) => Json(n.json()).into_response(),
            None => ErrorMessage::no_such_network(&id),
        }
    }
}

impl Networks {
    pub(crate) async fn delete(State(a): State<App>, Path(id): Path<String>) -> Response {
        let mut g = a.inner.lock().await;
        if g.networks
            .iter()
            .any(|n| n.matches(&id) && n.is_predefined())
        {
            return ErrorMessage::forbidden("predefined network cannot be removed");
        }
        // A user network with connected containers can't be removed — docker refuses with 403 until every
        // endpoint is disconnected. `containers` is populated by join_network / cleared by leave_network.
        if let Some(n) = g
            .networks
            .iter()
            .find(|n| n.matches(&id) && !n.containers.is_empty())
        {
            return ErrorMessage::forbidden(format!(
                "network {name} id {nid} has active endpoints",
                name = n.name,
                nid = n.id
            ));
        }
        let removed = g
            .networks
            .iter()
            .find(|n| n.matches(&id))
            .map(|n| (n.id.clone(), n.name.clone(), n.driver.clone()));
        let before = g.networks.len();
        g.networks.retain(|n| !n.matches(&id));
        if g.networks.len() != before {
            Store::save(&g, &a.state_path);
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
            ErrorMessage::no_such_network(&id)
        }
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
    let net_name = match g.networks.iter().find(|n| n.matches(&id)) {
        Some(n) => n.name.clone(),
        None => return ErrorMessage::no_such_network(&id),
    };
    let (cid, cname) = match ContainerId::get(&g, &req).map(|(f, c)| (f, Net::endpoint_name(c))) {
        Some(t) => t,
        None => return ErrorMessage::no_such(&req),
    };
    let net_id = g
        .networks
        .iter()
        .find(|n| n.name == net_name)
        .map(|n| n.id.clone())
        .unwrap_or_default();
    Net::join_network(&mut g.networks, &net_name, &cid, &cname);
    Store::save(&g, &a.state_path);
    // Apply the change to the LIVE network, not just daemon bookkeeping: refresh the per-user-network
    // reach-by-name table so the newly-attached endpoint is resolvable to running peers at once.
    Networks::refresh_live_names(&g, &net_name);
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
    if !g.networks.iter().any(|n| n.matches(&id)) {
        return ErrorMessage::no_such_network(&id);
    }
    let cid = match ContainerId::resolve(&g, &req) {
        Some(c) => c,
        None => return ErrorMessage::no_such(&req),
    };
    let mut net_id = String::new();
    let mut net_name = String::new();
    if let Some(n) = g.networks.iter_mut().find(|n| n.matches(&id)) {
        net_id = n.id.clone();
        net_name = n.name.clone();
        n.leave(&cid);
    }
    Store::save(&g, &a.state_path);
    // Apply to the LIVE network: rewrite the per-user-network reach-by-name table so the disconnected
    // container's now-stale name stops resolving for running peers immediately (not only after a restart).
    Networks::refresh_live_names(&g, &net_name);
    crate::events::emit_event(
        &a.events,
        "network",
        "disconnect",
        &net_id,
        json!({"name": net_name, "container": cid}),
    );
    StatusCode::OK.into_response()
}

/// Re-emit the LIVE reach-by-name table for a user-defined network after a connect/disconnect, so the
/// change reaches the in-engine 127.0.0.11 resolvers of running peers AT ONCE — the identical file the
/// spawn path writes per container start (`/tmp/.hlbr-<netid>/.names`, one `ip\tname` line per endpoint;
/// see runtime/spawn/live.rs and runtime/spawn/net.rs). Without this, `network connect`/`disconnect` on a
/// running container mutated only daemon state: a connected container stayed unresolvable to live peers,
/// and a disconnected one's stale name kept resolving, until the peer restarted and re-read its snapshot.
/// Docker withholds embedded-DNS names on the predefined `bridge`, so predefined networks are skipped
/// (mirrors spawn). A no-op for an unknown name. Best-effort: a write error never fails the API call.
impl Networks {
    fn refresh_live_names(g: &Inner, net_name: &str) {
        if let Some(n) = g.networks.iter().find(|n| n.name == net_name) {
            if !n.is_predefined() {
                crate::runtime::NetworkNames::write(&n.id, &n.endpoints);
            }
        }
    }
}

/// `POST /networks/prune` — `docker network prune`. Removes user-defined networks with no attached
/// containers (never the predefined bridge/host/none).
impl Networks {
    pub(crate) async fn prune(State(a): State<App>) -> Json<crate::api::NetworksPruneReport> {
        let mut g = a.inner.lock().await;
        // Collect (id, name, driver) so we can both report the names and emit a destroy event per network.
        let pruned: Vec<(String, String, String)> = g
            .networks
            .iter()
            .filter(|n| !n.is_predefined() && n.containers.is_empty())
            .map(|n| (n.id.clone(), n.name.clone(), n.driver.clone()))
            .collect();
        let pruned_names: Vec<String> = pruned.iter().map(|(_, n, _)| n.clone()).collect();
        g.networks.retain(|n| !pruned_names.contains(&n.name));
        Store::save(&g, &a.state_path);
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
}
