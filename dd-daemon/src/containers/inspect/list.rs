//! `docker ps` — the container list, with `--all`/`--filter`/`--size` and docker's newest-first order.
use super::super::*;
use super::detail::container_mounts_json;
use super::filter::*;

#[derive(Deserialize)]
pub(crate) struct PsQ {
    all: Option<String>,
    filters: Option<String>,
    size: Option<String>,
}

pub(crate) async fn containers_json(
    State(a): State<App>,
    Query(q): Query<PsQ>,
) -> Json<Vec<crate::api::ContainerSummary>> {
    let all = matches!(q.all.as_deref(), Some("1") | Some("true") | Some("True"));
    // `filters` arrives URL-encoded JSON; axum has already percent-decoded it. Bad JSON => no filters.
    // Docker encodes it as map[key]->{value:true} (e.g. {"name":{"web":true}}), older clients as
    // map[key]->[value]. Accept BOTH: decode to a generic Value, normalize to key -> [values].
    let filters: HashMap<String, Vec<String>> = q
        .filters
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| {
            v.as_object().map(|m| {
                m.iter()
                    .map(|(k, val)| {
                        let vals = match val {
                            Value::Object(set) => set.keys().cloned().collect(), // {"web":true}
                            Value::Array(a) => a
                                .iter()
                                .filter_map(|x| x.as_str().map(String::from))
                                .collect(), // ["web"]
                            _ => vec![],
                        };
                        (k.clone(), vals)
                    })
                    .collect()
            })
        })
        .unwrap_or_default();
    // A `status` filter implies "show all matching" (like `docker ps --filter status=exited`).
    let status_filter = filters.contains_key("status");
    // `--size`: docker only computes SizeRw/SizeRootFs on demand (a per-container rootfs walk is
    // expensive). Gather the matching containers, release the lock, THEN walk the disk so the daemon
    // lock isn't held across the (synchronous) `du`.
    let want_size = q_truthy(&q.size);
    let mut matched: Vec<Container> = {
        let g = a.inner.lock().await;
        // `before=`/`since=` name a reference container (by id-prefix or name); resolve each to that
        // container's `created` time so ps_match can compare create-order against it.
        let resolve = |key: &str| -> Option<i64> {
            filters
                .get(key)
                .and_then(|vals| vals.first())
                .and_then(|r| {
                    g.containers
                        .values()
                        .find(|c| c.id.starts_with(r.as_str()) || &c.name == r)
                        .map(|c| c.created)
                })
        };
        let before_ts = resolve("before");
        let since_ts = resolve("since");
        // A before/since (like status) filter implies "show all matching", not just running.
        let order_filter = filters.contains_key("before") || filters.contains_key("since");
        g.containers
            .values()
            .filter(|c| {
                // Default `docker ps` (no -a) lists containers with State.Running == true, which in
                // Moby INCLUDES paused ones (rendered "Up … (Paused)"), plus restarting.
                all || status_filter
                    || order_filter
                    || c.status == "running"
                    || c.status == "paused"
                    || c.status == "restarting"
            })
            .filter(|c| {
                let name = if c.name.is_empty() {
                    c.id[..12.min(c.id.len())].to_string()
                } else {
                    c.name.clone()
                };
                ps_match(c, &name, &filters, before_ts, since_ts)
            })
            .cloned()
            .collect()
    };
    // Resolve named-volume mountpoints for the Mounts array below (the g lock is released after the block
    // above, so snapshot the volume set here).
    let vols_snapshot: Vec<Vol> = { a.inner.lock().await.volumes.clone() };
    // Docker lists containers newest-first (by creation time); our container map is unordered, so a raw
    // walk yields an arbitrary order and `docker ps`/`ps -q` would return IDs in an unpredictable order.
    // Sort descending by `created` (tie-break on `started_at`) to match docker's ordering.
    matched.sort_by(|a, b| {
        b.created
            .cmp(&a.created)
            .then(b.started_at.cmp(&a.started_at))
    });
    let v: Vec<crate::api::ContainerSummary> = matched
        .iter()
        .map(|c| {
            // Only emit the size keys when requested -- docker omits them otherwise.
            let (size_rw, size_root_fs) = if want_size {
                let (rw, rootfs) = container_sizes(c);
                (Some(rw), Some(rootfs))
            } else {
                (None, None)
            };
            crate::api::ContainerSummary {
                id: c.id.clone(),
                image: c.image.clone(),
                command: c.cmd.join(" "),
                created: c.created,
                state: c.status.clone(),
                status: human_status(c),
                exit_code: c.exit_code,
                ports: ports_json(&c.publish),
                labels: c.labels.clone(),
                mounts: container_mounts_json(&vols_snapshot, c),
                names: vec![format!(
                    "/{}",
                    if c.name.is_empty() {
                        c.id[..12.min(c.id.len())].to_string()
                    } else {
                        c.name.clone()
                    }
                )],
                size_rw,
                size_root_fs,
            }
        })
        .collect();
    Json(v)
}
