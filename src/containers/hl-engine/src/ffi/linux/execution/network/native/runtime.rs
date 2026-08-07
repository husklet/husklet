use std::mem::{size_of, zeroed};
use std::os::fd::OwnedFd;
use std::sync::Arc;

use hl_network::{
    AddressFamily, BindRoute, EgressRoute, NetworkResourceKey, SocketAddress, SocketHostIo, SocketProtocol, SocketType,
};
use hl_runtime::{
    AcceptedSocket, CreatedSocket, HostReceive, HostSend, HostSendResult, ReceivedDatagram, RuntimeNetworkError,
    RuntimeNetworkHost,
};

use super::{Native, SwitchPath};

impl RuntimeNetworkHost for Native {
    type Attachment = OwnedFd;

    fn input_queue(&self, token: u64) -> Result<u64, RuntimeNetworkError> {
        let descriptor = self.descriptor(token)?;
        let mut pending = 0_i32;
        // SAFETY: pending is writable for one integer and the table owns the live descriptor.
        if unsafe { libc::ioctl(descriptor, libc::FIONREAD, std::ptr::from_mut(&mut pending)) } != 0 {
            return Err(Self::runtime_error());
        }
        u64::try_from(pending).map_err(|_| RuntimeNetworkError::Failed)
    }

    fn output_queue(&self, token: u64) -> Result<u64, RuntimeNetworkError> {
        let descriptor = self.descriptor(token)?;
        let mut pending = 0_i32;
        // SAFETY: pending is writable for one integer and the table owns the live descriptor.
        if unsafe { libc::ioctl(descriptor, libc::TIOCOUTQ, std::ptr::from_mut(&mut pending)) } != 0 {
            return Err(Self::runtime_error());
        }
        u64::try_from(pending).map_err(|_| RuntimeNetworkError::Failed)
    }

    fn create(
        &self,
        family: AddressFamily,
        socket_type: SocketType,
        protocol: SocketProtocol,
    ) -> Result<CreatedSocket<u64>, RuntimeNetworkError> {
        let family = match family {
            AddressFamily::Inet4 => libc::AF_INET,
            AddressFamily::Inet6 => libc::AF_INET6,
            AddressFamily::Unix => return Err(RuntimeNetworkError::Unsupported),
        };
        let kind = match socket_type {
            SocketType::Stream => libc::SOCK_STREAM,
            SocketType::Datagram => libc::SOCK_DGRAM,
            _ => return Err(RuntimeNetworkError::Unsupported),
        };
        let protocol = match protocol {
            SocketProtocol::Default => 0,
            SocketProtocol::Tcp => libc::IPPROTO_TCP,
            SocketProtocol::Udp => libc::IPPROTO_UDP,
            SocketProtocol::Icmp => libc::IPPROTO_ICMP,
        };
        // SAFETY: socket arguments are validated constants and return a newly owned descriptor.
        let descriptor = unsafe { libc::socket(family, kind, protocol) };
        if descriptor < 0 {
            return Err(Self::runtime_error());
        }
        // SAFETY: F_SETFL and F_SETFD mutate only the newly owned descriptor.
        let configured = unsafe {
            libc::fcntl(descriptor, libc::F_SETFL, libc::O_NONBLOCK) == 0
                && libc::fcntl(descriptor, libc::F_SETFD, libc::FD_CLOEXEC) == 0
        };
        if !configured {
            // SAFETY: descriptor is still solely owned here.
            unsafe {
                libc::close(descriptor);
            }
            return Err(Self::runtime_error());
        }
        let token = self.insert(descriptor)?;
        if let Some(entry) = self
            .shared
            .sockets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(&token)
        {
            entry.original_family = Some(family);
            entry.original_protocol = Some(protocol);
        }
        if protocol == libc::IPPROTO_ICMP
            && let Some(entry) = self
                .shared
                .sockets
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get_mut(&token)
        {
            entry.icmp = true;
        }
        Ok(CreatedSocket {
            token,
            resource: NetworkResourceKey::new(token).ok_or(RuntimeNetworkError::NoMemory)?,
            binding: std::sync::Arc::new(()),
        })
    }

    fn bind(&self, token: u64, address: SocketAddress) -> Result<SocketAddress, RuntimeNetworkError> {
        let descriptor = self.descriptor(token)?;
        let projected = match &address {
            SocketAddress::Inet4 { address: host, port } if *host == [0; 4] || *host == [127, 0, 0, 1] => {
                let bindings = self
                    .shared
                    .bindings
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if *port != 0 && bindings.iter().any(|(guest, _)| guest == &address) {
                    return Err(RuntimeNetworkError::AddressInUse);
                }
                SocketAddress::Inet4 {
                    address: [127, 0, 0, 1],
                    port: 0,
                }
            }
            _ => address.clone(),
        };
        let (storage, length) = Self::socket_address(&projected)?;
        // SAFETY: storage contains a sockaddr matching length and descriptor remains table-owned.
        if unsafe { libc::bind(descriptor, (&raw const storage).cast(), length) } != 0 {
            return Err(Self::runtime_error());
        }
        let actual = self.address_of(token, false)?;
        if projected != address {
            let guest = match (&address, &actual) {
                (
                    SocketAddress::Inet4 {
                        address,
                        port: requested,
                    },
                    SocketAddress::Inet4 { port: actual, .. },
                ) => SocketAddress::Inet4 {
                    address: *address,
                    port: if *requested == 0 { *actual } else { *requested },
                },
                _ => return Err(RuntimeNetworkError::Failed),
            };
            if matches!(address, SocketAddress::Inet4 { port, .. } if port != 0) {
                self.shared
                    .bindings
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push((address.clone(), actual));
            }
            if let Some(entry) = self
                .shared
                .sockets
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get_mut(&token)
            {
                entry.guest_local = Some(guest.clone());
            }
            return Ok(guest);
        }
        Ok(actual)
    }

    fn bind_route(&self, token: u64, route: BindRoute) -> Result<SocketAddress, RuntimeNetworkError> {
        if route.aliases.len() > hl_network::BIND_ROUTE_ALIAS_MAXIMUM {
            return Err(RuntimeNetworkError::Invalid);
        }
        let Some(interface) = route.interface else {
            return self.bind(token, route.address);
        };
        let (address, port, ipv6_wildcard) = match route.address {
            SocketAddress::Inet4 { address, port } => (address, port, false),
            SocketAddress::Inet6 { address, port, .. } if address == [0; 16] => ([0; 4], port, true),
            SocketAddress::Inet6 { .. } | SocketAddress::Unix(_) => return Err(RuntimeNetworkError::Invalid),
        };
        let kind = self.socket_type(token)?;
        if !matches!(kind, libc::SOCK_STREAM | libc::SOCK_DGRAM) {
            return Err(RuntimeNetworkError::OperationNotSupported);
        }
        let first = if port == 0 {
            20_000_u16.wrapping_add((token as u16) & 0x3fff)
        } else {
            port
        };
        let attempts = if port == 0 { 45_000 } else { 1 };
        let ipv6_only = if ipv6_wildcard {
            matches!(
                super::super::socket_option::get(self.descriptor(token)?, 41, 26),
                Ok(hl_linux::GuestSocketOption::Scalar(value)) if value != 0
            )
        } else {
            false
        };
        let mut descriptor = None;
        for offset in 0..attempts {
            let candidate = first.wrapping_add(offset as u16).max(1024);
            let bound = if let Some(bound) = descriptor {
                bound
            } else {
                let value = self.switch_socket(token, kind)?;
                descriptor = Some(value);
                value
            };
            let mut publication = hl_fs::Publication::default();
            let staged = (|| {
                let (anchor, path) = Self::switch_reservation(&interface, candidate, ipv6_only)?;
                let (storage, length) = Self::socket_address(&SocketAddress::Unix(path.clone()))?;
                // SAFETY: storage contains a bounded sockaddr_un and descriptor remains table-owned.
                if unsafe { libc::bind(bound, (&raw const storage).cast(), length) } != 0 {
                    return Err(Self::runtime_error());
                }
                // The peer protocol reads this name back with getsockname, so the bind must address it
                // by pathname; adoption confirms the entry it created is the one the anchor holds.
                publication
                    .adopt(&anchor, Self::switch_name(&path)?, path.clone())
                    .map_err(Self::publication_error)?;
                if address == [0; 4] && kind == libc::SOCK_STREAM {
                    for alias in &route.aliases {
                        let (alias_anchor, alias_path) = Self::switch_reservation(alias, candidate, ipv6_only)?;
                        let alias_name = Self::switch_name(&alias_path)?.to_vec();
                        publication
                            .reserve_link(&alias_anchor, &alias_name, alias_path, &path)
                            .map_err(Self::publication_error)?;
                    }
                    // Linux serves one wildcard listener at every local address, so the same
                    // publication carries the namespace-private loopback name.
                    if !ipv6_only {
                        let loopback = Self::switch_loopback_path(&interface, candidate)?;
                        let loopback_name = Self::switch_name(&loopback)?.to_vec();
                        publication
                            .reserve_link(&anchor, &loopback_name, loopback, &path)
                            .map_err(Self::publication_error)?;
                    }
                }
                publication.commit().map_err(Self::publication_error)
            })();
            if let Err(error) = staged {
                drop(publication);
                if port != 0 || error != RuntimeNetworkError::AddressInUse {
                    self.restore_inet_socket(token, kind)?;
                    return Err(error);
                }
                continue;
            }
            let local = SocketAddress::Inet4 {
                address: interface.ipv4,
                port: candidate,
            };
            let mut sockets = self
                .shared
                .sockets
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let entry = sockets.get_mut(&token).ok_or(RuntimeNetworkError::Invalid)?;
            entry.guest_local = Some(local.clone());
            entry.switch_interface = Some(interface.clone());
            let ownership = Arc::new(SwitchPath::new(publication));
            let weak = Arc::downgrade(&ownership);
            let mut registry = self
                .shared
                .switch_paths
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for owned_path in ownership.names() {
                registry.insert(owned_path.clone(), weak.clone());
            }
            entry.switch_path = Some(ownership);
            return Ok(local);
        }
        if descriptor.is_some() {
            self.restore_inet_socket(token, kind)?;
        }
        Err(RuntimeNetworkError::AddressInUse)
    }

    fn prepare_connect(&self, token: u64, address: SocketAddress) -> Result<(), RuntimeNetworkError> {
        let address = self.binding(&address).unwrap_or(address);
        let mut sockets = self
            .shared
            .sockets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sockets.get_mut(&token).ok_or(RuntimeNetworkError::Invalid)?.pending = Some(address);
        Ok(())
    }

    fn prepare_connect_route(&self, token: u64, route: EgressRoute) -> Result<(), RuntimeNetworkError> {
        let Some(interface) = route.interface else {
            return self.prepare_connect(token, route.address);
        };
        if self.is_icmp(token) {
            return self.prepare_connect(token, route.address);
        }
        let SocketAddress::Inet4 { address, port } = route.address else {
            return Err(RuntimeNetworkError::Invalid);
        };
        if port == 0 {
            return Err(RuntimeNetworkError::Invalid);
        }
        if address[0] == 127 {
            return self.prepare_loopback_connect(token, &interface, address, port);
        }
        let kind = self.socket_type(token)?;
        let (directory, path) = Self::switch_path(&interface, address, port)?;
        Self::switch_anchor(&directory)?;
        let peer = SocketAddress::Inet4 { address, port };
        if kind == libc::SOCK_DGRAM {
            let needs_source = self
                .shared
                .sockets
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&token)
                .is_none_or(|entry| entry.guest_local.is_none());
            if needs_source {
                self.bind_route(
                    token,
                    BindRoute {
                        address: SocketAddress::Inet4 {
                            address: interface.ipv4,
                            port: 0,
                        },
                        interface: Some(interface.clone()),
                        aliases: Vec::new(),
                    },
                )?;
            }
            let mut sockets = self
                .shared
                .sockets
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let entry = sockets.get_mut(&token).ok_or(RuntimeNetworkError::Invalid)?;
            entry.pending = Some(peer.clone());
            entry.guest_peer = Some(peer);
            entry.datagram_peer = Some(path);
            return Ok(());
        }
        if kind != libc::SOCK_STREAM {
            return Err(RuntimeNetworkError::OperationNotSupported);
        }
        self.switch_socket(token, libc::SOCK_STREAM)?;
        let mut sockets = self
            .shared
            .sockets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = sockets.get_mut(&token).ok_or(RuntimeNetworkError::Invalid)?;
        entry.pending = Some(SocketAddress::Unix(path));
        entry.guest_peer = Some(peer);
        Ok(())
    }

    fn listen(&self, token: u64, backlog: u32) -> Result<(), RuntimeNetworkError> {
        let descriptor = self.descriptor(token)?;
        // SAFETY: listen mutates only the owned descriptor.
        if unsafe { libc::listen(descriptor, backlog.min(i32::MAX as u32) as i32) } == 0 {
            Ok(())
        } else {
            Err(Self::runtime_error())
        }
    }

    fn accept(&self, token: u64) -> Result<AcceptedSocket<u64>, RuntimeNetworkError> {
        let descriptor = self.descriptor(token)?;
        let projected = self
            .shared
            .sockets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&token)
            .filter(|entry| entry.switched)
            .and_then(|entry| entry.guest_local.clone());
        // SAFETY: zero is valid initialization for sockaddr storage.
        let mut peer = unsafe { zeroed::<libc::sockaddr_storage>() };
        let mut length = size_of::<libc::sockaddr_storage>() as libc::socklen_t;
        // SAFETY: peer storage is writable and returned descriptor is newly owned.
        let accepted = unsafe { libc::accept(descriptor, (&raw mut peer).cast(), &raw mut length) };
        self.arm_read(token);
        if accepted < 0 {
            return Err(Self::runtime_error());
        }
        // SAFETY: accepted is newly owned; flags enforce nonblocking scheduler safety and host CLOEXEC.
        let configured = unsafe {
            libc::fcntl(accepted, libc::F_SETFL, libc::O_NONBLOCK) == 0
                && libc::fcntl(accepted, libc::F_SETFD, libc::FD_CLOEXEC) == 0
        };
        if !configured {
            // SAFETY: accepted is still solely owned here after flag configuration failed.
            unsafe {
                libc::close(accepted);
            }
            return Err(Self::runtime_error());
        }
        let accepted_token = self.insert(accepted)?;
        let projected_peer = projected.is_some();
        if let Some(address) = projected {
            let mut sockets = self
                .shared
                .sockets
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let entry = sockets.get_mut(&accepted_token).ok_or(RuntimeNetworkError::Invalid)?;
            entry.guest_local = Some(address.clone());
            // The retained switch deliberately reports the listening virtual
            // endpoint as the accepted peer because an unbound AF_UNIX client
            // has no guest-visible pathname identity.
            entry.guest_peer = Some(address.clone());
            entry.switched = true;
        }
        let local = self.local_address(accepted_token);
        let peer = if projected_peer {
            self.peer_address(accepted_token)
        } else {
            Self::decode_address(&peer, length)
        };
        match (local, peer) {
            (Ok(local), Ok(peer)) => Ok(AcceptedSocket {
                token: accepted_token,
                resource: NetworkResourceKey::new(accepted_token).ok_or(RuntimeNetworkError::NoMemory)?,
                binding: Arc::new(()),
                local,
                peer,
            }),
            (Err(error), _) | (_, Err(error)) => {
                self.close(accepted_token);
                Err(error)
            }
        }
    }

    fn local_address(&self, token: u64) -> Result<SocketAddress, RuntimeNetworkError> {
        self.address_of(token, false)
    }
    fn peer_address(&self, token: u64) -> Result<SocketAddress, RuntimeNetworkError> {
        self.address_of(token, true)
    }

    fn send_to(&self, token: u64, input: &[u8], address: SocketAddress, _: bool) -> Result<usize, RuntimeNetworkError> {
        {
            let mut sockets = self
                .shared
                .sockets
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(entry) = sockets.get_mut(&token).filter(|entry| entry.icmp) {
                super::icmp::enqueue(&mut entry.icmp_packets, &mut entry.icmp_bytes, input, address)
                    .map_err(|()| RuntimeNetworkError::WouldBlock)?;
                drop(sockets);
                self.notify(token);
                return Ok(input.len());
            }
        }
        if super::resolver::Resolver::accepts(&address) {
            let mut sockets = self
                .shared
                .sockets
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let entry = sockets.get_mut(&token).ok_or(RuntimeNetworkError::Invalid)?;
            entry.resolver = true;
            if let Some(response) = super::resolver::Resolver::answer(input) {
                if !super::resolver::Resolver::queue_available(
                    entry.resolver_packets.len(),
                    entry.resolver_bytes,
                    response.len(),
                ) {
                    return Err(RuntimeNetworkError::WouldBlock);
                }
                entry.resolver_bytes += response.len();
                entry.resolver_packets.push_back(response);
            }
            drop(sockets);
            self.notify(token);
            return Ok(input.len());
        }
        let descriptor = self.descriptor(token)?;
        let (storage, length) = Self::socket_address(&address)?;
        // SAFETY: buffers and sockaddr remain valid for the duration of the call.
        let result = unsafe {
            libc::sendto(
                descriptor,
                input.as_ptr().cast(),
                input.len(),
                0,
                (&raw const storage).cast(),
                length,
            )
        };
        if result >= 0 {
            Ok(result as usize)
        } else {
            Err(Self::runtime_error())
        }
    }

    fn send_to_route(
        &self,
        token: u64,
        input: &[u8],
        route: EgressRoute,
        nonblocking: bool,
    ) -> Result<usize, RuntimeNetworkError> {
        let Some(interface) = route.interface else {
            return self.send_to(token, input, route.address, nonblocking);
        };
        if self.is_icmp(token) {
            return self.send_to(token, input, route.address, nonblocking);
        }
        let SocketAddress::Inet4 { address, port } = route.address else {
            return Err(RuntimeNetworkError::Invalid);
        };
        if address[0] == 127 {
            return self.send_to(token, input, route.address, nonblocking);
        }
        if port == 0 || self.socket_type(token)? != libc::SOCK_DGRAM {
            return Err(RuntimeNetworkError::Invalid);
        }
        let needs_source = self
            .shared
            .sockets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&token)
            .is_none_or(|entry| entry.guest_local.is_none());
        if needs_source {
            self.bind_route(
                token,
                BindRoute {
                    address: SocketAddress::Inet4 {
                        address: interface.ipv4,
                        port: 0,
                    },
                    interface: Some(interface.clone()),
                    aliases: Vec::new(),
                },
            )?;
        }
        let path = Self::switch_destination_path(&interface, address, port)?;
        let (storage, length) = Self::socket_address(&SocketAddress::Unix(path))?;
        let descriptor = self.descriptor(token)?;
        // SAFETY: input and bounded sockaddr_un remain live while the table retains descriptor.
        let result = unsafe {
            libc::sendto(
                descriptor,
                input.as_ptr().cast(),
                input.len(),
                0,
                (&raw const storage).cast(),
                length,
            )
        };
        if result >= 0 {
            Ok(result as usize)
        } else if matches!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(error) if error == libc::ENOENT || error == libc::ECONNREFUSED
        ) {
            Ok(input.len())
        } else {
            Err(Self::runtime_error())
        }
    }

    fn receive_from(
        &self,
        token: u64,
        output: &mut [u8],
        _: bool,
        peek: bool,
    ) -> Result<ReceivedDatagram, RuntimeNetworkError> {
        {
            let mut sockets = self
                .shared
                .sockets
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(entry) = sockets.get_mut(&token).filter(|entry| entry.icmp) {
                let packet = if peek {
                    entry.icmp_packets.front().cloned()
                } else {
                    entry.icmp_packets.pop_front()
                };
                let Some((packet, source)) = packet else {
                    return Err(RuntimeNetworkError::WouldBlock);
                };
                if !peek {
                    entry.icmp_bytes = entry.icmp_bytes.saturating_sub(packet.len());
                }
                let full_length = packet.len();
                let count = output.len().min(full_length);
                output[..count].copy_from_slice(&packet[..count]);
                return Ok(ReceivedDatagram {
                    count,
                    full_length,
                    source,
                });
            }
            if let Some(entry) = sockets.get_mut(&token).filter(|entry| entry.resolver) {
                let packet = if peek {
                    entry.resolver_packets.front().cloned()
                } else {
                    entry.resolver_packets.pop_front()
                };
                let Some(packet) = packet else {
                    return Err(RuntimeNetworkError::WouldBlock);
                };
                if !peek {
                    entry.resolver_bytes = entry.resolver_bytes.saturating_sub(packet.len());
                }
                let full_length = packet.len();
                let count = output.len().min(full_length);
                output[..count].copy_from_slice(&packet[..count]);
                return Ok(ReceivedDatagram {
                    count,
                    full_length,
                    source: SocketAddress::Inet4 {
                        address: [127, 0, 0, 11],
                        port: 53,
                    },
                });
            }
        }
        let descriptor = self.descriptor(token)?;
        // SAFETY: zero is valid initialization for sockaddr storage.
        let mut source = unsafe { zeroed::<libc::sockaddr_storage>() };
        let mut length = size_of::<libc::sockaddr_storage>() as libc::socklen_t;
        let flags = if peek { libc::MSG_PEEK } else { 0 } | libc::MSG_TRUNC;
        // SAFETY: output and source are writable for their supplied lengths.
        let result = unsafe {
            libc::recvfrom(
                descriptor,
                output.as_mut_ptr().cast(),
                output.len(),
                flags,
                (&raw mut source).cast(),
                &raw mut length,
            )
        };
        self.arm_read(token);
        if result < 0 {
            return Err(Self::runtime_error());
        }
        let source = Self::decode_address(&source, length)?;
        let source = match source {
            SocketAddress::Unix(path) => Self::switch_source(&path).ok_or(RuntimeNetworkError::Invalid)?,
            source => source,
        };
        Ok(ReceivedDatagram {
            count: (result as usize).min(output.len()),
            full_length: result as usize,
            source,
        })
    }

    fn send_urgent(&self, token: u64, input: &[u8]) -> Result<usize, RuntimeNetworkError> {
        let descriptor = self.descriptor(token)?;
        // SAFETY: input remains readable and the table retains the descriptor.
        let result = unsafe { libc::send(descriptor, input.as_ptr().cast(), input.len(), libc::MSG_OOB) };
        if result >= 0 {
            Ok(result as usize)
        } else {
            Err(Self::runtime_error())
        }
    }

    fn receive_urgent(&self, token: u64, output: &mut [u8], peek: bool) -> Result<usize, RuntimeNetworkError> {
        let descriptor = self.descriptor(token)?;
        // SAFETY: output remains writable and the table retains the descriptor.
        let flags = libc::MSG_OOB | if peek { libc::MSG_PEEK } else { 0 };
        let result = unsafe { libc::recv(descriptor, output.as_mut_ptr().cast(), output.len(), flags) };
        self.arm_read(token);
        if result >= 0 {
            Ok(result as usize)
        } else {
            Err(Self::runtime_error())
        }
    }

    fn at_urgent_mark(&self, token: u64) -> Result<bool, RuntimeNetworkError> {
        let descriptor = self.descriptor(token)?;
        let mut marked = 0_i32;
        #[cfg(target_os = "macos")]
        const HOST_SIOCATMARK: libc::c_ulong = 0x4004_7307;
        #[cfg(target_os = "linux")]
        const HOST_SIOCATMARK: libc::c_ulong = 0x8905;
        // SAFETY: marked is writable and the table retains the descriptor.
        if unsafe { libc::ioctl(descriptor, HOST_SIOCATMARK, std::ptr::from_mut(&mut marked)) } == 0 {
            Ok(marked != 0)
        } else {
            Err(Self::runtime_error())
        }
    }

    fn send_message(
        &self,
        token: u64,
        message: HostSend<Self::Attachment>,
    ) -> Result<HostSendResult, RuntimeNetworkError> {
        self.send_control(token, message)
    }

    fn receive_message(
        &self,
        token: u64,
        payload_limit: usize,
        control_limit: usize,
        nonblocking: bool,
        peek: bool,
    ) -> Result<HostReceive<Self::Attachment>, RuntimeNetworkError> {
        self.receive_control(token, payload_limit, control_limit, nonblocking, peek)
    }

    fn shutdown(&self, token: u64, read: bool, write: bool) -> Result<(), RuntimeNetworkError> {
        let how = match (read, write) {
            (true, true) => libc::SHUT_RDWR,
            (true, false) => libc::SHUT_RD,
            (false, true) => libc::SHUT_WR,
            _ => return Err(RuntimeNetworkError::Invalid),
        };
        // SAFETY: shutdown mutates only the owned descriptor.
        if unsafe { libc::shutdown(self.descriptor(token)?, how) } == 0 {
            Ok(())
        } else {
            Err(Self::runtime_error())
        }
    }

    fn set_option(
        &self,
        token: u64,
        level: i32,
        option: i32,
        value: hl_linux::GuestSocketOption,
    ) -> Result<(), RuntimeNetworkError> {
        self.set_socket_option(token, level, option, value)
    }

    fn get_option(
        &self,
        token: u64,
        level: i32,
        option: i32,
    ) -> Result<hl_linux::GuestSocketOption, RuntimeNetworkError> {
        super::super::socket_option::get(self.descriptor(token)?, level, option)
    }
}

impl Native {
    /// Records the bridge rendezvous a refused loopback connect retries against, then dials host
    /// loopback first so a listener that really bound loopback still wins.
    fn prepare_loopback_connect(
        &self,
        token: u64,
        interface: &hl_network::EgressInterface,
        address: [u8; 4],
        port: u16,
    ) -> Result<(), RuntimeNetworkError> {
        let peer = SocketAddress::Inet4 { address, port };
        if self.socket_type(token)? == libc::SOCK_STREAM {
            let path = Self::switch_loopback_path(interface, port)?;
            let mut sockets = self
                .shared
                .sockets
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let entry = sockets.get_mut(&token).ok_or(RuntimeNetworkError::Invalid)?;
            entry.loopback_switch = Some((path, peer.clone()));
        }
        self.prepare_connect(token, peer)
    }
}
