const INTERFACE_MAXIMUM: usize = 8;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterfaceConfiguration {
    pub bridge: Vec<u8>,
    pub name: Vec<u8>,
    pub index: u32,
    pub ipv4: [u8; 4],
    pub prefix: u8,
    pub mac: [u8; 6],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamespaceInterface {
    pub name: Vec<u8>,
    pub index: u32,
    pub ipv4: [u8; 4],
    pub prefix: u8,
    pub mac: [u8; 6],
    pub flags: u16,
    pub hardware_type: u16,
    pub mtu: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkPolicy {
    pub isolated: bool,
    pub interfaces: Vec<InterfaceConfiguration>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteDisposition {
    Host,
    NetworkUnreachable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkPolicyError {
    InvalidInterface,
}

impl NetworkPolicy {
    #[must_use]
    pub fn namespace_interfaces(&self) -> Vec<NamespaceInterface> {
        let mut interfaces = vec![NamespaceInterface {
            name: b"lo".to_vec(),
            index: 1,
            ipv4: [127, 0, 0, 1],
            prefix: 8,
            mac: [0; 6],
            flags: 0x49,
            hardware_type: 772,
            mtu: 65_536,
        }];
        interfaces.extend(self.interfaces.iter().map(|interface| NamespaceInterface {
            name: interface.name.clone(),
            index: interface.index,
            ipv4: interface.ipv4,
            prefix: interface.prefix,
            mac: interface.mac,
            flags: 0x1043,
            hardware_type: 1,
            mtu: 1500,
        }));
        interfaces
    }
    pub fn from_launch(isolated: bool, bridge: &[u8], ipv4: &[u8], records: &[u8]) -> Result<Self, NetworkPolicyError> {
        if isolated {
            return Ok(Self {
                isolated,
                interfaces: Vec::new(),
            });
        }
        let mut parsed = Vec::new();
        if !records.is_empty() {
            for record in records.split(|byte| *byte == b'\n').filter(|record| !record.is_empty()) {
                if parsed.len() == INTERFACE_MAXIMUM {
                    break;
                }
                let equal = record
                    .iter()
                    .position(|byte| *byte == b'=')
                    .ok_or(NetworkPolicyError::InvalidInterface)?;
                let identity = &record[..equal];
                let address = &record[equal + 1..];
                if identity.is_empty() || identity.len() > 40 {
                    return Err(NetworkPolicyError::InvalidInterface);
                }
                let slash = address
                    .iter()
                    .position(|byte| *byte == b'/')
                    .ok_or(NetworkPolicyError::InvalidInterface)?;
                let ip = Self::ipv4(&address[..slash])?;
                let prefix = Self::decimal(&address[slash + 1..])?;
                if prefix > 32 {
                    return Err(NetworkPolicyError::InvalidInterface);
                }
                parsed.push(Self::interface(parsed.len(), identity, ip, prefix as u8));
            }
        } else if !bridge.is_empty() && !ipv4.is_empty() {
            parsed.push(Self::interface(
                0,
                &bridge[..bridge.len().min(40)],
                Self::ipv4(ipv4)?,
                16,
            ));
        } else {
            parsed.push(Self::interface(0, b"", [172, 17, 0, 2], 16));
        }
        Ok(Self {
            isolated,
            interfaces: parsed,
        })
    }

    /// Selects whether an internet destination is routable by this guest
    /// namespace. The current launch schema describes IPv4 interfaces only,
    /// so IPv6 retains its loopback/link-local routes but has no global route.
    #[must_use]
    pub fn route(&self, address: &crate::SocketAddress) -> RouteDisposition {
        let crate::SocketAddress::Inet6 { address, .. } = address else {
            // An isolated namespace keeps its loopback transport but owns no
            // interface, so only loopback and the unspecified address route.
            if let crate::SocketAddress::Inet4 { address, .. } = address
                && self.isolated
                && address[0] != 127
                && *address != [0; 4]
            {
                return RouteDisposition::NetworkUnreachable;
            }
            return RouteDisposition::Host;
        };
        if self.isolated {
            let unspecified = address.iter().all(|byte| *byte == 0);
            let loopback = address[..15].iter().all(|byte| *byte == 0) && address[15] == 1;
            return if unspecified || loopback {
                RouteDisposition::Host
            } else {
                RouteDisposition::NetworkUnreachable
            };
        }
        let unspecified = address.iter().all(|byte| *byte == 0);
        let loopback = address[..15].iter().all(|byte| *byte == 0) && address[15] == 1;
        let link_local = address[0] == 0xfe && address[1] & 0xc0 == 0x80;
        if unspecified || loopback || link_local {
            RouteDisposition::Host
        } else {
            RouteDisposition::NetworkUnreachable
        }
    }

    /// Selects the virtual interface that owns an IPv4 connection destination.
    ///
    /// Loopback is handled by the namespace-local loopback transport. For
    /// overlapping subnets, launch order is authoritative, matching the retained
    /// switch implementation.
    #[must_use]
    pub fn connect_interface(&self, address: [u8; 4]) -> Option<&InterfaceConfiguration> {
        if address[0] == 127 {
            return None;
        }
        self.switch_interfaces()
            .find(|interface| Self::subnet_contains(interface, address))
    }

    /// Selects the virtual interface that owns an IPv4 bind address.
    ///
    /// An unspecified address binds the first configured switch interface.
    /// Otherwise an interface owns its configured address and every address in
    /// its subnet. Loopback remains namespace-local rather than switch-backed.
    #[must_use]
    pub fn bind_interface(&self, address: [u8; 4]) -> Option<&InterfaceConfiguration> {
        if address[0] == 127 {
            return None;
        }
        if address == [0; 4] {
            return self.switch_interfaces().next();
        }
        self.switch_interfaces()
            .find(|interface| interface.ipv4 == address)
            .or_else(|| {
                self.switch_interfaces()
                    .find(|interface| Self::subnet_contains(interface, address))
            })
    }

    fn switch_interfaces(&self) -> impl Iterator<Item = &InterfaceConfiguration> {
        self.interfaces.iter().filter(|interface| !interface.bridge.is_empty())
    }

    fn subnet_contains(interface: &InterfaceConfiguration, address: [u8; 4]) -> bool {
        let mask = if interface.prefix == 0 {
            0
        } else {
            u32::MAX << (32 - interface.prefix)
        };
        u32::from_be_bytes(address) & mask == u32::from_be_bytes(interface.ipv4) & mask
    }

    fn interface(slot: usize, bridge: &[u8], ipv4: [u8; 4], prefix: u8) -> InterfaceConfiguration {
        let mut name = b"eth".to_vec();
        name.extend_from_slice(slot.to_string().as_bytes());
        InterfaceConfiguration {
            bridge: bridge.to_vec(),
            name,
            index: u32::try_from(slot + 2).expect("bounded interface index"),
            ipv4,
            prefix,
            mac: [2, 0x42, ipv4[0], ipv4[1], ipv4[2], ipv4[3]],
        }
    }

    fn decimal(bytes: &[u8]) -> Result<u32, NetworkPolicyError> {
        if bytes.is_empty() || bytes.iter().any(|byte| !byte.is_ascii_digit()) {
            return Err(NetworkPolicyError::InvalidInterface);
        }
        bytes.iter().try_fold(0_u32, |value, byte| {
            value
                .checked_mul(10)
                .and_then(|value| value.checked_add(u32::from(byte - b'0')))
                .ok_or(NetworkPolicyError::InvalidInterface)
        })
    }

    fn ipv4(bytes: &[u8]) -> Result<[u8; 4], NetworkPolicyError> {
        let mut address = [0_u8; 4];
        let mut parts = bytes.split(|byte| *byte == b'.');
        for octet in &mut address {
            let value = Self::decimal(parts.next().ok_or(NetworkPolicyError::InvalidInterface)?)?;
            *octet = u8::try_from(value).map_err(|_| NetworkPolicyError::InvalidInterface)?;
        }
        if parts.next().is_some() || address == [0; 4] {
            return Err(NetworkPolicyError::InvalidInterface);
        }
        Ok(address)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_records() {
        let policy =
            NetworkPolicy::from_launch(false, b"ignored", b"192.0.2.9", b"blue=10.4.5.6/24\ngreen=10.8.0.9/12")
                .unwrap();
        assert_eq!(policy.interfaces.len(), 2);
        assert_eq!(policy.interfaces[0].name, b"eth0");
        assert_eq!(policy.interfaces[0].ipv4, [10, 4, 5, 6]);
        assert_eq!(policy.interfaces[0].prefix, 24);
        assert_eq!(policy.interfaces[0].mac, [2, 0x42, 10, 4, 5, 6]);
        assert_eq!(policy.interfaces[1].bridge, b"green");
    }

    #[test]
    fn isolation() {
        let policy = NetworkPolicy::from_launch(true, b"bridge", b"10.0.0.2", b"blue=10.1.0.2/16").unwrap();
        assert!(policy.interfaces.is_empty());
    }

    #[test]
    fn legacy_fallback() {
        let bridge = NetworkPolicy::from_launch(false, b"default", b"192.0.2.7", b"").unwrap();
        assert_eq!(
            (bridge.interfaces[0].ipv4, bridge.interfaces[0].prefix),
            ([192, 0, 2, 7], 16)
        );
        let fallback = NetworkPolicy::from_launch(false, b"", b"", b"").unwrap();
        assert_eq!(fallback.interfaces[0].ipv4, [172, 17, 0, 2]);
    }

    #[test]
    fn malformed_records() {
        assert_eq!(
            NetworkPolicy::from_launch(false, b"", b"", b"bridge=10.0.0.2/33"),
            Err(NetworkPolicyError::InvalidInterface)
        );
    }

    #[test]
    fn ipv6_routes() {
        let policy = NetworkPolicy::from_launch(false, b"", b"", b"").unwrap();
        let address = |address| crate::SocketAddress::Inet6 {
            address,
            port: 443,
            scope: 0,
        };
        let mut global = [0_u8; 16];
        global[..4].copy_from_slice(&[0x20, 0x01, 0x48, 0x60]);
        assert_eq!(policy.route(&address(global)), RouteDisposition::NetworkUnreachable);
        let mut loopback = [0_u8; 16];
        loopback[15] = 1;
        assert_eq!(policy.route(&address(loopback)), RouteDisposition::Host);
        let mut link_local = [0_u8; 16];
        link_local[..2].copy_from_slice(&[0xfe, 0x80]);
        assert_eq!(policy.route(&address(link_local)), RouteDisposition::Host);
        assert_eq!(policy.route(&address([0; 16])), RouteDisposition::Host);
    }

    #[test]
    fn isolated_namespace_keeps_loopback_and_drops_external_routes() {
        let policy = NetworkPolicy::from_launch(true, b"", b"", b"").unwrap();
        let v4 = |address| crate::SocketAddress::Inet4 { address, port: 443 };
        assert_eq!(policy.route(&v4([127, 0, 0, 1])), RouteDisposition::Host);
        assert_eq!(policy.route(&v4([0; 4])), RouteDisposition::Host);
        assert_eq!(policy.route(&v4([8, 8, 8, 8])), RouteDisposition::NetworkUnreachable);
        assert_eq!(
            policy.route(&v4([172, 17, 0, 2])),
            RouteDisposition::NetworkUnreachable
        );

        let v6 = |address| crate::SocketAddress::Inet6 {
            address,
            port: 443,
            scope: 0,
        };
        let mut loopback = [0_u8; 16];
        loopback[15] = 1;
        assert_eq!(policy.route(&v6(loopback)), RouteDisposition::Host);
        let mut link_local = [0_u8; 16];
        link_local[..2].copy_from_slice(&[0xfe, 0x80]);
        assert_eq!(policy.route(&v6(link_local)), RouteDisposition::NetworkUnreachable);
    }

    #[test]
    fn connect_priority() {
        let policy = NetworkPolicy::from_launch(
            false,
            b"",
            b"",
            b"wide=10.0.0.2/8\nnarrow=10.4.0.2/16\npoint=192.0.2.9/32",
        )
        .unwrap();

        assert_eq!(policy.connect_interface([10, 4, 9, 8]).unwrap().bridge, b"wide");
        assert_eq!(policy.connect_interface([192, 0, 2, 9]).unwrap().bridge, b"point");
        assert!(policy.connect_interface([192, 0, 2, 8]).is_none());
        assert!(policy.connect_interface([127, 0, 0, 1]).is_none());
    }

    #[test]
    fn wildcard_bind() {
        let policy = NetworkPolicy::from_launch(false, b"", b"", b"first=172.20.0.2/24\nsecond=172.21.0.2/24").unwrap();

        assert_eq!(policy.bind_interface([0, 0, 0, 0]).unwrap().bridge, b"first");
        assert_eq!(policy.bind_interface([172, 21, 0, 2]).unwrap().bridge, b"second");
        assert_eq!(policy.bind_interface([172, 20, 0, 255]).unwrap().bridge, b"first");
        assert!(policy.bind_interface([172, 22, 0, 2]).is_none());
        assert!(policy.bind_interface([127, 0, 0, 1]).is_none());
    }

    #[test]
    fn exact_bind() {
        let policy = NetworkPolicy::from_launch(false, b"", b"", b"wide=10.0.0.2/8\nnarrow=10.4.0.2/16").unwrap();

        assert_eq!(policy.bind_interface([10, 4, 0, 2]).unwrap().bridge, b"narrow");
        assert_eq!(policy.bind_interface([10, 4, 9, 8]).unwrap().bridge, b"wide");
    }

    #[test]
    fn prefix_edges() {
        let all = NetworkPolicy::from_launch(false, b"", b"", b"all=10.0.0.2/0").unwrap();
        assert_eq!(all.connect_interface([203, 0, 113, 7]).unwrap().bridge, b"all");

        let synthetic = NetworkPolicy::from_launch(false, b"", b"", b"").unwrap();
        assert!(synthetic.connect_interface([172, 17, 0, 3]).is_none());
        assert!(synthetic.bind_interface([0, 0, 0, 0]).is_none());
    }
}
