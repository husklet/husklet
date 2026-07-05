#![allow(unused_imports, dead_code)]
//! `docker inspect` — the full container detail document, plus the resolved `Mounts[]` builder it
//! (and `docker ps`) share.
use super::super::*;

/// The resolved `Mounts[]` array a docker client reads — the §6.3-9 repair. Covers ALL mount surfaces
/// with the fidelity docker reports: `-v`/Binds split into Type=bind (absolute host src) vs Type=volume
/// (a named volume, carrying Name + resolved Source mountpoint + Driver), `--mount` bind/volume specs
/// (volume ones likewise carry Name/Driver), and `--tmpfs`/`type=tmpfs` as Type=tmpfs. Previously only
/// `c.binds` were enumerated and everything was hardcoded Type=bind (Name/Driver/tmpfs all dropped).
pub(crate) fn container_mounts_json(vols: &[Vol], c: &Container) -> Vec<Value> {
    let vol_mp = |name: &str| {
        vols.iter()
            .find(|v| v.name == name)
            .map(|v| v.mountpoint.clone())
            .unwrap_or_default()
    };
    let mut out: Vec<Value> = Vec::new();
    let to_val = |m: crate::api::MountPoint| serde_json::to_value(m).unwrap_or(Value::Null);
    for b in &c.binds {
        if let Some((src, dst, ro)) = parse_bind(b) {
            if src.starts_with('/') {
                out.push(to_val(crate::api::MountPoint {
                    type_: "bind".into(),
                    name: None,
                    source: src.to_string(),
                    destination: dst.to_string(),
                    driver: None,
                    mode: Some(if ro { "ro" } else { "" }.to_string()),
                    rw: !ro,
                    propagation: "rprivate",
                }));
            } else {
                out.push(to_val(crate::api::MountPoint {
                    type_: "volume".into(),
                    name: Some(src.to_string()),
                    source: vol_mp(src),
                    destination: dst.to_string(),
                    driver: Some("local"),
                    mode: Some(if ro { "ro" } else { "z" }.to_string()),
                    rw: !ro,
                    propagation: "",
                }));
            }
        }
    }
    for m in &c.mounts {
        if m.typ == "volume" {
            out.push(to_val(crate::api::MountPoint {
                type_: "volume".into(),
                name: Some(m.source.clone()),
                source: vol_mp(&m.source),
                destination: m.target.clone(),
                driver: Some("local"),
                mode: None,
                rw: !m.read_only,
                propagation: "",
            }));
        } else {
            out.push(to_val(crate::api::MountPoint {
                type_: m.typ.clone(),
                name: None,
                source: m.source.clone(),
                destination: m.target.clone(),
                driver: None,
                mode: None,
                rw: !m.read_only,
                propagation: "rprivate",
            }));
        }
    }
    for target in c.tmpfs.keys() {
        out.push(
            serde_json::to_value(crate::api::TmpfsMountPoint {
                type_: "tmpfs",
                source: "",
                destination: target.clone(),
                rw: true,
                mode: "",
            })
            .unwrap_or(Value::Null),
        );
    }
    out
}

pub(crate) async fn containers_inspect(State(a): State<App>, Path(id): Path<String>) -> Response {
    let g = a.inner.lock().await;
    let Some(full) = resolve_cid(&g, &id) else {
        return no_such(&id);
    };
    match g.containers.get(&full) {
        Some(c) => {
            let running = c.status == "running" || c.status == "paused";
            // Pid = the live JIT process pid while running, else 0 (docker reports 0 for stopped).
            let pid = if running {
                g.live
                    .get(&full)
                    .and_then(|l| *l.pid.lock().unwrap())
                    .unwrap_or(0)
            } else {
                0
            };
            // StartedAt/FinishedAt as RFC3339; docker's zero value is "0001-01-01T00:00:00Z".
            // Nanosecond precision (docker's shape) so a quick `docker restart` advances StartedAt even
            // within one wall-clock second; fall back to the second-precise field for pre-upgrade state.
            let started_at = if c.started_at == 0 {
                "0001-01-01T00:00:00Z".to_string()
            } else if c.started_at_ns > 0 {
                fmt_rfc3339_nanos(c.started_at_ns)
            } else {
                fmt_rfc3339(c.started_at)
            };
            let finished_at = if c.finished_at == 0 {
                "0001-01-01T00:00:00Z".to_string()
            } else if c.finished_at_ns > 0 {
                fmt_rfc3339_nanos(c.finished_at_ns)
            } else {
                fmt_rfc3339(c.finished_at)
            };
            // Networks the container has joined -> NetworkSettings.Networks, with the IPAM-assigned
            // identity (IP/gateway/mac) per network. A BTreeMap keeps them name-sorted, matching the
            // prior `serde_json::Map` (BTreeMap-backed) collection order.
            let networks: std::collections::BTreeMap<String, crate::api::EndpointJson> = g
                .networks
                .iter()
                .filter_map(|n| {
                    n.endpoints.get(&c.id).map(|e| {
                        (
                            n.name.clone(),
                            crate::api::EndpointJson {
                                network_id: n.id.clone(),
                                ip_address: e.ip.clone(),
                                gateway: n.gateway.clone(),
                                ip_prefix_len: n
                                    .subnet
                                    .split('/')
                                    .nth(1)
                                    .and_then(|p| p.parse::<i64>().ok())
                                    .unwrap_or(16),
                                mac_address: ip_mac(&e.ip),
                            },
                        )
                    })
                })
                .collect();
            // Top-level NetworkSettings.IPAddress = the primary endpoint IP (first joined network).
            let primary = g
                .networks
                .iter()
                .find_map(|n| {
                    n.endpoints
                        .get(&c.id)
                        .map(|e| (e.ip.clone(), n.gateway.clone()))
                })
                .unwrap_or_default();
            // State, adding `Health` only when a HEALTHCHECK is configured (docker omits it otherwise).
            // §8.3-4: the always-present state booleans docker emits (Restarting from the backoff window;
            // OOMKilled/Dead not modelled by dd's reaper, reported false; Error empty). Running stays true
            // through the restart backoff (Moby keeps Running=true while restarting).
            let restarting = c.status == "restarting";
            let state = crate::api::ContainerState {
                status: c.status.clone(),
                exit_code: c.exit_code,
                running: running || restarting,
                paused: c.status == "paused",
                restarting,
                oom_killed: false,
                dead: false,
                error: String::new(),
                pid: pid as i64,
                started_at,
                finished_at,
                // `State.Health` reuses the model's `HealthState` (its Serialize carries the exact keys).
                health: c.health.clone(),
            };
            // Config.Healthcheck / StopSignal round-trip so a client diffs the resolved lifecycle config
            // (Healthcheck reuses the model's `HealthConfig`; both are `null` when unset).
            let config = crate::api::ContainerConfig {
                cmd: c.cmd.clone(),
                hostname: c.hostname.clone(),
                image: c.image.clone(),
                env: c.env.clone(),
                labels: c.labels.clone(),
                healthcheck: c.healthcheck.clone(),
                stop_signal: if c.stop_signal.is_empty() {
                    None
                } else {
                    Some(c.stop_signal.clone())
                },
            };
            // HostConfig round-trips the fidelity extras verbatim so docker clients diff them cleanly.
            let host_config = crate::api::HostConfigJson {
                binds: c.binds.clone(),
                memory: c.memory,
                pids_limit: c.pids_limit,
                nano_cpus: c.nano_cpus,
                readonly_rootfs: c.readonly_rootfs,
                ulimits: c.ulimits.clone(),
                restart_policy: c.restart_policy.clone(),
                cap_add: c.cap_add.clone(),
                cap_drop: c.cap_drop.clone(),
                devices: c.devices.clone(),
                mounts: c.mounts.clone(),
                tmpfs: c.tmpfs.clone(),
                stop_timeout: c.stop_timeout,
                privileged: c.privileged,
                security_opt: c.security_opt.clone(),
            };
            Json(crate::api::ContainerInspect {
                id: c.id.clone(),
                image: c.image.clone(),
                created: fmt_rfc3339(c.created),
                name: format!(
                    "/{}",
                    if c.name.is_empty() {
                        c.id[..12.min(c.id.len())].to_string()
                    } else {
                        c.name.clone()
                    }
                ),
                state,
                config,
                restart_count: c.restart_count,
                // Top-level resolved Mounts (bind/volume/tmpfs, with volume Name/Driver).
                mounts: container_mounts_json(&g.volumes, c),
                host_config,
                // NetworkSettings present so `docker port` (reads .NetworkSettings.Ports) doesn't panic.
                network_settings: crate::api::NetworkSettingsJson {
                    ports: ports_map_json(&c.publish),
                    ip_address: primary.0,
                    gateway: primary.1,
                    networks,
                },
            })
            .into_response()
        }
        None => no_such(&id),
    }
}
