use super::*;

/// One network from `GET /networks`.
#[derive(Debug, Clone, Default)]
pub struct Network {
    pub id: String,
    pub name: String,
    pub driver: String,
    pub scope: String,
    pub subnet: String,
    pub gateway: String,
    pub internal: bool,
    pub attachable: bool,
    pub ipv6: bool,
    pub labels: Vec<(String, String)>,
    pub options: Vec<(String, String)>,
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
            labels: sorted_pairs(n.labels.unwrap_or_default()),
            options: sorted_pairs(n.options.unwrap_or_default()),
            // bollard's list `Network` model carries no container map (only on inspect).
            containers: Vec::new(),
            created_at: n.created.unwrap_or_default(),
        }
    }
}

impl Network {
    /// Short 12-char id.
    pub fn short_id(&self) -> String {
        short(&self.id)
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
