use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[cfg(feature = "runtime")]
use std::collections::BTreeSet;

/// Docker's string-keyed container port declaration map.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(transparent)]
pub struct ExposedPorts(pub BTreeMap<String, serde_json::Value>);

/// One Docker host-side port binding.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct PortBinding {
    #[serde(default)]
    pub host_ip: String,
    #[serde(default)]
    pub host_port: String,
}

/// Docker's container-port to host-bindings map.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(transparent)]
pub struct PortBindings(pub BTreeMap<String, Option<Vec<PortBinding>>>);

/// Docker list-view representation of one exposed or published port.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct PortSummary {
    #[serde(rename = "IP", skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    pub private_port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_port: Option<u16>,
    #[serde(rename = "Type")]
    pub protocol: String,
}

impl PortSummary {
    #[must_use]
    pub(crate) fn display(&self) -> String {
        let protocol = if self.protocol.is_empty() {
            "tcp"
        } else {
            &self.protocol
        };
        self.public_port.map_or_else(
            || format!("{}/{protocol}", self.private_port),
            |public| format!("{public}->{}/{protocol}", self.private_port),
        )
    }
}

#[cfg(feature = "runtime")]
impl PortBindings {
    pub(crate) async fn ports(
        &self,
        exposed: &ExposedPorts,
        containers: &hl_container::Containers,
    ) -> Result<(BTreeSet<hl_container::Port>, Vec<hl_container::Publication>), String> {
        let mut used = BTreeSet::new();
        for container in containers.list().await.map_err(|error| error.to_string())? {
            if container.state.is_active() {
                used.extend(container.spec.publish.iter().map(|port| (port.host_ip, port.host)));
            }
        }
        self.resolve(exposed, used)
    }

    fn resolve(
        &self,
        exposed: &ExposedPorts,
        mut used: BTreeSet<(std::net::Ipv4Addr, u16)>,
    ) -> Result<(BTreeSet<hl_container::Port>, Vec<hl_container::Publication>), String> {
        let mut ports = BTreeSet::new();
        for (key, value) in &exposed.0 {
            if value.as_object().is_none_or(|value| !value.is_empty()) {
                return Err(format!("ExposedPorts[{key:?}] must be an empty object"));
            }
            ports.insert(Self::port(key)?);
        }
        let mut publish = Vec::new();
        for (key, bindings) in &self.0 {
            let port = Self::port(key)?;
            ports.insert(port);
            for binding in bindings.as_deref().unwrap_or_default() {
                let host_ip = if binding.host_ip.is_empty() {
                    std::net::Ipv4Addr::UNSPECIFIED
                } else {
                    binding
                        .host_ip
                        .parse::<std::net::Ipv4Addr>()
                        .map_err(|_| format!("HostIp {:?} is not an IPv4 address", binding.host_ip))?
                };
                let host = if binding.host_port.is_empty() || binding.host_port == "0" {
                    (49152..=65535)
                        .find(|candidate| !Self::conflicts(&used, host_ip, *candidate))
                        .ok_or_else(|| "no ephemeral host ports are available".to_owned())?
                } else {
                    binding
                        .host_port
                        .parse::<u16>()
                        .map_err(|_| format!("invalid HostPort {:?}", binding.host_port))?
                };
                if host == 0 {
                    return Err("HostPort must be nonzero or empty for automatic allocation".into());
                }
                if Self::conflicts(&used, host_ip, host) {
                    return Err(format!("host TCP address {host_ip}:{host} is already allocated"));
                }
                used.insert((host_ip, host));
                publish.push(
                    hl_container::Publication::tcp(host_ip, host, port.guest).map_err(|error| error.to_string())?,
                );
            }
        }
        Ok((ports, publish))
    }

    fn conflicts(used: &BTreeSet<(std::net::Ipv4Addr, u16)>, address: std::net::Ipv4Addr, port: u16) -> bool {
        used.iter().any(|(bound, candidate)| {
            *candidate == port && (bound.is_unspecified() || address.is_unspecified() || *bound == address)
        })
    }

    fn port(value: &str) -> Result<hl_container::Port, String> {
        let (port, protocol) = value
            .split_once('/')
            .ok_or_else(|| format!("port key {value:?} must use PORT/PROTOCOL"))?;
        if protocol != "tcp" {
            return Err(format!(
                "port protocol {protocol:?} is unsupported; only tcp is available"
            ));
        }
        let port = port
            .parse::<u16>()
            .map_err(|_| format!("invalid container port {port:?}"))?;
        hl_container::Port::tcp(port).map_err(|error| error.to_string())
    }
}

#[cfg(all(test, feature = "runtime"))]
mod tests {
    use super::*;

    fn bindings(values: &[(&str, &str, &str)]) -> PortBindings {
        PortBindings(
            values
                .iter()
                .map(|(guest, ip, host)| {
                    (
                        (*guest).into(),
                        Some(vec![PortBinding {
                            host_ip: (*ip).into(),
                            host_port: (*host).into(),
                        }]),
                    )
                })
                .collect(),
        )
    }

    #[test]
    fn ephemeral_allocation_skips_active_ports_and_is_deterministic() {
        let (_, ports) = bindings(&[("80/tcp", "", ""), ("443/tcp", "", "")])
            .resolve(
                &ExposedPorts::default(),
                [49152, 49154]
                    .into_iter()
                    .map(|port| (std::net::Ipv4Addr::UNSPECIFIED, port))
                    .collect(),
            )
            .unwrap();
        assert_eq!(
            ports.iter().map(|port| port.host).collect::<Vec<_>>(),
            vec![49153, 49155]
        );
    }

    #[test]
    fn collisions_and_protocols_fail_honestly() {
        assert!(
            bindings(&[("80/tcp", "", "5000")])
                .resolve(
                    &ExposedPorts::default(),
                    [(std::net::Ipv4Addr::UNSPECIFIED, 5000)].into_iter().collect(),
                )
                .unwrap_err()
                .contains("already allocated")
        );
        assert!(
            bindings(&[("53/udp", "", "5001")])
                .resolve(&ExposedPorts::default(), BTreeSet::new())
                .unwrap_err()
                .contains("only tcp")
        );
    }

    #[test]
    fn host_addresses_have_socket_compatible_collision_semantics() {
        let (_, loopback) = bindings(&[("80/tcp", "127.0.0.1", "5002")])
            .resolve(&ExposedPorts::default(), BTreeSet::new())
            .unwrap();
        assert_eq!(loopback[0].host_ip, std::net::Ipv4Addr::LOCALHOST);

        let other = "127.0.0.2".parse().unwrap();
        assert!(
            bindings(&[("80/tcp", "127.0.0.1", "5002")])
                .resolve(&ExposedPorts::default(), [(other, 5002)].into())
                .is_ok()
        );
        assert!(
            bindings(&[("80/tcp", "127.0.0.1", "5002")])
                .resolve(
                    &ExposedPorts::default(),
                    [(std::net::Ipv4Addr::UNSPECIFIED, 5002)].into(),
                )
                .unwrap_err()
                .contains("already allocated")
        );
        assert!(
            bindings(&[("80/tcp", "", "5002")])
                .resolve(&ExposedPorts::default(), [(other, 5002)].into())
                .unwrap_err()
                .contains("already allocated")
        );
    }

    #[test]
    fn docker_port_maps_preserve_keys_bindings_and_pascal_case_fields() {
        let ports = PortBindings(BTreeMap::from([
            (
                "80/tcp".into(),
                Some(vec![
                    PortBinding {
                        host_ip: "127.0.0.1".into(),
                        host_port: "8080".into(),
                    },
                    PortBinding {
                        host_ip: "127.0.0.2".into(),
                        host_port: "8081".into(),
                    },
                ]),
            ),
            ("443/tcp".into(), None),
        ]));
        assert_eq!(
            serde_json::to_value(ports).unwrap(),
            serde_json::json!({
                "80/tcp": [
                    {"HostIp": "127.0.0.1", "HostPort": "8080"},
                    {"HostIp": "127.0.0.2", "HostPort": "8081"}
                ],
                "443/tcp": null
            })
        );

        let summary = PortSummary {
            ip: Some("127.0.0.1".into()),
            private_port: 80,
            public_port: Some(8080),
            protocol: "tcp".into(),
        };
        assert_eq!(
            serde_json::to_value(summary).unwrap(),
            serde_json::json!({
                "IP": "127.0.0.1",
                "PrivatePort": 80,
                "PublicPort": 8080,
                "Type": "tcp"
            })
        );
    }
}
