//! Network-namespace projections and their procfs and sysfs renderings.

use std::fmt::Write as _;

/// Linux-visible values for one `AF_UNIX` row in a coherent network snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnixSocketView {
    pub identity: u64,
    pub reference_count: u32,
    pub protocol: u32,
    pub flags: u32,
    pub socket_type: u16,
    pub state: u8,
    pub inode: u64,
    pub path: Option<Vec<u8>>,
}

/// One generation-qualified view of a task's network namespace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkView {
    pub generation: u64,
    pub unix: Vec<UnixSocketView>,
    pub interfaces: Vec<NetworkInterfaceView>,
    pub internet: Vec<InternetSocketView>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InternetSocketView {
    pub ipv6: bool,
    pub udp: bool,
    pub local: [u8; 16],
    pub local_port: u16,
    pub remote: [u8; 16],
    pub remote_port: u16,
    pub state: u8,
    pub inode: u64,
}

/// One namespace-owned network interface and its Linux-visible accounting.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkInterfaceView {
    pub name: Vec<u8>,
    pub index: u32,
    pub loopback: bool,
    pub address: [u8; 6],
    pub ipv4: Option<[u8; 4]>,
    pub prefix: u8,
    pub receive: [u64; 8],
    pub transmit: [u64; 8],
}

impl NetworkView {
    pub(in crate::procfs) fn unix_bytes(&self) -> Vec<u8> {
        let mut bytes = b"Num       RefCount Protocol Flags    Type St Inode Path\n".to_vec();
        for socket in &self.unix {
            bytes.extend_from_slice(
                format!(
                    "{:016x}: {:08x} {:08x} {:08x} {:04x} {:02x} {:5} ",
                    socket.identity,
                    socket.reference_count,
                    socket.protocol,
                    socket.flags,
                    socket.socket_type,
                    socket.state,
                    socket.inode,
                )
                .as_bytes(),
            );
            if let Some(path) = &socket.path {
                bytes.extend_from_slice(path);
            }
            bytes.push(b'\n');
        }
        bytes
    }

    #[allow(clippy::unused_self)]
    pub(in crate::procfs) fn entries(&self) -> impl Iterator<Item = (&'static [u8], u8)> {
        const NAMES: &[&[u8]] = &[
            b"arp",
            b"dev",
            b"dev_mcast",
            b"if_inet6",
            b"igmp",
            b"ipv6_route",
            b"netstat",
            b"route",
            b"snmp",
            b"snmp6",
            b"sockstat",
            b"tcp",
            b"tcp6",
            b"udp",
            b"udp6",
            b"unix",
        ];
        NAMES.iter().map(|name| (*name, 8))
    }

    pub(in crate::procfs) fn bytes(&self, leaf: NetworkLeaf) -> Vec<u8> {
        match leaf {
            NetworkLeaf::Unix => self.unix_bytes(),
            NetworkLeaf::Dev => self.dev_bytes(),
            NetworkLeaf::Route => self.route_bytes(),
            NetworkLeaf::IfInet6 => b"00000000000000000000000000000001 01 80 10 80        lo\n".to_vec(),
            NetworkLeaf::Tcp => self.socket_bytes(false, false),
            NetworkLeaf::Tcp6 => self.socket_bytes(true, false),
            NetworkLeaf::Udp => self.socket_bytes(false, true),
            NetworkLeaf::Udp6 => self.socket_bytes(true, true),
            NetworkLeaf::Arp => b"IP address       HW type     Flags       HW address            Mask     Device\n".to_vec(),
            NetworkLeaf::Igmp => self.igmp_bytes(),
            NetworkLeaf::DevMcast => Vec::new(),
            NetworkLeaf::Ipv6Route => b"00000000000000000000000000000001 80 00000000000000000000000000000000 00 00000000000000000000000000000000 00000000 00000002 00000000 80200001       lo\n".to_vec(),
            NetworkLeaf::Snmp => SNMP.as_bytes().to_vec(),
            NetworkLeaf::Netstat => NETSTAT.as_bytes().to_vec(),
            NetworkLeaf::Snmp6 => SNMP6.as_bytes().to_vec(),
            NetworkLeaf::Sockstat => format!(
                "sockets: used {}\nTCP: inuse 0 orphan 0 tw 0 alloc 0 mem 0\nUDP: inuse 0 mem 0\n",
                self.unix.len()
            )
            .into_bytes(),
        }
    }

    fn dev_bytes(&self) -> Vec<u8> {
        let mut output = String::from(
            "Inter-|   Receive                                                |  Transmit\n face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n",
        );
        for interface in &self.interfaces {
            let name = String::from_utf8_lossy(&interface.name);
            let receive = interface
                .receive
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(" ");
            let transmit = interface
                .transmit
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(" ");
            let _ = writeln!(output, "{name:>6}: {receive} {transmit}");
        }
        output.into_bytes()
    }

    fn route_bytes(&self) -> Vec<u8> {
        let mut output =
            String::from("Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n");
        for interface in self.interfaces.iter().filter(|interface| !interface.loopback) {
            let Some(ip) = interface.ipv4 else { continue };
            let host = u32::from_le_bytes(ip);
            let mask = if interface.prefix == 32 {
                u32::MAX
            } else {
                (1_u32 << interface.prefix) - 1
            };
            let network = host & mask;
            let gateway = network | 0x0100_0000;
            let _ = write!(
                output,
                "{}\t00000000\t{gateway:08X}\t0003\t0\t0\t0\t00000000\t0\t0\t0\n{}\t{network:08X}\t00000000\t0001\t0\t0\t0\t{mask:08X}\t0\t0\t0\n",
                String::from_utf8_lossy(&interface.name),
                String::from_utf8_lossy(&interface.name)
            );
        }
        output.into_bytes()
    }

    fn igmp_bytes(&self) -> Vec<u8> {
        let mut output = String::from("Idx\tDevice    : Count Querier\tGroup    Users Timer\tReporter\n");
        for interface in &self.interfaces {
            let _ = write!(
                output,
                "{}\t{:<10}:     1      V3\n\t\t\t\t010000E0     1 0:00000000\t\t0\n",
                interface.index,
                String::from_utf8_lossy(&interface.name)
            );
        }
        output.into_bytes()
    }

    fn socket_header(ipv6: bool, udp: bool) -> Vec<u8> {
        if ipv6 {
            format!("  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode{}\n", if udp { " ref pointer drops" } else { "" }).into_bytes()
        } else {
            format!(
                "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode{}\n",
                if udp { " ref pointer drops" } else { "" }
            )
            .into_bytes()
        }
    }

    fn socket_bytes(&self, ipv6: bool, udp: bool) -> Vec<u8> {
        let mut output = Self::socket_header(ipv6, udp);
        for (slot, socket) in self
            .internet
            .iter()
            .filter(|socket| socket.ipv6 == ipv6 && socket.udp == udp)
            .enumerate()
        {
            // The map/collect reads as the encoding it is; folding a String obscures it.
            #[allow(clippy::format_collect)]
            let encode = |bytes: &[u8]| {
                bytes
                    .chunks_exact(4)
                    .map(|part| format!("{:08X}", u32::from_le_bytes(part.try_into().unwrap())))
                    .collect::<String>()
            };
            output.extend_from_slice(format!(" {:3}: {}:{:04X} {}:{:04X} {:02X} 00000000:00000000 00:00000000 00000000 0 0 {} 1 0000000000000000 100 0 0 10 0\n", slot, encode(if ipv6 { &socket.local } else { &socket.local[..4] }), socket.local_port, encode(if ipv6 { &socket.remote } else { &socket.remote[..4] }), socket.remote_port, socket.state, socket.inode).as_bytes());
        }
        output
    }

    pub(in crate::procfs) fn interface(&self, name: &[u8]) -> Option<&NetworkInterfaceView> {
        self.interfaces.iter().find(|interface| interface.name == name)
    }
}

impl NetworkInterfaceView {
    pub(in crate::procfs) fn attribute(&self, attribute: InterfaceAttribute) -> Vec<u8> {
        let value = match attribute {
            InterfaceAttribute::Address => format!(
                "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}\n",
                self.address[0], self.address[1], self.address[2], self.address[3], self.address[4], self.address[5]
            ),
            InterfaceAttribute::Mtu => format!("{}\n", if self.loopback { 65536 } else { 1500 }),
            InterfaceAttribute::IfIndex => format!("{}\n", self.index),
            InterfaceAttribute::Type => format!("{}\n", if self.loopback { 772 } else { 1 }),
            InterfaceAttribute::Flags => format!("{}\n", if self.loopback { "0x9" } else { "0x1003" }),
            InterfaceAttribute::Operstate => format!("{}\n", if self.loopback { "unknown" } else { "up" }),
            InterfaceAttribute::Statistic(_) => String::from("0\n"),
        };
        value.into_bytes()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::procfs) enum InterfaceAttribute {
    Address,
    IfIndex,
    Mtu,
    Operstate,
    Type,
    Flags,
    Statistic(Statistic),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::procfs) enum Statistic {
    RxBytes,
    TxPackets,
}

impl InterfaceAttribute {
    pub(in crate::procfs) const fn identity(self) -> u64 {
        match self {
            Self::Address => 1,
            Self::IfIndex => 2,
            Self::Mtu => 3,
            Self::Operstate => 4,
            Self::Type => 5,
            Self::Flags => 6,
            Self::Statistic(Statistic::RxBytes) => 7,
            Self::Statistic(Statistic::TxPackets) => 8,
        }
    }

    pub(super) fn parse(path: &[u8]) -> Option<Self> {
        Some(match path {
            b"address" => Self::Address,
            b"ifindex" => Self::IfIndex,
            b"mtu" => Self::Mtu,
            b"operstate" => Self::Operstate,
            b"type" => Self::Type,
            b"flags" => Self::Flags,
            b"statistics/rx_bytes" => Self::Statistic(Statistic::RxBytes),
            b"statistics/tx_packets" => Self::Statistic(Statistic::TxPackets),
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::procfs) enum NetworkLeaf {
    Arp,
    Dev,
    DevMcast,
    IfInet6,
    Igmp,
    Ipv6Route,
    Netstat,
    Route,
    Snmp,
    Snmp6,
    Sockstat,
    Tcp,
    Tcp6,
    Udp,
    Udp6,
    Unix,
}

impl NetworkLeaf {
    pub(super) fn parse(name: &[u8]) -> Option<Self> {
        Some(match name {
            b"arp" => Self::Arp,
            b"dev" => Self::Dev,
            b"dev_mcast" => Self::DevMcast,
            b"if_inet6" => Self::IfInet6,
            b"igmp" => Self::Igmp,
            b"ipv6_route" => Self::Ipv6Route,
            b"netstat" => Self::Netstat,
            b"route" => Self::Route,
            b"snmp" => Self::Snmp,
            b"snmp6" => Self::Snmp6,
            b"sockstat" => Self::Sockstat,
            b"tcp" => Self::Tcp,
            b"tcp6" => Self::Tcp6,
            b"udp" => Self::Udp,
            b"udp6" => Self::Udp6,
            b"unix" => Self::Unix,
            _ => return None,
        })
    }
}

const SNMP: &str = "Ip: Forwarding DefaultTTL InReceives\nIp: 2 64 0\nIcmp: InMsgs InErrors\nIcmp: 0 0\nTcp: RtoAlgorithm RtoMin RtoMax MaxConn\nTcp: 1 200 120000 -1\nUdp: InDatagrams OutDatagrams\nUdp: 0 0\n";
const NETSTAT: &str = "TcpExt: TCPFastRetrans\nTcpExt: 0\nIpExt: InOctets\nIpExt: 0\n";
const SNMP6: &str = "Ip6InReceives                   \t0\nUdp6InDatagrams                 \t0\n";
