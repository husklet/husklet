use super::*;

impl Native {
    pub(super) fn transmit(
        &self,
        token: u64,
        input: &[u8],
        address: SocketAddress,
    ) -> Result<usize, RuntimeNetworkError> {
        {
            let mut sockets = self
                .shared
                .sockets
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(entry) = sockets.get_mut(&token).filter(|entry| entry.icmp) {
                super::super::icmp::enqueue(&mut entry.icmp_packets, &mut entry.icmp_bytes, input, address)
                    .map_err(|()| RuntimeNetworkError::WouldBlock)?;
                drop(sockets);
                self.notify(token);
                return Ok(input.len());
            }
        }
        if super::super::resolver::Resolver::accepts(&address) {
            let mut sockets = self
                .shared
                .sockets
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let entry = sockets.get_mut(&token).ok_or(RuntimeNetworkError::Invalid)?;
            entry.resolver = true;
            if let Some(response) = super::super::resolver::Resolver::answer(input) {
                if !super::super::resolver::Resolver::queue_available(
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

    pub(super) fn transmit_route(
        &self,
        token: u64,
        input: &[u8],
        route: EgressRoute,
        _: bool,
    ) -> Result<usize, RuntimeNetworkError> {
        let Some(interface) = route.interface else {
            return self.transmit(token, input, route.address);
        };
        if self.is_icmp(token) {
            return self.transmit(token, input, route.address);
        }
        let SocketAddress::Inet4 { address, port } = route.address else {
            return Err(RuntimeNetworkError::Invalid);
        };
        if address[0] == 127 {
            return self.transmit(token, input, route.address);
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
            self.bind_switch_route(
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
        } else if matches!(std::io::Error::last_os_error().raw_os_error(), Some(error) if error == libc::ENOENT || error == libc::ECONNREFUSED)
        {
            Ok(input.len())
        } else {
            Err(Self::runtime_error())
        }
    }

    pub(super) fn receive_datagram(
        &self,
        token: u64,
        output: &mut [u8],
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
        let source = match Self::decode_address(&source, length)? {
            SocketAddress::Unix(path) => Self::switch_source(&path).ok_or(RuntimeNetworkError::Invalid)?,
            source => source,
        };
        Ok(ReceivedDatagram {
            count: (result as usize).min(output.len()),
            full_length: result as usize,
            source,
        })
    }
}
