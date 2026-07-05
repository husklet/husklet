#![allow(unused_imports, dead_code)]
//! Published-port string assembly for `POST /containers/create`: turn the
//! HostConfig.PortBindings map into the daemon's `ip:hport:cport/proto` publish
//! string (`publish_str`), with an auto-allocating variant (`publish_str_alloc`)
//! that fills empty HostPorts from the ephemeral range. Stateless helpers.
use super::super::super::*;

use super::dto::PortBinding;

/// Split a PortBindings key (`"<cport>/<proto>"`, e.g. `"9000/tcp"`) into (container-port, proto).
fn split_key(k: &str) -> (&str, &str) {
    k.split_once('/').unwrap_or((k, "tcp"))
}

pub(crate) fn publish_str(pb: &HashMap<String, Vec<PortBinding>>) -> String {
    let mut v = Vec::new();
    for (k, binds) in pb {
        let (cport, proto) = split_key(k);
        if cport.is_empty() {
            continue;
        }
        for b in binds {
            if let Some(hp) = &b.host_port {
                if !hp.is_empty() {
                    let ip = b.host_ip.as_deref().unwrap_or("");
                    v.push(format!("{ip}:{hp}:{cport}/{proto}"));
                }
            }
        }
    }
    v.join(",")
}

/// Like [`publish_str`] but AUTO-ASSIGNS a free host port for any binding with an empty `HostPort` —
/// docker's `-p <container>` / `-p 127.0.0.1::<container>` "publish to an ephemeral host port" form. The
/// daemon picks the port here (from the IANA dynamic range 49152-65535) so `docker port`/`ps`/inspect
/// report a concrete host port and the engine's `-p` host forwarder binds it. Ports already published by
/// existing containers are skipped to avoid intra-daemon collisions. Bindings with an explicit HostPort
/// are emitted verbatim (byte-identical to `publish_str`).
pub(crate) fn publish_str_alloc(pb: &HashMap<String, Vec<PortBinding>>, g: &Inner) -> String {
    let mut used: std::collections::HashSet<u16> = g
        .containers
        .values()
        .flat_map(|c| crate::containers::parse_publish(&c.publish))
        .map(|p| p.host_port)
        .collect();
    let mut next: u16 = 49152;
    let mut alloc = || -> u16 {
        while next < 65535 && used.contains(&next) {
            next += 1;
        }
        let p = next;
        used.insert(p);
        next = next.saturating_add(1);
        p
    };
    // Sort by container port so auto-assignment is deterministic (HashMap iteration order is not).
    let mut keys: Vec<&String> = pb.keys().collect();
    keys.sort();
    let mut v = Vec::new();
    for k in keys {
        let (cport, proto) = split_key(k);
        if cport.is_empty() {
            continue;
        }
        for b in &pb[k] {
            let hp = match &b.host_port {
                Some(h) if !h.is_empty() => h.clone(),
                _ => alloc().to_string(),
            };
            let ip = b.host_ip.as_deref().unwrap_or("");
            v.push(format!("{ip}:{hp}:{cport}/{proto}"));
        }
    }
    v.join(",")
}
