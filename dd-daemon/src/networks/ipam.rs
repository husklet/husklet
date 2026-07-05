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

// ---- networks --------------------------------------------------------------

/// CIDR prefix length of a subnet ("172.18.0.0/16" -> 16), defaulting to /16.
fn subnet_prefix(subnet: &str) -> u32 {
    subnet
        .split('/')
        .nth(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or(16)
}

/// Deterministic MAC for an IPv4 (Docker convention): `02:42:` + the four address bytes. Cosmetic.
pub(crate) fn ip_mac(ip: &str) -> String {
    let o: Vec<u8> = ip.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    if o.len() == 4 {
        format!("02:42:{:02x}:{:02x}:{:02x}:{:02x}", o[0], o[1], o[2], o[3])
    } else {
        "02:42:00:00:00:00".into()
    }
}

/// Pick the next free `/16` from the `172.18.0.0/12` pool, skipping subnets already in use. Returns
/// `(subnet, gateway)`. `bridge` is special-cased to `172.17.0.0/16` by the caller.
pub(crate) fn alloc_subnet(nets: &[Net]) -> (String, String) {
    for o in 18u32..=31 {
        let sub = format!("172.{o}.0.0/16");
        if !nets.iter().any(|n| n.subnet == sub) {
            return (sub, format!("172.{o}.0.1"));
        }
    }
    ("172.18.0.0/16".into(), "172.18.0.1".into()) // pool exhausted — degrade rather than fail
}

/// Next free host address in a network's subnet (`.1` reserved for the gateway, hosts start at `.2`).
/// Assumes a `/16` "172.B.0.0" subnet — IPs are handed out as `172.B.0.N`.
pub(crate) fn alloc_ip(net: &Net) -> String {
    let base = net.subnet.split('/').next().unwrap_or("172.18.0.0");
    let p: Vec<&str> = base.split('.').collect();
    let (a, b) = (
        p.first().copied().unwrap_or("172"),
        p.get(1).copied().unwrap_or("18"),
    );
    let used: std::collections::HashSet<&str> =
        net.endpoints.values().map(|e| e.ip.as_str()).collect();
    for k in 2u32..=254 {
        let ip = format!("{a}.{b}.0.{k}");
        if !used.contains(ip.as_str()) {
            return ip;
        }
    }
    format!("{a}.{b}.0.2")
}

/// Join container `cid` (reporting as `cname`) to the network named `net_name` in `nets`: lazily
/// allocate the subnet if absent (e.g. for `bridge` from old state), assign a fresh endpoint IP, and
/// add the cid to the membership list. Idempotent — re-joining returns the existing IP. Returns the IP.
pub(crate) fn join_network(
    nets: &mut [Net],
    net_name: &str,
    cid: &str,
    cname: &str,
) -> Option<String> {
    let idx = nets.iter().position(|n| n.name == net_name)?;
    if nets[idx].subnet.is_empty() {
        let (sub, gw) = if net_name == "bridge" {
            ("172.17.0.0/16".into(), "172.17.0.1".into())
        } else {
            alloc_subnet(nets)
        };
        nets[idx].subnet = sub;
        nets[idx].gateway = gw;
    }
    let n = &mut nets[idx];
    if let Some(e) = n.endpoints.get(cid) {
        return Some(e.ip.clone());
    }
    let ip = alloc_ip(n);
    if !n.containers.iter().any(|c| c == cid) {
        n.containers.push(cid.to_string());
    }
    n.endpoints.insert(
        cid.to_string(),
        Endpoint {
            name: cname.to_string(),
            ip: ip.clone(),
        },
    );
    Some(ip)
}

/// Drop a container from a network (membership + endpoint IP). Frees the IP for reuse.
pub(crate) fn leave_network(n: &mut Net, cid: &str) {
    n.containers.retain(|c| c != cid);
    n.endpoints.remove(cid);
}

pub(crate) fn net_json(n: &Net) -> crate::api::NetworkJson {
    use crate::api::{Ipam, IpamConfig, NetContainer, NetworkJson};
    let prefix = subnet_prefix(&n.subnet);
    let containers: HashMap<String, NetContainer> = n
        .endpoints
        .iter()
        .map(|(cid, e)| {
            (
                cid.clone(),
                NetContainer {
                    name: e.name.clone(),
                    endpoint_id: cid.clone(),
                    mac_address: ip_mac(&e.ip),
                    ipv4_address: format!("{}/{}", e.ip, prefix),
                    ipv6_address: String::new(),
                },
            )
        })
        .collect();
    let config = if n.subnet.is_empty() {
        vec![]
    } else {
        vec![IpamConfig {
            subnet: n.subnet.clone(),
            gateway: n.gateway.clone(),
        }]
    };
    NetworkJson {
        id: n.id.clone(),
        name: n.name.clone(),
        driver: n.driver.clone(),
        scope: n.scope.clone(),
        containers,
        created: fmt_rfc3339(n.created),
        enable_ipv6: false,
        internal: false,
        ipam: Ipam {
            driver: "default",
            config,
        },
    }
}

/// The name a container is reported by on a network: its `--name`, or the 12-char short id.
pub(crate) fn endpoint_name(c: &Container) -> String {
    if c.name.is_empty() {
        c.id[..12.min(c.id.len())].to_string()
    } else {
        c.name.clone()
    }
}

pub(crate) fn net_matches(n: &Net, id: &str) -> bool {
    n.id == id || n.name == id || n.id.starts_with(id)
}

pub(crate) fn is_predefined(name: &str) -> bool {
    matches!(name, "bridge" | "host" | "none")
}

pub(crate) fn default_networks() -> Vec<Net> {
    ["bridge", "host", "none"]
        .iter()
        .map(|name| Net {
            id: fake_id(&format!("net-{name}")),
            name: name.to_string(),
            driver: if *name == "bridge" {
                "bridge".into()
            } else {
                name.to_string()
            },
            created: 0,
            scope: "local".into(),
            containers: vec![],
            // bridge is the default network a container without `--network` lands on, so it gets the
            // canonical 172.17.0.0/16; host/none carry no L3 identity.
            subnet: if *name == "bridge" {
                "172.17.0.0/16".into()
            } else {
                String::new()
            },
            gateway: if *name == "bridge" {
                "172.17.0.1".into()
            } else {
                String::new()
            },
            endpoints: HashMap::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_net(subnet: &str) -> Net {
        Net {
            id: "id".into(),
            name: "n".into(),
            driver: "bridge".into(),
            scope: "local".into(),
            containers: vec![],
            created: 0,
            subnet: subnet.into(),
            gateway: String::new(),
            endpoints: HashMap::new(),
        }
    }

    fn net_with_subnet(subnet: &str) -> Net {
        let mut n = mk_net("");
        n.subnet = subnet.into();
        n
    }

    #[test]
    fn alloc_subnet_empty_pool() {
        // No existing networks — first free /16 is 172.18.0.0/16, gateway .1.
        let (sub, gw) = alloc_subnet(&[]);
        assert_eq!(sub, "172.18.0.0/16");
        assert_eq!(gw, "172.18.0.1");
    }

    #[test]
    fn alloc_subnet_skips_occupied() {
        // 172.18 and 172.19 taken -> next free is 172.20.0.0/16.
        let nets = vec![net_with_subnet("172.18.0.0/16"), net_with_subnet("172.19.0.0/16")];
        let (sub, gw) = alloc_subnet(&nets);
        assert_eq!(sub, "172.20.0.0/16");
        assert_eq!(gw, "172.20.0.1");
    }

    #[test]
    fn alloc_subnet_exhausted_pool_falls_back() {
        // All of 172.18..=172.31 occupied -> degrade to 172.18.0.0/16 rather than fail.
        let nets: Vec<Net> = (18u32..=31)
            .map(|o| net_with_subnet(&format!("172.{o}.0.0/16")))
            .collect();
        let (sub, gw) = alloc_subnet(&nets);
        assert_eq!(sub, "172.18.0.0/16");
        assert_eq!(gw, "172.18.0.1");
    }

    #[test]
    fn alloc_ip_first_is_dot2() {
        // .1 is the reserved gateway, so hosts start at .2.
        let n = net_with_subnet("172.18.0.0/16");
        assert_eq!(alloc_ip(&n), "172.18.0.2");
    }

    #[test]
    fn alloc_ip_skips_used() {
        let mut n = net_with_subnet("172.18.0.0/16");
        n.endpoints.insert(
            "c1".into(),
            Endpoint { name: "c1".into(), ip: "172.18.0.2".into() },
        );
        n.endpoints.insert(
            "c2".into(),
            Endpoint { name: "c2".into(), ip: "172.18.0.3".into() },
        );
        assert_eq!(alloc_ip(&n), "172.18.0.4");
    }

    #[test]
    fn alloc_ip_exhausted_falls_back_to_dot2() {
        let mut n = net_with_subnet("172.18.0.0/16");
        for k in 2u32..=254 {
            n.endpoints.insert(
                format!("c{k}"),
                Endpoint { name: format!("c{k}"), ip: format!("172.18.0.{k}") },
            );
        }
        // Every .2..=.254 taken -> degrade to .2.
        assert_eq!(alloc_ip(&n), "172.18.0.2");
    }

    #[test]
    fn ip_mac_deterministic() {
        assert_eq!(ip_mac("172.18.0.2"), "02:42:ac:12:00:02");
    }

    #[test]
    fn ip_mac_malformed_input() {
        // Not four dotted octets -> the fixed placeholder.
        assert_eq!(ip_mac("not-an-ip"), "02:42:00:00:00:00");
    }
}
