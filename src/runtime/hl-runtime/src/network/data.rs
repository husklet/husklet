use hl_linux::{Errno, GuestAccess, GuestMarshaller, GuestMemory, LinuxResult, NetworkAbi};

use crate::{
    RuntimeNetworkHost, RuntimeNetworkSyscalls, filesystem::FileErrno, network::errno::SocketErrno,
    network::wait::SocketCancellation,
};

struct ReadinessWake(std::sync::Arc<hl_sync::WaitQueue>);

impl hl_descriptor::ReadinessObserver for ReadinessWake {
    fn readiness_changed(&self) {
        self.0.notify_all();
    }
}

const MSG_DONTWAIT: u32 = 0x40;
const MSG_PEEK: u32 = 0x2;
const MSG_TRUNC: u32 = 0x20;
const MSG_WAITALL: u32 = 0x100;
const MSG_NOSIGNAL: u32 = 0x4000;
const MSG_OOB: u32 = 0x1;
const SOCKADDR_COPYOUT_CAPACITY_MAXIMUM: u32 = 4096;
const IPV4_DATAGRAM_MAXIMUM: usize = 65_507;
const IPV6_DATAGRAM_MAXIMUM: usize = 65_527;

impl<H: RuntimeNetworkHost, M: GuestMemory> RuntimeNetworkSyscalls<H, M> {
    pub(crate) fn send(&self, descriptor: i32, pointer: u64, length: u64, flags: u32) -> LinuxResult {
        if flags & !(MSG_DONTWAIT | MSG_NOSIGNAL | MSG_OOB) != 0 {
            return LinuxResult::Error(Errno::ENOSYS);
        }
        let length = match usize::try_from(length) {
            Ok(value) => value,
            Err(_) => return LinuxResult::Error(Errno::EINVAL),
        };
        let mut input = Vec::new();
        if input.try_reserve_exact(length).is_err() {
            return LinuxResult::Error(Errno::ENOMEM);
        }
        input.resize(length, 0);
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        let copied = marshaller.copy_from(pointer, &mut input);
        if copied.fault.is_some() {
            return LinuxResult::Error(Errno::EFAULT);
        }
        let socket = match self.lookup(descriptor) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        if socket.netlink_socket().is_some() {
            return match socket.write_with(&input, true) {
                Ok(count) => LinuxResult::Value(count as u64),
                Err(error) => LinuxResult::Error(FileErrno::object(error)),
            };
        }
        if flags & MSG_OOB != 0 {
            let crate::RuntimeSocketKind::Host { token, .. } = &socket.kind else {
                return LinuxResult::Error(Errno::EOPNOTSUPP);
            };
            let Some(host) = &self.host else {
                return LinuxResult::Error(Errno::ENOSYS);
            };
            return match host.send_urgent(*token, &input) {
                Ok(count) => LinuxResult::Value(count as u64),
                Err(error) => LinuxResult::Error(SocketErrno::runtime(error)),
            };
        }
        if let Err(error) = Self::datagram_send(&socket, input.len(), false) {
            return LinuxResult::Error(error);
        }
        let nonblocking = flags & MSG_DONTWAIT != 0
            || socket
                .snapshot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .nonblocking;
        match self.write_socket(&socket, &input, nonblocking) {
            Ok(count) => LinuxResult::Value(count as u64),
            Err(error) => LinuxResult::Error(error),
        }
    }

    pub(crate) fn write_socket(
        &self,
        socket: &crate::RuntimeSocket<H>,
        input: &[u8],
        nonblocking: bool,
    ) -> Result<usize, Errno> {
        if socket.unix_datagram().is_some() {
            return self.send_unix_datagram(socket, input, None);
        }
        let result = if nonblocking {
            socket.write_with(&input, true)
        } else if let Some(wait) = &self.wait {
            let cancellation = SocketCancellation::new(wait.interruption());
            socket.write_blocking(&input, &cancellation)
        } else {
            socket.write_with(&input, false)
        };
        result.map_err(FileErrno::object)
    }

    pub(crate) fn sendto(
        &self,
        descriptor: i32,
        pointer: u64,
        length: u64,
        flags: u32,
        address: u64,
        address_length: u32,
    ) -> LinuxResult {
        if address == 0 && address_length == 0 {
            return self.send(descriptor, pointer, length, flags);
        }
        if flags & !(MSG_DONTWAIT | MSG_NOSIGNAL) != 0 {
            return LinuxResult::Error(Errno::ENOSYS);
        }
        let length = match usize::try_from(length) {
            Ok(value) => value,
            Err(_) => return LinuxResult::Error(Errno::EINVAL),
        };
        let mut input = Vec::new();
        if input.try_reserve_exact(length).is_err() {
            return LinuxResult::Error(Errno::ENOMEM);
        }
        input.resize(length, 0);
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        if marshaller.copy_from(pointer, &mut input).fault.is_some() {
            return LinuxResult::Error(Errno::EFAULT);
        }
        let socket = match self.lookup(descriptor) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        if socket.netlink_socket().is_some() {
            return match socket.write_with(&input, true) {
                Ok(count) => LinuxResult::Value(count as u64),
                Err(error) => LinuxResult::Error(FileErrno::object(error)),
            };
        }
        let address = match NetworkAbi::new(&self.memory, self.architecture)
            .decode_sockaddr(address, address_length)
            .and_then(Self::host_address)
        {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(SocketErrno::marshal(error)),
        };
        if let Err(error) = Self::datagram_send(&socket, input.len(), true) {
            return LinuxResult::Error(error);
        }
        if socket.unix_datagram().is_some() {
            let hl_network::SocketAddress::Unix(raw) = address else {
                return LinuxResult::Error(Errno::EAFNOSUPPORT);
            };
            let target = Self::unix_address(raw);
            return match self.send_unix_datagram(&socket, &input, Some(target)) {
                Ok(count) => LinuxResult::Value(count as u64),
                Err(error) => LinuxResult::Error(error),
            };
        }
        let crate::RuntimeSocketKind::Host { token, .. } = &socket.kind else {
            return LinuxResult::Error(Errno::EAFNOSUPPORT);
        };
        let Some(host) = &self.host else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        if let Err(error) = self.route(&address) {
            return LinuxResult::Error(error);
        }
        let nonblocking = flags & MSG_DONTWAIT != 0
            || socket
                .snapshot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .nonblocking;
        match host.send_to_route(*token, &input, self.connect_route(address), nonblocking) {
            Ok(count) => LinuxResult::Value(count as u64),
            Err(error) => LinuxResult::Error(SocketErrno::runtime(error)),
        }
    }

    pub(crate) fn recv(&self, descriptor: i32, pointer: u64, length: u64, flags: u32) -> LinuxResult {
        self.recvfrom(descriptor, pointer, length, flags, 0, 0)
    }

    pub(crate) fn recvfrom(
        &self,
        descriptor: i32,
        pointer: u64,
        length: u64,
        flags: u32,
        address_pointer: u64,
        length_pointer: u64,
    ) -> LinuxResult {
        if flags & !(MSG_DONTWAIT | MSG_PEEK | MSG_TRUNC | MSG_WAITALL | MSG_OOB) != 0 {
            return LinuxResult::Error(Errno::ENOSYS);
        }
        let length = match usize::try_from(length) {
            Ok(value) => value,
            Err(_) => return LinuxResult::Error(Errno::EINVAL),
        };
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        match marshaller.probe(pointer, length, GuestAccess::Write) {
            Ok(available) if available == length => {}
            Ok(_) => return LinuxResult::Error(Errno::EFAULT),
            Err(error) => return LinuxResult::Error(error.errno()),
        }
        if address_pointer != 0 {
            if let Err(error) = marshaller.socklen(length_pointer, SOCKADDR_COPYOUT_CAPACITY_MAXIMUM) {
                return LinuxResult::Error(error.errno());
            }
        }
        let socket = match self.lookup(descriptor) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        if flags & MSG_OOB != 0 {
            let crate::RuntimeSocketKind::Host { token, .. } = &socket.kind else {
                return LinuxResult::Error(Errno::EOPNOTSUPP);
            };
            let Some(host) = &self.host else {
                return LinuxResult::Error(Errno::ENOSYS);
            };
            let mut output = vec![0; length];
            return match host.receive_urgent(*token, &mut output, flags & MSG_PEEK != 0) {
                Ok(count) if count <= output.len() => {
                    let copied = marshaller.copy_to(pointer, &output[..count]);
                    if copied.fault.is_some() {
                        LinuxResult::Error(Errno::EFAULT)
                    } else {
                        LinuxResult::Value(count as u64)
                    }
                }
                Ok(_) => LinuxResult::Error(Errno::EIO),
                Err(error) => LinuxResult::Error(SocketErrno::runtime(error)),
            };
        }
        let nonblocking = flags & MSG_DONTWAIT != 0
            || socket
                .snapshot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .nonblocking;
        let mut output = vec![0; length];
        let received = self.receive_from_socket(
            &socket,
            &mut output,
            nonblocking,
            address_pointer != 0,
            flags & MSG_PEEK != 0,
            flags & MSG_WAITALL != 0,
        );
        let (count, full_length, source) = match received {
            Ok(value) if value.0 <= output.len() && value.0 <= value.1 => value,
            Ok(_) => return LinuxResult::Error(Errno::EIO),
            Err(Errno::EAGAIN)
                if !nonblocking && matches!(&socket.kind, crate::RuntimeSocketKind::UnixStandalone { .. }) =>
            {
                return LinuxResult::Restart(hl_linux::RestartKind::NoInterrupt);
            }
            Err(error) => return LinuxResult::Error(error),
        };
        let staged_address = match source {
            Some(source) => {
                match NetworkAbi::new(&self.memory, self.architecture).prepare_sockaddr_copyout(
                    address_pointer,
                    length_pointer,
                    &Self::guest_address(&source),
                ) {
                    Ok(value) => Some(value),
                    Err(error) => {
                        return LinuxResult::Error(SocketErrno::marshal(error));
                    }
                }
            }
            None => None,
        };
        let copied = marshaller.copy_to(pointer, &output[..count]);
        if copied.fault.is_some() {
            return LinuxResult::Error(Errno::EFAULT);
        }
        if let Some(staged) = staged_address {
            if let Err(error) = staged.commit(&marshaller) {
                return LinuxResult::Error(SocketErrno::marshal(error));
            }
        }
        let record_oriented = socket
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .socket_type
            != hl_network::SocketType::Stream;
        let reported = if record_oriented && flags & MSG_TRUNC != 0 {
            full_length
        } else {
            count
        };
        LinuxResult::Value(reported as u64)
    }

    pub(crate) fn receive_from_socket(
        &self,
        socket: &crate::RuntimeSocket<H>,
        output: &mut [u8],
        nonblocking: bool,
        source_requested: bool,
        peek: bool,
        waitall: bool,
    ) -> Result<(usize, usize, Option<hl_network::SocketAddress>), Errno> {
        if let Some(netlink) = socket.netlink_socket() {
            return netlink
                .receive(output, peek)
                .map(|(count, full)| (count, full, None))
                .map_err(FileErrno::object);
        }
        let socket_type = socket
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .socket_type;
        let requested_source = if source_requested {
            socket
                .snapshot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .peer
                .clone()
        } else {
            None
        };
        if let Some(datagram) = socket.unix_datagram() {
            return datagram
                .receive(output, peek)
                .map(|received| {
                    let source = source_requested.then(|| Self::socket_address(received.source));
                    (received.count, received.full_length, source)
                })
                .map_err(Self::unix_datagram_errno);
        }
        match &socket.kind {
            crate::RuntimeSocketKind::Host { description, token } => self.host_receive(
                socket,
                description,
                *token,
                output,
                nonblocking,
                source_requested,
                peek,
                waitall,
                socket_type,
            ),
            crate::RuntimeSocketKind::Unix { pair, endpoint } => self.unix_receive(
                socket,
                &pair.endpoints[*endpoint],
                output,
                nonblocking,
                source_requested,
                requested_source,
                peek,
                waitall,
                socket_type,
            ),
            crate::RuntimeSocketKind::UnixStandalone { .. } => {
                let Some((pair, endpoint)) = socket.standalone_connection() else {
                    return Err(Errno::ENOTCONN);
                };
                self.unix_receive(
                    socket,
                    &pair.endpoints[endpoint],
                    output,
                    nonblocking,
                    source_requested,
                    requested_source,
                    peek,
                    waitall,
                    socket_type,
                )
            }
        }
    }

    pub(crate) fn send_unix_datagram(
        &self,
        socket: &crate::RuntimeSocket<H>,
        input: &[u8],
        explicit: Option<hl_network::UnixAddress>,
    ) -> Result<usize, Errno> {
        let datagram = socket.unix_datagram().ok_or(Errno::ENOTSOCK)?;
        let target = explicit.or_else(|| datagram.connected()).ok_or(Errno::ENOTCONN)?;
        let destination_id = self
            .sockets
            .unix_namespace()
            .resolve(&target)
            .ok_or(Errno::ECONNREFUSED)?;
        let destination = self.sockets.get_id(destination_id).ok_or(Errno::ECONNREFUSED)?;
        let queue = destination.unix_datagram().ok_or(Errno::ECONNREFUSED)?;
        let source = socket
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .local
            .clone()
            .and_then(|address| match address {
                hl_network::SocketAddress::Unix(raw) => Some(Self::unix_address(raw)),
                _ => None,
            })
            .unwrap_or(hl_network::UnixAddress::Unnamed);
        queue.enqueue(input, source).map_err(Self::unix_datagram_errno)
    }

    pub(crate) fn datagram_send(
        socket: &crate::RuntimeSocket<H>,
        length: usize,
        has_destination: bool,
    ) -> Result<(), Errno> {
        let snapshot = socket
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if snapshot.socket_type != hl_network::SocketType::Datagram {
            return Ok(());
        }
        let maximum = match snapshot.family {
            hl_network::AddressFamily::Inet4 => Some(IPV4_DATAGRAM_MAXIMUM),
            hl_network::AddressFamily::Inet6 => Some(IPV6_DATAGRAM_MAXIMUM),
            _ => None,
        };
        if maximum.is_some_and(|maximum| length > maximum) {
            return Err(Errno::EMSGSIZE);
        }
        if !has_destination && snapshot.peer.is_none() {
            return Err(Errno::EDESTADDRREQ);
        }
        Ok(())
    }

    pub(crate) fn unix_address(raw: Vec<u8>) -> hl_network::UnixAddress {
        if raw.is_empty() {
            hl_network::UnixAddress::Unnamed
        } else if raw[0] == 0 {
            hl_network::UnixAddress::Abstract(raw[1..].to_vec())
        } else {
            hl_network::UnixAddress::Pathname(raw)
        }
    }

    fn socket_address(address: hl_network::UnixAddress) -> hl_network::SocketAddress {
        match address {
            hl_network::UnixAddress::Unnamed => hl_network::SocketAddress::Unix(Vec::new()),
            hl_network::UnixAddress::Pathname(value) => hl_network::SocketAddress::Unix(value),
            hl_network::UnixAddress::Abstract(value) => hl_network::SocketAddress::Unix([vec![0], value].concat()),
        }
    }

    fn unix_datagram_errno(error: hl_network::UnixDatagramError) -> Errno {
        match error {
            hl_network::UnixDatagramError::WouldBlock => Errno::EAGAIN,
            hl_network::UnixDatagramError::MessageTooLarge => Errno::EMSGSIZE,
            hl_network::UnixDatagramError::Closed => Errno::ECONNREFUSED,
            hl_network::UnixDatagramError::Invalid => Errno::EINVAL,
        }
    }

    fn host_receive(
        &self,
        socket: &crate::RuntimeSocket<H>,
        description: &std::sync::Arc<hl_network::SocketDescription<H>>,
        token: H::Token,
        output: &mut [u8],
        nonblocking: bool,
        source_requested: bool,
        peek: bool,
        waitall: bool,
        socket_type: hl_network::SocketType,
    ) -> Result<(usize, usize, Option<hl_network::SocketAddress>), Errno> {
        if socket_type == hl_network::SocketType::Stream && !source_requested && !peek {
            if let Some(deadline) = self.socket_deadline(socket)? {
                let count = self.read_stream_until(socket, description, output, waitall, deadline)?;
                return Ok((count, count, None));
            }
            let count = self.read_stream(socket, output, nonblocking, waitall)?;
            return Ok((count, count, None));
        }
        let host = self.host.as_ref().ok_or(Errno::ENOSYS)?;
        loop {
            match host.receive_from(token, output, true, peek) {
                Ok(received) => {
                    return Ok((
                        received.count,
                        received.full_length,
                        source_requested.then_some(received.source),
                    ));
                }
                Err(crate::RuntimeNetworkError::WouldBlock) if !nonblocking => {}
                Err(error) => return Err(SocketErrno::runtime(error)),
            }
            let wait = self.wait.as_ref().ok_or(Errno::EAGAIN)?;
            let cancellation = SocketCancellation::new(wait.interruption());
            description.wait_readable(&cancellation).map_err(FileErrno::object)?;
        }
    }

    fn read_stream_until(
        &self,
        socket: &crate::RuntimeSocket<H>,
        description: &std::sync::Arc<hl_network::SocketDescription<H>>,
        output: &mut [u8],
        waitall: bool,
        deadline: hl_time::Deadline,
    ) -> Result<usize, Errno> {
        let wait = self.wait.as_ref().ok_or(Errno::EAGAIN)?;
        let queue = std::sync::Arc::new(hl_sync::WaitQueue::new());
        let _subscription = description
            .observe_readiness(std::sync::Arc::new(ReadinessWake(queue.clone())))
            .map_err(FileErrno::object)?;
        let mut count = 0;
        loop {
            let observed = queue.observation();
            match socket.read_with(&mut output[count..], true) {
                Ok(0) => return Ok(count),
                Ok(read) => {
                    count += read;
                    if !waitall || count == output.len() {
                        return Ok(count);
                    }
                }
                Err(hl_descriptor::ObjectError::WouldBlock) => match wait.wait(&queue, observed, Some(deadline)) {
                    Ok(hl_sync::WaitOutcome::Notified) => {}
                    Ok(hl_sync::WaitOutcome::Interrupted) => return Err(Errno::EINTR),
                    Ok(hl_sync::WaitOutcome::TimedOut) if count == 0 => return Err(Errno::EAGAIN),
                    Ok(hl_sync::WaitOutcome::TimedOut) => return Ok(count),
                    Err(_) => return Err(Errno::EIO),
                },
                Err(error) if count == 0 => return Err(FileErrno::object(error)),
                Err(_) => return Ok(count),
            }
        }
    }

    fn unix_receive(
        &self,
        socket: &crate::RuntimeSocket<H>,
        endpoint: &hl_network::UnixSocketEndpoint,
        output: &mut [u8],
        nonblocking: bool,
        source_requested: bool,
        requested_source: Option<hl_network::SocketAddress>,
        peek: bool,
        waitall: bool,
        socket_type: hl_network::SocketType,
    ) -> Result<(usize, usize, Option<hl_network::SocketAddress>), Errno> {
        if socket_type == hl_network::SocketType::Stream {
            let count = if peek {
                self.peek_stream(endpoint, output, nonblocking)?
            } else {
                self.read_stream(socket, output, nonblocking, waitall)?
            };
            return Ok((count, count, requested_source));
        }
        if matches!(&socket.kind, crate::RuntimeSocketKind::UnixStandalone { .. }) && !source_requested && !peek {
            let count = if nonblocking {
                socket.read_with(output, true).map_err(FileErrno::object)?
            } else if let Some(wait) = &self.wait {
                let cancellation = SocketCancellation::new(wait.interruption());
                socket.read_blocking(output, &cancellation).map_err(FileErrno::object)?
            } else {
                socket.read_with(output, true).map_err(FileErrno::object)?
            };
            return Ok((count, count, None));
        }
        let source = if source_requested {
            Some(requested_source.ok_or(Errno::ENOTCONN)?)
        } else {
            None
        };
        endpoint
            .receive_record(output, true, peek)
            .map(|(count, full_length)| (count, full_length, source))
            .map_err(SocketErrno::socket_host)
    }

    fn read_stream(
        &self,
        socket: &crate::RuntimeSocket<H>,
        output: &mut [u8],
        nonblocking: bool,
        waitall: bool,
    ) -> Result<usize, Errno> {
        let mut count = 0;
        loop {
            let result = if nonblocking {
                socket.read_with(&mut output[count..], true)
            } else if let Some(wait) = &self.wait {
                let cancellation = SocketCancellation::new(wait.interruption());
                socket.read_blocking(&mut output[count..], &cancellation)
            } else {
                socket.read_with(&mut output[count..], false)
            };
            match result {
                Ok(0) => return Ok(count),
                Ok(read) => count += read,
                Err(error) if count == 0 => return Err(FileErrno::object(error)),
                Err(_) => return Ok(count),
            }
            if !waitall || count == output.len() {
                return Ok(count);
            }
        }
    }

    fn peek_stream(
        &self,
        endpoint: &hl_network::UnixSocketEndpoint,
        output: &mut [u8],
        nonblocking: bool,
    ) -> Result<usize, Errno> {
        loop {
            match endpoint.peek(output, true) {
                Ok(count) => return Ok(count),
                Err(hl_network::SocketHostError::WouldBlock) if !nonblocking => {}
                Err(error) => return Err(SocketErrno::socket_host(error)),
            }
            let Some(wait) = &self.wait else {
                return Err(Errno::EAGAIN);
            };
            let cancellation = SocketCancellation::new(wait.interruption());
            endpoint
                .description
                .wait_readable(&cancellation)
                .map_err(FileErrno::object)?;
        }
    }
}
