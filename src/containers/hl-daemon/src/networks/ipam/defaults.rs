//! Static predefined-network data — the three docker built-ins (`bridge`/`host`/`none`).

use crate::model::*;
use crate::prelude::*;
use crate::util::*;

impl Net {
    pub(crate) fn is_predefined(&self) -> bool {
        matches!(self.name.as_str(), "bridge" | "host" | "none")
    }
}

pub(crate) fn default_networks() -> Vec<Net> {
    ["bridge", "host", "none"]
        .iter()
        .map(|name| Net {
            id: Digest::fake(&format!("net-{name}")),
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

    // ---- default_networks ---------------------------------------------------
    fn find<'a>(nets: &'a [Net], name: &str) -> &'a Net {
        nets.iter()
            .find(|n| n.name == name)
            .unwrap_or_else(|| panic!("missing default network {name:?}"))
    }

    #[test]
    fn default_networks_are_the_three_predefined() {
        // Docker's three built-ins, in order, all local-scoped.
        let nets = default_networks();
        let names: Vec<&str> = nets.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["bridge", "host", "none"]);
        assert!(nets.iter().all(|n| n.scope == "local"));
        // All predefined per is_predefined, all with a stable non-empty id and no members.
        assert!(nets.iter().all(|n| n.is_predefined()));
        assert!(nets.iter().all(|n| !n.id.is_empty()));
        assert!(nets
            .iter()
            .all(|n| n.containers.is_empty() && n.endpoints.is_empty()));
    }

    #[test]
    fn default_networks_bridge_has_canonical_l3() {
        // bridge is the default landing network: driver "bridge", 172.17.0.0/16 with .1 gateway.
        let nets = default_networks();
        let bridge = find(&nets, "bridge");
        assert_eq!(bridge.driver, "bridge");
        assert_eq!(bridge.subnet, "172.17.0.0/16");
        assert_eq!(bridge.gateway, "172.17.0.1");
    }

    #[test]
    fn default_networks_host_and_none_carry_no_l3() {
        // host/none use their name as the driver and have no subnet/gateway (no L3 identity).
        let nets = default_networks();
        for name in ["host", "none"] {
            let n = find(&nets, name);
            assert_eq!(n.driver, name);
            assert!(n.subnet.is_empty(), "{name} should have no subnet");
            assert!(n.gateway.is_empty(), "{name} should have no gateway");
        }
    }

    #[test]
    fn default_networks_ids_are_deterministic() {
        // ids come from Digest::fake(net-<name>), so two builds produce identical ids.
        let a = default_networks();
        let b = default_networks();
        for (na, nb) in a.iter().zip(b.iter()) {
            assert_eq!(na.id, nb.id, "id for {} not deterministic", na.name);
        }
    }
}
