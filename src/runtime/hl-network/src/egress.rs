use crate::{InterfaceConfiguration, NetworkPolicy, SocketAddress};

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

impl NetworkPolicy {
    #[must_use]
    pub fn bind_route(&self, address: SocketAddress) -> EgressRoute {
        let interface = match &address {
            SocketAddress::Inet4 { address, .. } => self.bind_interface(*address).map(EgressInterface::from),
            SocketAddress::Inet6 { .. } | SocketAddress::Unix(_) => None,
        };
        EgressRoute { address, interface }
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
}
