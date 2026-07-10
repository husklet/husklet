//! The `docker events` lifecycle bus.
//!
//! A single process-wide [`tokio::sync::broadcast`] channel of Docker event JSON values. Lifecycle
//! handlers (`containers.rs`, `images.rs`, `networks.rs`, `volumes.rs`) publish with [`emit_event`];
//! the long-lived `GET /events` stream ([`events`]) subscribes and writes one JSON object per line
//! (newline-delimited, chunked) — the exact shape `docker events` / the Engine API client decode.
//!
//! Mirrors the [`crate::model::Live`] broadcast pattern (one `Sender` kept in shared state, each
//! client gets its own `Receiver` via `subscribe()`). The `Sender` lives even with zero receivers:
//! `send` just returns `Err` (ignored) and later `subscribe()` calls still work.

use crate::model::App;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use futures_util::stream;
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::broadcast;

/// The shared event bus. A clone of this `Sender` lives in [`App`]; every `/events` client holds a
/// `Receiver` from `subscribe()`. Carries one fully-formed Docker event JSON object per message.
pub(crate) type EventBus = broadcast::Sender<Value>;

/// Create a fresh bus. Capacity is the per-receiver backlog before a slow client lags (and skips the
/// oldest events rather than blocking publishers). Used by `main.rs` to init `App.events`.
pub(crate) fn new_bus() -> EventBus {
    let (tx, _rx) = broadcast::channel(256);
    tx
}

/// Publish one lifecycle event. `type_`/`action` are Docker's event taxonomy (`"container"`/`"start"`,
/// `"image"`/`"pull"`, `"network"`/`"create"`, `"volume"`/`"destroy"`, ...). `id` is the primary
/// object id (container/image/network/volume). `attrs` is a JSON object of `Actor.Attributes`
/// (`{"name":..,"image":..}`); a non-object is treated as empty. Best-effort: a send with no live
/// `/events` clients is silently dropped.
pub(crate) fn emit_event(bus: &EventBus, type_: &str, action: &str, id: &str, attrs: Value) {
    if bus.receiver_count() == 0 {
        return; // no listeners — skip building the value entirely
    }
    let (secs, nanos) = crate::util::now_epoch_parts();
    let attributes = match attrs {
        Value::Object(m) => Value::Object(m),
        _ => Value::Object(serde_json::Map::new()),
    };
    // Docker's event document. `Type`/`Action`/`Actor` are the modern fields; `status`/`id`/`from`
    // are the legacy top-level aliases older clients still read for container events.
    let mut ev = crate::api::Event {
        type_: type_.to_string(),
        action: action.to_string(),
        actor: crate::api::Actor {
            id: id.to_string(),
            attributes,
        },
        scope: "local",
        time: secs,
        time_nano: nanos,
        status: None,
        id: None,
        from: None,
    };
    if type_ == "container" {
        ev.status = Some(action.to_string());
        ev.id = Some(id.to_string());
        if let Some(image) = ev.actor.attributes["image"].as_str() {
            ev.from = Some(image.to_string());
        }
    }
    // The bus carries `Value` (the `/events` stream re-serializes it and `Filters::matches` reads it
    // by key), so convert the typed DTO once here.
    let _ = bus.send(serde_json::to_value(&ev).unwrap_or_default()); // Err == no receivers; fine.
}

#[derive(Deserialize, Default)]
pub(crate) struct EventsQ {
    /// `docker events --filter`, sent as a URL-encoded JSON map (`{"type":["container"],...}`).
    pub(crate) filters: Option<String>,
    /// `since` is accepted (so the query deserializes) but not applied — dd keeps no historical event
    /// store, so `--since <past>` simply streams live events from now.
    #[allow(dead_code)] // wire-contract field: deserialized but intentionally unused (see above)
    pub(crate) since: Option<String>,
    /// `--until <t>`: makes `docker events` a BOUNDED command that ends once wall-clock passes `t`.
    pub(crate) until: Option<String>,
}

/// The parsed, best-effort subset of `docker events` filters dd honors.
#[derive(Default, Clone)]
struct Filters {
    types: Vec<String>,      // `type=` (container/image/network/volume/...)
    actions: Vec<String>,    // `event=`/`action=` (start/die/...)
    containers: Vec<String>, // `container=` (id or name)
    images: Vec<String>,     // `image=`
    labels: Vec<String>,     // `label=key` or `label=key=value`
    networks: Vec<String>,   // `network=` (id or name)
    volumes: Vec<String>,    // `volume=` (name)
    scopes: Vec<String>,     // `scope=` (local/swarm)
}

impl Filters {
    /// Parse the `filters` query param. `None`/empty ⇒ the empty (match-all) filter. Malformed JSON is an
    /// ERROR, not a silent fall-through to match-all: `docker events --filter '<bad json>'` must be a
    /// 400 bad-parameter, otherwise a client/proxy encoding bug silently subscribes to EVERY daemon event.
    fn parse(raw: &Option<String>) -> Result<Filters, String> {
        let mut f = Filters::default();
        let Some(s) = raw.as_deref().filter(|s| !s.is_empty()) else {
            return Ok(f);
        };
        let v: Value =
            serde_json::from_str(s).map_err(|e| format!("invalid filters JSON: {e}"))?;
        f.types = filter_values(&v, "type");
        f.actions = filter_values(&v, "event");
        f.actions.extend(filter_values(&v, "action"));
        f.containers = filter_values(&v, "container");
        f.images = filter_values(&v, "image");
        f.labels = filter_values(&v, "label");
        f.networks = filter_values(&v, "network");
        f.volumes = filter_values(&v, "volume");
        f.scopes = filter_values(&v, "scope");
        Ok(f)
    }

    /// Does this event pass every active filter? (An empty filter list = "match all" for that key.)
    fn matches(&self, ev: &Value) -> bool {
        let typ = ev["Type"].as_str().unwrap_or("");
        let action = ev["Action"].as_str().unwrap_or("");
        let id = ev["Actor"]["ID"].as_str().unwrap_or("");
        let attrs = &ev["Actor"]["Attributes"];
        let name = attrs["name"].as_str().unwrap_or("");
        let image = attrs["image"].as_str().unwrap_or("");
        if !self.types.is_empty() && !self.types.iter().any(|t| t == typ) {
            return false;
        }
        // Health transitions emit compound actions like `health_status: unhealthy`; docker's
        // `event=health_status` (and `event=exec_die`) filters match on the action's KEY (before `:`),
        // so compare both the full action and its `:`-prefixed key.
        let action_key = action.split(':').next().unwrap_or(action).trim();
        if !self.actions.is_empty()
            && !self.actions.iter().any(|a| a == action || a == action_key)
        {
            return false;
        }
        // Exact match on the resolved id or the name only. `events()` pre-resolves every `container=`
        // filter value to its FULL id and appends it to this set, so a name/id-prefix filter still
        // catches name-less events (die/stop/kill) via that full id. We deliberately do NOT also match
        // `id.starts_with(filter)` — that broad prefix made `--filter container=a` match every container
        // whose id merely begins with "a".
        if !self.containers.is_empty() && !self.containers.iter().any(|c| c == id || c == name) {
            return false;
        }
        // Image events publish the ref under `Attributes.name` (not `image`), so match either. Non-image
        // events (no name/image match) are still filtered out.
        if !self.images.is_empty()
            && !self.images.iter().any(|i| i == image || i == name)
        {
            return false;
        }
        // `label=key[=value]`: every requested label must be satisfied by the actor attributes (create
        // events now carry the object's labels as attributes). A bare `key` matches any value.
        for lf in &self.labels {
            let (k, want) = match lf.split_once('=') {
                Some((k, v)) => (k, Some(v)),
                None => (lf.as_str(), None),
            };
            let got = attrs[k].as_str();
            let ok = match (got, want) {
                (Some(g), Some(w)) => g == w,
                (Some(_), None) => true,
                _ => false,
            };
            if !ok {
                return false;
            }
        }
        // `scope=` narrows on the event scope (local/swarm).
        if !self.scopes.is_empty() {
            let scope = ev["scope"].as_str().or_else(|| ev["Scope"].as_str()).unwrap_or("");
            if !self.scopes.iter().any(|s| s == scope) {
                return false;
            }
        }
        // `network=` matches network-type events by id/name, or any event carrying a `network` attribute.
        if !self.networks.is_empty() {
            let net_attr = attrs["network"].as_str();
            let ok = (typ == "network" && self.networks.iter().any(|n| n == id || n == name))
                || net_attr.map_or(false, |x| self.networks.iter().any(|n| n == x));
            if !ok {
                return false;
            }
        }
        // `volume=` matches volume-type events by id/name, or any event carrying a `volume` attribute.
        if !self.volumes.is_empty() {
            let vol_attr = attrs["volume"].as_str();
            let ok = (typ == "volume" && self.volumes.iter().any(|v| v == id || v == name))
                || vol_attr.map_or(false, |x| self.volumes.iter().any(|v| v == x));
            if !ok {
                return false;
            }
        }
        true
    }
}

/// Extract the string values a Docker filter key carries. The wire format is `map[string][]string`
/// (`{"type":["container"]}`); older/CLI encodings use a set-as-object (`{"type":{"container":true}}`).
/// Both are handled; anything else yields an empty list.
fn filter_values(v: &Value, key: &str) -> Vec<String> {
    match &v[key] {
        Value::Array(a) => a
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect(),
        Value::Object(m) => m.keys().cloned().collect(),
        Value::String(s) => vec![s.clone()],
        _ => Vec::new(),
    }
}

/// `GET /events` — `docker events`. Subscribes to the bus and streams matching events as
/// newline-delimited JSON (one object per line, chunked) on a long-lived connection. The stream ends
/// only when the client disconnects or the bus is torn down (daemon shutdown).
pub(crate) async fn events(State(a): State<App>, Query(q): Query<EventsQ>) -> Response {
    let rx = a.events.subscribe();
    let mut filters = match Filters::parse(&q.filters) {
        Ok(f) => f,
        Err(e) => return crate::util::bad_request(e),
    };
    // `--filter container=<name|id>` is matched against each event's Actor.ID / name. Some lifecycle
    // events (die/stop/kill) carry no `name` attribute, so a name-only filter would miss them. Resolve
    // every container filter value to its FULL id now (by name or id-prefix) and add it to the match
    // set, so the id-based match catches all of that container's events regardless of the attributes.
    if !filters.containers.is_empty() {
        let g = a.inner.lock().await;
        let resolved: Vec<String> = filters
            .containers
            .iter()
            .filter_map(|c| crate::util::resolve_cid(&g, c))
            .collect();
        for id in resolved {
            if !filters.containers.contains(&id) {
                filters.containers.push(id);
            }
        }
    }

    // `--until <t>` makes `docker events` a BOUNDED command: it replays matching events up to `t` then
    // closes the stream and the CLI exits. dd keeps no historical event store, so an `--until` already in
    // the past has nothing to replay and closes IMMEDIATELY (an unbounded live stream here would hang the
    // client forever, e.g. `docker events --until $(date +%s)`); a future `--until` ends the live stream
    // once wall-clock passes it. Without `--until` the stream stays unbounded (ends on client disconnect).
    let until = q
        .until
        .as_deref()
        .and_then(|s| crate::util::parse_docker_ts(s, crate::util::now_secs()));
    if matches!(until, Some(u) if u <= crate::util::now_secs()) {
        return Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .body(Body::empty())
            .unwrap();
    }

    // `unfold` drives the broadcast receiver into a byte stream. Returning `Some` yields a line;
    // `continue` skips a filtered-out / lagged event; `None` ends the stream (bus closed, or `--until`).
    let body = stream::unfold((rx, filters, until), |(mut rx, filters, until)| async move {
        loop {
            // End the stream the moment a future `--until` bound passes (docker closes the bounded stream
            // then). Recomputed each iteration so a filtered-out event doesn't reset the deadline.
            let ev = match until {
                Some(u) => {
                    let remaining = (u - crate::util::now_secs()).max(0) as u64;
                    tokio::select! {
                        r = rx.recv() => r,
                        _ = tokio::time::sleep(std::time::Duration::from_secs(remaining)) => return None,
                    }
                }
                None => rx.recv().await,
            };
            match ev {
                Ok(ev) => {
                    if !filters.matches(&ev) {
                        continue;
                    }
                    let mut line = serde_json::to_vec(&ev).unwrap_or_default();
                    line.push(b'\n');
                    return Some((Ok::<Vec<u8>, std::io::Error>(line), (rx, filters, until)));
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from_stream(body))
        .unwrap()
}

#[cfg(test)]
mod filter_tests {
    use super::Filters;

    // "Malformed Filters JSON Becomes An Unfiltered Stream" (P1): bad filters JSON must be an error
    // (-> 400), never a silent fall-through to the empty match-all filter.
    #[test]
    fn malformed_filters_json_is_an_error() {
        assert!(Filters::parse(&Some("{\"type\":[\"container\"".into())).is_err(), "truncated JSON");
        assert!(Filters::parse(&Some("not json".into())).is_err());
    }

    #[test]
    fn absent_or_empty_filters_is_match_all_ok() {
        assert!(Filters::parse(&None).is_ok());
        assert!(Filters::parse(&Some(String::new())).is_ok());
    }

    #[test]
    fn valid_filters_parse_keys() {
        let f = Filters::parse(&Some(
            "{\"type\":[\"container\"],\"event\":[\"start\"],\"image\":[\"nginx\"]}".into(),
        ))
        .expect("valid filters parse");
        assert_eq!(f.types, vec!["container".to_string()]);
        assert_eq!(f.actions, vec!["start".to_string()]);
        assert_eq!(f.images, vec!["nginx".to_string()]);
    }

    use serde_json::json;

    // "Image Event Filters Drop Image Events": image lifecycle events carry the ref under
    // Attributes.name (not `image`), so `--filter image=busy:1` must still match.
    #[test]
    fn image_filter_matches_name_attribute() {
        let f = Filters::parse(&Some("{\"image\":[\"busy:1\"]}".into())).unwrap();
        let ev = json!({"Type":"image","Action":"pull",
            "Actor":{"ID":"busy:1","Attributes":{"name":"busy:1"}}});
        assert!(f.matches(&ev), "image filter must match the name attribute");
        let other = json!({"Type":"image","Action":"pull",
            "Actor":{"ID":"other:2","Attributes":{"name":"other:2"}}});
        assert!(!f.matches(&other));
    }

    // "event=health_status Filter Misses Health Transitions": actions are `health_status: unhealthy`;
    // the `health_status` filter matches on the action key before `:`.
    #[test]
    fn action_filter_matches_health_status_key() {
        let f = Filters::parse(&Some("{\"event\":[\"health_status\"]}".into())).unwrap();
        let ev = json!({"Type":"container","Action":"health_status: unhealthy",
            "Actor":{"ID":"c1","Attributes":{}}});
        assert!(f.matches(&ev), "health_status must match the compound action key");
    }

    // "Event Filters Broaden To Match-All For Supported Keys": label/network/volume/scope must NARROW,
    // not leak unrelated events.
    #[test]
    fn label_network_volume_scope_filters_narrow() {
        // label=app=web matches only events whose attributes carry that label.
        let f = Filters::parse(&Some("{\"label\":[\"app=web\"]}".into())).unwrap();
        let hit = json!({"Type":"container","Action":"create",
            "Actor":{"ID":"c1","Attributes":{"app":"web"}}});
        let miss = json!({"Type":"container","Action":"create",
            "Actor":{"ID":"c2","Attributes":{"app":"db"}}});
        assert!(f.matches(&hit));
        assert!(!f.matches(&miss), "label filter must not leak a non-matching event");

        // network=frontend only matches network events for that network (or events tagged with it).
        let fnet = Filters::parse(&Some("{\"network\":[\"frontend\"]}".into())).unwrap();
        let net_ev = json!({"Type":"network","Action":"connect",
            "Actor":{"ID":"netid","Attributes":{"name":"frontend"}}});
        let unrelated = json!({"Type":"container","Action":"start",
            "Actor":{"ID":"c1","Attributes":{}}});
        assert!(fnet.matches(&net_ev));
        assert!(!fnet.matches(&unrelated), "network filter must not leak unrelated container events");

        // volume=cache narrows to that volume.
        let fvol = Filters::parse(&Some("{\"volume\":[\"cache\"]}".into())).unwrap();
        let vol_ev = json!({"Type":"volume","Action":"destroy",
            "Actor":{"ID":"cache","Attributes":{"name":"cache"}}});
        assert!(fvol.matches(&vol_ev));
        assert!(!fvol.matches(&unrelated));

        // scope=swarm narrows on the event scope (dd only emits local).
        let fscope = Filters::parse(&Some("{\"scope\":[\"swarm\"]}".into())).unwrap();
        let local_ev = json!({"Type":"container","Action":"start","scope":"local",
            "Actor":{"ID":"c1","Attributes":{}}});
        assert!(!fscope.matches(&local_ev), "scope=swarm must not match a local event");
    }
}
