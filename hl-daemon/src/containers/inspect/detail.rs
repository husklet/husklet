//! `docker inspect` — the full container detail document. The resolved `Mounts[]` builder it
//! (and `docker ps`) share now lives in the sibling `mounts` module.
use super::super::*;
// `container_mounts_json` moved to `mounts`; re-exported here so the async handler below and the
// `super::detail::container_mounts_json` path used by `list` resolve unchanged.
pub(super) use super::mounts::container_mounts_json;

pub(crate) async fn containers_inspect(State(a): State<App>, Path(id): Path<String>) -> Response {
    let g = a.inner.lock().await;
    let Some((full, c)) = resolve_get(&g, &id) else {
        return no_such(&id);
    };
    {
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
                                endpoint_id: c.id.clone(),
                                ip_address: e.ip.clone(),
                                gateway: n.gateway.clone(),
                                ip_prefix_len: n
                                    .subnet
                                    .split('/')
                                    .nth(1)
                                    .and_then(|p| p.parse::<i64>().ok())
                                    .unwrap_or(16),
                                mac_address: ip_mac(&e.ip),
                                aliases: e.aliases.clone(),
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
            // OOMKilled/Dead not modelled by hl's reaper, reported false; Error empty). Running stays true
            // through the restart backoff (Moby keeps Running=true while restarting).
            let restarting = c.status == "restarting";
            let state = crate::api::ContainerState {
                status: c.status.clone(),
                exit_code: c.exit_code,
                running: running || restarting,
                paused: c.status == "paused",
                restarting,
                oom_killed: false,
                // State.Dead must agree with a `dead` lifecycle status (docker sets both together);
                // reporting Status="dead" with Dead=false is a self-contradictory inspect document.
                dead: c.status == "dead",
                error: String::new(),
                pid: pid as i64,
                started_at,
                finished_at,
                // `State.Health` reuses the model's `HealthState` (its Serialize carries the exact keys).
                health: c.health.clone(),
            };
            // Config.Healthcheck / StopSignal round-trip so a client diffs the resolved lifecycle config
            // (Healthcheck reuses the model's `HealthConfig`; both are `null` when unset).
            // Config.ExposedPorts: the declared-port set as docker's `{"8080/tcp":{}}` object.
            let exposed_ports = {
                let mut m = serde_json::Map::new();
                for p in &c.exposed_ports {
                    m.insert(p.clone(), serde_json::json!({}));
                }
                serde_json::Value::Object(m)
            };
            // Docker keeps Entrypoint/Cmd SPLIT and reports WorkingDir/User/Domainname/interactive flags.
            let config = crate::api::ContainerConfig {
                hostname: c.hostname.clone(),
                domainname: c.domainname.clone(),
                user: c.user.clone(),
                image: c.image.clone(),
                entrypoint: c.entrypoint.clone(),
                cmd: c.cmd_config.clone(),
                working_dir: c.working_dir.clone(),
                env: c.env.clone(),
                tty: c.tty,
                open_stdin: c.open_stdin,
                stdin_once: c.stdin_once,
                exposed_ports,
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
                network_mode: c.network_mode.clone(),
                auto_remove: c.auto_remove,
                log_config: c.log_config.clone().unwrap_or_default(),
                dns: c.dns.clone(),
                dns_search: c.dns_search.clone(),
                dns_options: c.dns_options.clone(),
                extra_hosts: c.extra_hosts.clone(),
                device_requests: c.device_requests.clone(),
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
                    ports: {
                        // Published ports carry their host bindings; exposed-but-unpublished ports appear
                        // as `null` (docker parity) so `Config.ExposedPorts` is reflected here too.
                        let mut ports = ports_map_json(&c.publish);
                        if let Some(obj) = ports.as_object_mut() {
                            for p in &c.exposed_ports {
                                obj.entry(p.clone()).or_insert(serde_json::Value::Null);
                            }
                        }
                        ports
                    },
                    ip_address: primary.0,
                    gateway: primary.1,
                    networks,
                },
            })
            .into_response()
    }
}
