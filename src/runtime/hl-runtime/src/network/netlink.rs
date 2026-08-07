use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use hl_descriptor::{ObjectError, ReadinessObserver, ReadinessRegistry, ReadinessSubscription};
use hl_network::NamespaceInterface;

struct NamespaceRoute {
    prefix: u8,
    scope: u8,
    destination: Option<[u8; 4]>,
    gateway: Option<[u8; 4]>,
    preferred_source: Option<[u8; 4]>,
    interface: u32,
}

pub(crate) struct RouteSocket {
    interfaces: Vec<NamespaceInterface>,
    port: u32,
    replies: Mutex<VecDeque<Vec<u8>>>,
    readiness: ReadinessRegistry,
}

impl RouteSocket {
    pub(crate) fn new(interfaces: Vec<NamespaceInterface>, port: u32) -> Arc<Self> {
        Arc::new(Self {
            interfaces,
            port,
            replies: Mutex::new(VecDeque::new()),
            readiness: ReadinessRegistry::new(),
        })
    }

    pub(crate) fn port(&self) -> u32 {
        self.port
    }

    pub(crate) fn ready(&self) -> bool {
        !self
            .replies
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    }

    pub(crate) fn observe(
        &self,
        observer: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn ReadinessSubscription>, ObjectError> {
        self.readiness.subscribe(observer)
    }

    pub(crate) fn send(&self, input: &[u8]) -> Result<usize, ObjectError> {
        let mut offset = 0;
        let mut generated = Vec::new();
        while offset + 16 <= input.len() {
            let length = u32::from_ne_bytes(input[offset..offset + 4].try_into().unwrap()) as usize;
            if length < 16 || offset.checked_add(length).is_none_or(|end| end > input.len()) {
                return Err(ObjectError::InvalidArgument);
            }
            let kind = u16::from_ne_bytes(input[offset + 4..offset + 6].try_into().unwrap());
            let flags = u16::from_ne_bytes(input[offset + 6..offset + 8].try_into().unwrap());
            let sequence = u32::from_ne_bytes(input[offset + 8..offset + 12].try_into().unwrap());
            generated.push(self.reply(&input[offset..offset + length], kind, flags, sequence));
            offset += (length + 3) & !3;
        }
        if offset != input.len() {
            return Err(ObjectError::InvalidArgument);
        }
        let mut replies = self.replies.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        replies.extend(generated);
        drop(replies);
        self.readiness.notify();
        Ok(input.len())
    }

    pub(crate) fn receive(&self, output: &mut [u8], peek: bool) -> Result<(usize, usize), ObjectError> {
        let mut replies = self.replies.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(reply) = replies.front() else {
            return Err(ObjectError::WouldBlock);
        };
        let full = reply.len();
        let count = full.min(output.len());
        output[..count].copy_from_slice(&reply[..count]);
        if !peek {
            replies.pop_front();
        }
        Ok((count, full))
    }

    fn reply(&self, request: &[u8], kind: u16, flags: u16, sequence: u32) -> Vec<u8> {
        if kind >= 16 && kind % 4 != 2 {
            return self.error(request, sequence, -1);
        }
        if kind == 18 && flags & 0x300 == 0 {
            return self.one_link(request, sequence);
        }
        let mut output = Vec::new();
        match kind {
            18 => {
                for interface in &self.interfaces {
                    self.link(&mut output, sequence, interface, true);
                }
            }
            22 => {
                if let Some(loopback) = self.interfaces.first() {
                    self.address(
                        &mut output,
                        sequence,
                        loopback,
                        2,
                        &loopback.ipv4,
                        254,
                        Some(&loopback.name),
                    );
                    let mut ipv6 = [0_u8; 16];
                    ipv6[15] = 1;
                    self.address(&mut output, sequence, loopback, 10, &ipv6, 254, None);
                }
                for interface in self.interfaces.iter().skip(1) {
                    self.address(
                        &mut output,
                        sequence,
                        interface,
                        2,
                        &interface.ipv4,
                        0,
                        Some(&interface.name),
                    );
                }
            }
            26 => {
                if let Some(interface) = self.interfaces.iter().find(|interface| interface.index != 1) {
                    self.route(
                        &mut output,
                        sequence,
                        NamespaceRoute {
                            prefix: 0,
                            scope: 0,
                            destination: None,
                            gateway: Some(Self::gateway(interface)),
                            preferred_source: None,
                            interface: interface.index,
                        },
                    );
                }
                for interface in self.interfaces.iter().filter(|interface| interface.index != 1) {
                    self.route(
                        &mut output,
                        sequence,
                        NamespaceRoute {
                            prefix: interface.prefix,
                            scope: 253,
                            destination: Some(Self::network(interface)),
                            gateway: None,
                            preferred_source: Some(interface.ipv4),
                            interface: interface.index,
                        },
                    );
                }
            }
            _ => {}
        }
        Self::header(&mut output, 3, 2, sequence, self.port, 16);
        output
    }

    fn one_link(&self, request: &[u8], sequence: u32) -> Vec<u8> {
        let index = request
            .get(20..24)
            .map_or(0, |bytes| i32::from_ne_bytes(bytes.try_into().unwrap()));
        let mut name = None;
        let mut offset = 32;
        while offset + 4 <= request.len() {
            let length = u16::from_ne_bytes(request[offset..offset + 2].try_into().unwrap()) as usize;
            let kind = u16::from_ne_bytes(request[offset + 2..offset + 4].try_into().unwrap());
            if length < 4 || offset + length > request.len() {
                break;
            }
            if kind == 3 {
                let value = &request[offset + 4..offset + length];
                name = Some(&value[..value.iter().position(|byte| *byte == 0).unwrap_or(value.len())]);
            }
            offset += (length + 3) & !3;
        }
        let Some(interface) = self.interfaces.iter().find(|interface| {
            (index != 0 && interface.index as i32 == index) || name.is_some_and(|name| interface.name == name)
        }) else {
            return self.error(request, sequence, -19);
        };
        let mut output = Vec::new();
        self.link(&mut output, sequence, interface, false);
        output
    }

    fn error(&self, request: &[u8], sequence: u32, error: i32) -> Vec<u8> {
        let mut output = Vec::new();
        Self::header(&mut output, 2, 0, sequence, self.port, 36);
        output.extend_from_slice(&error.to_ne_bytes());
        output.extend_from_slice(&request[..16.min(request.len())]);
        output.resize(36, 0);
        output
    }

    fn link(&self, output: &mut Vec<u8>, sequence: u32, interface: &NamespaceInterface, multipart: bool) {
        let start = output.len();
        output.resize(start + 32, 0);
        output[start + 4..start + 6].copy_from_slice(&16_u16.to_ne_bytes());
        output[start + 6..start + 8].copy_from_slice(&(if multipart { 2_u16 } else { 0 }).to_ne_bytes());
        output[start + 8..start + 12].copy_from_slice(&sequence.to_ne_bytes());
        output[start + 12..start + 16].copy_from_slice(&self.port.to_ne_bytes());
        output[start + 18..start + 20].copy_from_slice(&interface.hardware_type.to_ne_bytes());
        output[start + 20..start + 24].copy_from_slice(&interface.index.to_ne_bytes());
        let flags = u32::from(interface.flags) | 0x1_0000;
        output[start + 24..start + 28].copy_from_slice(&flags.to_ne_bytes());
        output[start + 28..start + 32].copy_from_slice(&u32::MAX.to_ne_bytes());
        Self::attribute(output, 3, &[interface.name.as_slice(), &[0]].concat());
        Self::attribute(output, 1, &interface.mac);
        let broadcast = if interface.index == 1 { [0; 6] } else { [0xff; 6] };
        Self::attribute(output, 2, &broadcast);
        Self::attribute(output, 4, &interface.mtu.to_ne_bytes());
        let tx = if interface.index == 1 { 0_u32 } else { 1000 };
        Self::attribute(output, 13, &tx.to_ne_bytes());
        Self::attribute(output, 16, &[6]);
        Self::attribute(output, 17, &[0]);
        let length = (output.len() - start) as u32;
        output[start..start + 4].copy_from_slice(&length.to_ne_bytes());
    }

    fn address(
        &self,
        output: &mut Vec<u8>,
        sequence: u32,
        interface: &NamespaceInterface,
        family: u8,
        address: &[u8],
        scope: u8,
        label: Option<&[u8]>,
    ) {
        let start = output.len();
        output.resize(start + 24, 0);
        output[start + 4..start + 6].copy_from_slice(&20_u16.to_ne_bytes());
        output[start + 6..start + 8].copy_from_slice(&2_u16.to_ne_bytes());
        output[start + 8..start + 12].copy_from_slice(&sequence.to_ne_bytes());
        output[start + 12..start + 16].copy_from_slice(&self.port.to_ne_bytes());
        output[start + 16] = family;
        output[start + 17] = if family == 10 { 128 } else { interface.prefix };
        output[start + 19] = scope;
        output[start + 20..start + 24].copy_from_slice(&interface.index.to_ne_bytes());
        Self::attribute(output, 1, address);
        Self::attribute(output, 2, address);
        if let Some(label) = label {
            Self::attribute(output, 3, &[label, &[0]].concat());
        }
        let length = (output.len() - start) as u32;
        output[start..start + 4].copy_from_slice(&length.to_ne_bytes());
    }

    fn route(&self, output: &mut Vec<u8>, sequence: u32, route: NamespaceRoute) {
        let start = output.len();
        output.resize(start + 28, 0);
        output[start + 4..start + 6].copy_from_slice(&24_u16.to_ne_bytes());
        output[start + 6..start + 8].copy_from_slice(&2_u16.to_ne_bytes());
        output[start + 8..start + 12].copy_from_slice(&sequence.to_ne_bytes());
        output[start + 12..start + 16].copy_from_slice(&self.port.to_ne_bytes());
        output[start + 16] = 2;
        output[start + 17] = route.prefix;
        output[start + 20] = 254;
        output[start + 21] = 3;
        output[start + 22] = route.scope;
        output[start + 23] = 1;
        if let Some(destination) = route.destination {
            Self::attribute(output, 1, &destination);
        }
        Self::attribute(output, 4, &route.interface.to_ne_bytes());
        if let Some(gateway) = route.gateway {
            Self::attribute(output, 5, &gateway);
        }
        if let Some(preferred_source) = route.preferred_source {
            Self::attribute(output, 7, &preferred_source);
        }
        let length = (output.len() - start) as u32;
        output[start..start + 4].copy_from_slice(&length.to_ne_bytes());
    }

    fn network(interface: &NamespaceInterface) -> [u8; 4] {
        let address = u32::from_be_bytes(interface.ipv4);
        let mask = if interface.prefix == 0 {
            0
        } else {
            u32::MAX << (32 - interface.prefix)
        };
        (address & mask).to_be_bytes()
    }

    fn gateway(interface: &NamespaceInterface) -> [u8; 4] {
        u32::from_be_bytes(Self::network(interface))
            .saturating_add(1)
            .to_be_bytes()
    }

    fn header(output: &mut Vec<u8>, kind: u16, flags: u16, sequence: u32, port: u32, length: u32) {
        output.extend_from_slice(&length.to_ne_bytes());
        output.extend_from_slice(&kind.to_ne_bytes());
        output.extend_from_slice(&flags.to_ne_bytes());
        output.extend_from_slice(&sequence.to_ne_bytes());
        output.extend_from_slice(&port.to_ne_bytes());
    }

    fn attribute(output: &mut Vec<u8>, kind: u16, data: &[u8]) {
        let length = 4 + data.len();
        output.extend_from_slice(&(length as u16).to_ne_bytes());
        output.extend_from_slice(&kind.to_ne_bytes());
        output.extend_from_slice(data);
        output.resize((output.len() + 3) & !3, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::RouteSocket;

    fn socket() -> std::sync::Arc<RouteSocket> {
        let policy = hl_network::NetworkPolicy::from_launch(false, b"", b"", b"").unwrap();
        RouteSocket::new(policy.namespace_interfaces(), 41)
    }

    fn request(kind: u16, flags: u16, index: i32) -> Vec<u8> {
        let mut bytes = vec![0; 32];
        bytes[..4].copy_from_slice(&32_u32.to_ne_bytes());
        bytes[4..6].copy_from_slice(&kind.to_ne_bytes());
        bytes[6..8].copy_from_slice(&flags.to_ne_bytes());
        bytes[8..12].copy_from_slice(&7_u32.to_ne_bytes());
        bytes[20..24].copy_from_slice(&index.to_ne_bytes());
        bytes
    }

    #[test]
    fn link_dump_is_multipart_and_ordered() {
        let socket = socket();
        socket.send(&request(18, 0x300, 0)).unwrap();
        let mut bytes = vec![0; 4096];
        let (count, full) = socket.receive(&mut bytes, false).unwrap();
        assert_eq!(count, full);
        assert_eq!(u16::from_ne_bytes(bytes[4..6].try_into().unwrap()), 16);
        assert!(bytes[..count].windows(4).any(|name| name == b"eth0"));
        assert_eq!(u16::from_ne_bytes(bytes[count - 12..count - 10].try_into().unwrap()), 3);
    }

    #[test]
    fn absent_single_link_returns_enodev() {
        let socket = socket();
        socket.send(&request(18, 1, 250)).unwrap();
        let mut bytes = vec![0; 64];
        let (count, _) = socket.receive(&mut bytes, false).unwrap();
        assert_eq!(count, 36);
        assert_eq!(u16::from_ne_bytes(bytes[4..6].try_into().unwrap()), 2);
        assert_eq!(i32::from_ne_bytes(bytes[16..20].try_into().unwrap()), -19);
    }

    #[test]
    fn route_dump_preserves_interface_order_prefixes_and_default() {
        let policy =
            hl_network::NetworkPolicy::from_launch(false, b"", b"", b"blue=10.4.5.6/24\ngreen=192.0.2.9/28").unwrap();
        let socket = RouteSocket::new(policy.namespace_interfaces(), 41);
        socket.send(&request(26, 0x300, 0)).unwrap();
        let mut bytes = vec![0; 4096];
        let (count, full) = socket.receive(&mut bytes, false).unwrap();
        assert_eq!(count, full);

        let mut messages = Vec::new();
        let mut offset = 0;
        while offset < count {
            let length = u32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
            messages.push((
                offset,
                length,
                u16::from_ne_bytes(bytes[offset + 4..offset + 6].try_into().unwrap()),
            ));
            offset += (length + 3) & !3;
        }
        assert_eq!(
            messages.iter().map(|message| message.2).collect::<Vec<_>>(),
            [24, 24, 24, 3]
        );

        let attribute = |message: (usize, usize, u16), kind: u16| {
            let mut offset = message.0 + 28;
            while offset < message.0 + message.1 {
                let length = u16::from_ne_bytes(bytes[offset..offset + 2].try_into().unwrap()) as usize;
                let found = u16::from_ne_bytes(bytes[offset + 2..offset + 4].try_into().unwrap());
                if found == kind {
                    return Some(bytes[offset + 4..offset + length].to_vec());
                }
                offset += (length + 3) & !3;
            }
            None
        };
        assert_eq!(bytes[messages[0].0 + 17], 0);
        assert_eq!(attribute(messages[0], 4).unwrap(), 2_u32.to_ne_bytes());
        assert_eq!(attribute(messages[0], 5).unwrap(), [10, 4, 5, 1]);
        assert_eq!(bytes[messages[1].0 + 17], 24);
        assert_eq!(attribute(messages[1], 1).unwrap(), [10, 4, 5, 0]);
        assert_eq!(attribute(messages[1], 7).unwrap(), [10, 4, 5, 6]);
        assert_eq!(attribute(messages[1], 4).unwrap(), 2_u32.to_ne_bytes());
        assert_eq!(bytes[messages[2].0 + 17], 28);
        assert_eq!(attribute(messages[2], 1).unwrap(), [192, 0, 2, 0]);
        assert_eq!(attribute(messages[2], 4).unwrap(), 3_u32.to_ne_bytes());
    }

    #[test]
    fn loopback_only_route_dump_is_empty_and_bounded() {
        let policy = hl_network::NetworkPolicy::from_launch(true, b"", b"", b"").unwrap();
        let socket = RouteSocket::new(policy.namespace_interfaces(), 41);
        socket.send(&request(26, 0x300, 0)).unwrap();
        let mut bytes = [0; 16];
        assert_eq!(socket.receive(&mut bytes, false).unwrap(), (16, 16));
        assert_eq!(u16::from_ne_bytes(bytes[4..6].try_into().unwrap()), 3);
    }
}
