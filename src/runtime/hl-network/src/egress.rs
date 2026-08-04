use crate::{InterfaceConfiguration, NetworkPolicy, SocketAddress};

pub const BIND_ROUTE_ALIAS_MAXIMUM: usize = 7;

/// Stable virtual-interface identity carried across the runtime/host boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EgressInterface {
    pub bridge: Vec<u8>,
    pub index: u32,
    pub ipv4: [u8; 4],
}

impl From<&InterfaceConfiguration> for EgressInterface {
    fn from(value: &InterfaceConfiguration) -> Self {
        Self {
            bridge: value.bridge.clone(),
            index: value.index,
            ipv4: value.ipv4,
        }
    }
}

/// A socket address paired with the virtual interface selected for it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EgressRoute {
    pub address: SocketAddress,
    pub interface: Option<EgressInterface>,
}

/// A bind address, its selected interface, and the bounded interfaces that a
/// wildcard listener must also expose.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BindRoute {
    pub address: SocketAddress,
    pub interface: Option<EgressInterface>,
    pub aliases: Vec<EgressInterface>,
}

impl NetworkPolicy {
    #[must_use]
    pub fn bind_route(&self, address: SocketAddress) -> BindRoute {
        let interface = match &address {
            SocketAddress::Inet4 { address, .. } => self.bind_interface(*address).map(EgressInterface::from),
            SocketAddress::Inet6 { .. } | SocketAddress::Unix(_) => None,
        };
        let aliases = if matches!(&address, SocketAddress::Inet4 { address, .. } if *address == [0; 4]) {
            self.interfaces
                .iter()
                .filter(|candidate| !candidate.bridge.is_empty())
                .filter(|candidate| {
                    interface
                        .as_ref()
                        .is_none_or(|primary| candidate.index != primary.index)
                })
                .map(EgressInterface::from)
                .collect()
        } else {
            Vec::new()
        };
        BindRoute {
            address,
            interface,
            aliases,
        }
    }

    #[must_use]
    pub fn connect_route(&self, address: SocketAddress) -> EgressRoute {
        let interface = match &address {
            SocketAddress::Inet4 { address, .. } => self.connect_interface(*address).map(EgressInterface::from),
            SocketAddress::Inet6 { .. } | SocketAddress::Unix(_) => None,
        };
        EgressRoute { address, interface }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_identity() {
        let policy = NetworkPolicy::from_launch(false, b"", b"", b"wide=10.0.0.2/8\nnarrow=10.4.0.2/16").unwrap();
        let address = |address| SocketAddress::Inet4 { address, port: 8080 };

        let connected = policy.connect_route(address([10, 4, 0, 2]));
        assert_eq!(connected.interface.unwrap().bridge, b"wide");
        let bound = policy.bind_route(address([10, 4, 0, 2]));
        let interface = bound.interface.unwrap();
        assert_eq!(interface.bridge, b"narrow");
        assert_eq!(interface.index, 3);
        assert_eq!(interface.ipv4, [10, 4, 0, 2]);
    }

    #[test]
    fn wildcard_bind_carries_every_other_configured_switch_interface() {
        let policy = NetworkPolicy::from_launch(false, b"", b"", b"first=10.0.0.2/8\nsecond=192.0.2.2/24").unwrap();
        let route = policy.bind_route(SocketAddress::Inet4 {
            address: [0; 4],
            port: 8080,
        });
        assert_eq!(route.interface.as_ref().unwrap().bridge, b"first");
        assert_eq!(route.aliases.len(), 1);
        assert_eq!(route.aliases[0].bridge, b"second");
        assert_eq!(route.aliases[0].ipv4, [192, 0, 2, 2]);
    }
}
