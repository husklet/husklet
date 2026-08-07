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
            SocketAddress::Inet6 { address, .. } if *address == [0; 16] => self
                .interfaces
                .iter()
                .find(|candidate| !candidate.bridge.is_empty())
                .map(EgressInterface::from),
            SocketAddress::Inet6 { .. } | SocketAddress::Unix(_) => None,
        };
        let wildcard = matches!(&address, SocketAddress::Inet4 { address, .. } if *address == [0; 4])
            || matches!(&address, SocketAddress::Inet6 { address, .. } if *address == [0; 16]);
        let aliases = if wildcard {
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
            // A wildcard bind lands on the bridge rendezvous, so a loopback connect carries that
            // bridge as the fallback route reaching the same listener Linux would reach.
            SocketAddress::Inet4 { address, port } if address[0] == 127 => (*port != 0)
                .then(|| self.loopback_interface())
                .flatten()
                .map(EgressInterface::from),
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

        let ipv6 = policy.bind_route(SocketAddress::Inet6 {
            address: [0; 16],
            port: 8080,
            scope: 0,
        });
        assert_eq!(ipv6.interface.as_ref().unwrap().bridge, b"first");
        assert_eq!(ipv6.aliases, route.aliases);
    }

    /// A loopback connect carries the bridge a wildcard bind published on, so the switch can
    /// reach the listener Linux would reach through one wildcard socket.
    #[test]
    fn loopback_connect_carries_the_wildcard_bind_bridge() {
        let policy = NetworkPolicy::from_launch(false, b"", b"", b"first=172.28.0.5/16\nsecond=10.0.0.2/8").unwrap();
        let route = policy.connect_route(SocketAddress::Inet4 {
            address: [127, 0, 0, 1],
            port: 34_567,
        });
        let interface = route.interface.unwrap();
        assert_eq!(interface.bridge, b"first");
        assert_eq!(interface.ipv4, [172, 28, 0, 5]);
        assert_eq!(
            policy
                .connect_route(SocketAddress::Inet4 {
                    address: [127, 0, 0, 1],
                    port: 0,
                })
                .interface,
            None
        );

        let unbridged = NetworkPolicy::from_launch(false, b"", b"", b"").unwrap();
        assert_eq!(
            unbridged
                .connect_route(SocketAddress::Inet4 {
                    address: [127, 0, 0, 1],
                    port: 34_567,
                })
                .interface,
            None
        );
    }

    #[test]
    fn specific_ipv6_bind_stays_on_the_native_ipv6_stack() {
        let policy = NetworkPolicy::from_launch(false, b"", b"", b"first=10.0.0.2/8").unwrap();
        let mut address = [0; 16];
        address[15] = 1;
        let route = policy.bind_route(SocketAddress::Inet6 {
            address,
            port: 8080,
            scope: 0,
        });
        assert!(route.interface.is_none());
        assert!(route.aliases.is_empty());
    }
}
