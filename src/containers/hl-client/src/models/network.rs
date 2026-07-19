use super::*;

/// One network from `GET /networks`.
#[derive(Debug, Clone, Default)]
pub struct Network {
    /// Full network id.
    pub id: String,
    /// Network name.
    pub name: String,
    /// Network driver backing it (e.g. `bridge`, `host`, `null`).
    pub driver: String,
    /// Scope the network is valid in (e.g. `local`, `swarm`).
    pub scope: String,
    /// IPAM subnet in CIDR form (first config entry), or empty.
    pub subnet: String,
    /// IPAM gateway address (first config entry), or empty.
    pub gateway: String,
    /// True if the network is internal (no external connectivity).
    pub internal: bool,
    /// True if containers can be attached to it manually.
    pub attachable: bool,
    /// True if IPv6 is enabled on the network.
    pub ipv6: bool,
    /// User-defined labels, sorted by key.
    pub metadata: Metadata,
    /// IDs of the containers attached to this network (from the inspect `Containers` map).
    pub containers: Vec<String>,
    /// ISO-8601 creation time (sorts chronologically as a string) — for newest-first sorting.
    pub created_at: String,
}

impl From<bollard::models::Network> for Network {
    fn from(n: bollard::models::Network) -> Self {
        let (subnet, gateway) = n
            .ipam
            .and_then(|i| i.config)
            .and_then(|c| c.into_iter().next())
            .map(|c| (c.subnet.unwrap_or_default(), c.gateway.unwrap_or_default()))
            .unwrap_or_default();
        Network {
            id: n.id.unwrap_or_default(),
            name: n.name.unwrap_or_default(),
            driver: n.driver.unwrap_or_default(),
            scope: n.scope.unwrap_or_default(),
            subnet,
            gateway,
            internal: n.internal.unwrap_or_default(),
            attachable: n.attachable.unwrap_or_default(),
            ipv6: n.enable_ipv6.unwrap_or_default(),
            metadata: Metadata::new(n.labels.unwrap_or_default(), n.options.unwrap_or_default()),
            // bollard's list `Network` model carries no container map (only on inspect).
            containers: Vec::new(),
            created_at: n.created.unwrap_or_default(),
        }
    }
}

impl Network {
    /// Short 12-char id.
    pub fn short_id(&self) -> String {
        self.id
            .trim_start_matches("sha256:")
            .chars()
            .take(12)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_short_id_delegates_to_short() {
        let n = Network {
            id: "abcdef0123456789ffff".into(),
            ..Default::default()
        };
        assert_eq!(n.short_id(), "abcdef012345");
    }
}
