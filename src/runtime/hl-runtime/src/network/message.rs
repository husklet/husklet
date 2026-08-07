use std::cell::Cell;

use hl_linux::{
    Errno, GuestMarshaller, GuestMemory, GuestSocketOption, LinuxResult, MessageCopyoutResult, MessageImport,
    NetworkAbi, NetworkMarshalError,
};
use hl_network::{ControlError, UnixTransportError};

use crate::{
    RuntimeNetworkHost, RuntimeNetworkSyscalls, RuntimeSocket, RuntimeSocketKind, filesystem::FileErrno,
    network::errno::SocketErrno,
};

const MSG_CTRUNC: u32 = 0x8;
const MSG_PEEK: u32 = 0x2;
const MSG_TRUNC: u32 = 0x20;
const MSG_DONTWAIT: u32 = 0x40;
const MSG_CMSG_CLOEXEC: u32 = 0x4000_0000;
const MESSAGE_VECTOR_MAXIMUM: u32 = 1024;
const MULTI_MESSAGE_SIZE: usize = 64;
const MULTI_MESSAGE_LENGTH_OFFSET: u64 = 56;
const MSG_WAITFORONE: u32 = 0x1_0000;
const SOL_SOCKET: i32 = 1;
const SO_RCVTIMEO: i32 = 20;

enum BatchReceiveEntry {
    Committed,
    Retry,
    Failed(LinuxResult),
}

impl<H: RuntimeNetworkHost, M: GuestMemory> RuntimeNetworkSyscalls<H, M> {
    pub(crate) fn sendmmsg(&self, descriptor: i32, messages: u64, count: u32, flags: u32) -> LinuxResult {
        if count > MESSAGE_VECTOR_MAXIMUM {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let bytes = count as usize * MULTI_MESSAGE_SIZE;
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        for access in [hl_linux::GuestAccess::Read, hl_linux::GuestAccess::Write] {
            match marshaller.probe(messages, bytes, access) {
                Ok(available) if available == bytes => {}
                _ => return LinuxResult::Error(Errno::EFAULT),
            }
        }
        let mut completed = 0_u32;
        while completed < count {
            let header = messages + completed as u64 * MULTI_MESSAGE_SIZE as u64;
            let length = match self.sendmsg(descriptor, header, flags) {
                LinuxResult::Value(value) => value,
                LinuxResult::Error(error) if completed == 0 => {
                    return LinuxResult::Error(error);
                }
                LinuxResult::Error(_) => break,
                LinuxResult::Restart(kind) if completed == 0 => {
                    return LinuxResult::Restart(kind);
                }
                LinuxResult::Restart(_) => break,
            };
            match self.write_message_length(&marshaller, header, length) {
                Ok(()) => {}
                Err(error) if completed == 0 => return LinuxResult::Error(error),
                Err(_) => break,
            }
            completed += 1;
        }
        LinuxResult::Value(completed as u64)
    }

    fn write_message_length(&self, marshaller: &GuestMarshaller<'_, M>, header: u64, length: u64) -> Result<(), Errno> {
        let encoded = (length as u32).to_le_bytes();
        if marshaller
            .copy_to(header + MULTI_MESSAGE_LENGTH_OFFSET, &encoded)
            .fault
            .is_some()
        {
            return Err(Errno::EFAULT);
        }
        Ok(())
    }

    pub(crate) fn recvmmsg(&self, descriptor: i32, messages: u64, count: u32, flags: u32, timeout: u64) -> LinuxResult {
        if count > MESSAGE_VECTOR_MAXIMUM {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let bytes = count as usize * MULTI_MESSAGE_SIZE;
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        for access in [hl_linux::GuestAccess::Read, hl_linux::GuestAccess::Write] {
            match marshaller.probe(messages, bytes, access) {
                Ok(available) if available == bytes => {}
                _ => return LinuxResult::Error(Errno::EFAULT),
            }
        }
        let (deadline, zero_timeout) = match self.receive_deadline(timeout, &marshaller) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let socket = match self.lookup(descriptor) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let endpoint = match &socket.kind {
            RuntimeSocketKind::Unix { pair, endpoint } => Some(&pair.endpoints[*endpoint]),
            RuntimeSocketKind::Host { .. } | RuntimeSocketKind::UnixStandalone { .. } => None,
        };
        let mut completed = 0_u32;
        loop {
            if completed >= count {
                break;
            }
            let header = messages + completed as u64 * MULTI_MESSAGE_SIZE as u64;
            let later_nonblocking = completed > 0;
            let nested_flags = (flags & !MSG_WAITFORONE)
                | if later_nonblocking { MSG_DONTWAIT } else { 0 }
                | if zero_timeout { MSG_DONTWAIT } else { 0 };
            match self.receive_batch_entry(
                descriptor,
                header,
                nested_flags,
                completed,
                zero_timeout,
                endpoint,
                deadline,
                &marshaller,
            ) {
                BatchReceiveEntry::Committed => completed += 1,
                BatchReceiveEntry::Retry => continue,
                BatchReceiveEntry::Failed(result) => {
                    self.copy_remaining_timeout(timeout, deadline, &marshaller);
                    return result;
                }
            }
        }
        self.copy_remaining_timeout(timeout, deadline, &marshaller);
        LinuxResult::Value(completed as u64)
    }

    fn receive_deadline(
        &self,
        pointer: u64,
        marshaller: &GuestMarshaller<'_, M>,
    ) -> Result<(Option<hl_time::Deadline>, bool), Errno> {
        if pointer == 0 {
            return Ok((None, false));
        }
        let mut bytes = [0_u8; 16];
        if marshaller.copy_from(pointer, &mut bytes).fault.is_some() {
            return Err(Errno::EFAULT);
        }
        let seconds = i64::from_le_bytes(bytes[..8].try_into().unwrap());
        let nanoseconds = i64::from_le_bytes(bytes[8..].try_into().unwrap());
        if seconds < 0 || !(0..1_000_000_000).contains(&nanoseconds) {
            return Err(Errno::EINVAL);
        }
        let duration = (seconds as u64)
            .checked_mul(1_000_000_000)
            .and_then(|value| value.checked_add(nanoseconds as u64))
            .ok_or(Errno::EINVAL)?;
        if duration == 0 {
            return Ok((Some(hl_time::Deadline::from_nanoseconds(0)), true));
        }
        let wait = self.wait.as_ref().ok_or(Errno::ENOSYS)?;
        let now = wait.monotonic_now().map_err(|_| Errno::EIO)?;
        Ok((
            Some(now.deadline_after(hl_time::Duration::from_nanoseconds(duration))),
            false,
        ))
    }

    fn batch_write_failure(completed: u32, error: Errno) -> LinuxResult {
        match completed {
            0 => LinuxResult::Error(error),
            value => LinuxResult::Value(value as u64),
        }
    }

    fn receive_batch_entry(
        &self,
        descriptor: i32,
        header: u64,
        flags: u32,
        completed: u32,
        zero_timeout: bool,
        endpoint: Option<&hl_network::UnixSocketEndpoint>,
        deadline: Option<hl_time::Deadline>,
        marshaller: &GuestMarshaller<'_, M>,
    ) -> BatchReceiveEntry {
        match self.receive_message(descriptor, header, flags, false) {
            LinuxResult::Value(length) => match self.write_message_length(marshaller, header, length) {
                Ok(()) => BatchReceiveEntry::Committed,
                Err(error) => BatchReceiveEntry::Failed(Self::batch_write_failure(completed, error)),
            },
            LinuxResult::Error(Errno::EAGAIN) if zero_timeout => {
                BatchReceiveEntry::Failed(LinuxResult::Value(completed as u64))
            }
            LinuxResult::Error(Errno::EAGAIN) if flags & MSG_DONTWAIT != 0 => {
                let result = match completed {
                    0 => LinuxResult::Error(Errno::EAGAIN),
                    value => LinuxResult::Value(value as u64),
                };
                BatchReceiveEntry::Failed(result)
            }
            LinuxResult::Error(Errno::EAGAIN) => {
                let Some(endpoint) = endpoint else {
                    return BatchReceiveEntry::Failed(Self::batch_write_failure(completed, Errno::EAGAIN));
                };
                match self.wait_for_message(endpoint, deadline) {
                    Ok(true) => BatchReceiveEntry::Retry,
                    Ok(false) => BatchReceiveEntry::Failed(LinuxResult::Value(completed as u64)),
                    Err(error) => BatchReceiveEntry::Failed(Self::batch_write_failure(completed, error)),
                }
            }
            result => BatchReceiveEntry::Failed(result),
        }
    }

    fn wait_for_message(
        &self,
        endpoint: &hl_network::UnixSocketEndpoint,
        deadline: Option<hl_time::Deadline>,
    ) -> Result<bool, Errno> {
        let wait = self.wait.as_ref().ok_or(Errno::ENOSYS)?;
        loop {
            let observed = endpoint.message_wait_queue().observation();
            if endpoint.message_ready() || endpoint.readable_bytes() != 0 {
                return Ok(true);
            }
            match wait.wait(endpoint.message_wait_queue(), observed, deadline) {
                Ok(hl_sync::WaitOutcome::Notified) => {}
                Ok(hl_sync::WaitOutcome::Interrupted) => return Err(Errno::EINTR),
                Ok(hl_sync::WaitOutcome::TimedOut) => return Ok(false),
                Err(_) => return Err(Errno::EIO),
            }
        }
    }

    fn copy_remaining_timeout(
        &self,
        pointer: u64,
        deadline: Option<hl_time::Deadline>,
        marshaller: &GuestMarshaller<'_, M>,
    ) {
        let (Some(deadline), Some(wait)) = (deadline, self.wait.as_ref()) else {
            return;
        };
        if pointer == 0 {
            return;
        }
        let Ok(now) = wait.monotonic_now() else {
            return;
        };
        let remaining = deadline.remaining_at(now).timespec();
        let bytes = [
            remaining.seconds().to_le_bytes(),
            (remaining.subsecond_nanoseconds() as u64).to_le_bytes(),
        ]
        .concat();
        let _result = marshaller.copy_to(pointer, &bytes);
    }

    pub(crate) fn sendmsg(&self, descriptor: i32, header: u64, flags: u32) -> LinuxResult {
        let abi = NetworkAbi::new(&self.memory, self.architecture);
        let imported = match abi.import_message(header, flags) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(SocketErrno::marshal(error)),
        };
        let mut payload = Vec::new();
        let Ok(length) = usize::try_from(imported.vectors.total_length) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        if payload.try_reserve_exact(length).is_err() {
            return LinuxResult::Error(Errno::ENOMEM);
        }
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        for vector in &imported.vectors.vectors {
            let start = payload.len();
            payload.resize(start + vector.length as usize, 0);
            if marshaller.copy_from(vector.base, &mut payload[start..]).fault.is_some() {
                return LinuxResult::Error(Errno::EFAULT);
            }
        }
        let socket = match self.lookup(descriptor) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        if socket.netlink_socket().is_some() {
            return match socket.write_with(&payload, true) {
                Ok(count) => LinuxResult::Value(count as u64),
                Err(error) => LinuxResult::Error(FileErrno::object(error)),
            };
        }
        if let Err(error) = Self::datagram_send(&socket, payload.len(), imported.address.is_some()) {
            return LinuxResult::Error(error);
        }
        let nonblocking = flags & MSG_DONTWAIT != 0
            || socket
                .snapshot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .nonblocking;
        if socket.unix_datagram().is_some() {
            if !imported.controls.is_empty() {
                return LinuxResult::Error(Errno::EOPNOTSUPP);
            }
            let explicit = match imported.address.map(Self::host_address).transpose() {
                Ok(Some(hl_network::SocketAddress::Unix(raw))) => Some(Self::unix_address(raw)),
                Ok(Some(_)) => return LinuxResult::Error(Errno::EAFNOSUPPORT),
                Ok(None) => None,
                Err(error) => return LinuxResult::Error(SocketErrno::marshal(error)),
            };
            return match self.send_unix_datagram(&socket, &payload, explicit) {
                Ok(count) => LinuxResult::Value(count as u64),
                Err(Errno::EAGAIN) if !nonblocking => LinuxResult::Restart(hl_linux::RestartKind::NoInterrupt),
                Err(error) => LinuxResult::Error(error),
            };
        }
        let RuntimeSocketKind::Unix { pair, endpoint } = &socket.kind else {
            return self.send_host(&socket, payload, imported.address, imported.controls, nonblocking);
        };
        if imported.address.is_some() {
            return LinuxResult::Error(Errno::EOPNOTSUPP);
        }
        if imported.controls.is_empty()
            && socket
                .snapshot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .socket_type
                == hl_network::SocketType::Stream
        {
            return match socket.write_with(&payload, true) {
                Ok(count) => LinuxResult::Value(count as u64),
                Err(hl_descriptor::ObjectError::WouldBlock) if !nonblocking => {
                    LinuxResult::Restart(hl_linux::RestartKind::NoInterrupt)
                }
                Err(error) => LinuxResult::Error(FileErrno::object(error)),
            };
        }
        match pair.endpoints[*endpoint].send_message_with(
            &self.descriptors,
            payload,
            imported.controls,
            || self.credentials.as_ref().and_then(|credentials| credentials.current()),
            nonblocking,
        ) {
            Ok(()) => LinuxResult::Value(length as u64),
            Err(error) => LinuxResult::Error(Self::unix_error(error)),
        }
    }

    pub(crate) fn recvmsg(&self, descriptor: i32, header: u64, flags: u32) -> LinuxResult {
        self.receive_message(descriptor, header, flags, true)
    }

    fn receive_message(&self, descriptor: i32, header: u64, flags: u32, blocking: bool) -> LinuxResult {
        let abi = NetworkAbi::new(&self.memory, self.architecture);
        let imported = match abi.import_receive_message(header, flags) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(SocketErrno::marshal(error)),
        };
        let socket = match self.lookup(descriptor) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        if let Some(netlink) = socket.netlink_socket() {
            let mut payload = vec![0; imported.vectors.total_length as usize];
            let (count, full) = match netlink.receive(&mut payload, flags & MSG_PEEK != 0) {
                Ok(value) => value,
                Err(hl_descriptor::ObjectError::WouldBlock) if blocking => {
                    return LinuxResult::Restart(hl_linux::RestartKind::NoInterrupt);
                }
                Err(error) => return LinuxResult::Error(FileErrno::object(error)),
            };
            payload.truncate(count);
            let result = MessageCopyoutResult {
                address: Some(hl_linux::GuestNetworkAddress::Netlink { port: 0, groups: 0 }),
                data: payload,
                controls: Vec::new(),
                flags: if count < full { MSG_TRUNC } else { 0 },
            };
            let staged = match abi.prepare_receive(&imported, &result) {
                Ok(value) => value,
                Err(error) => return LinuxResult::Error(SocketErrno::marshal(error)),
            };
            return match staged.commit(&GuestMarshaller::new(&self.memory, self.architecture)) {
                Ok(()) => LinuxResult::Value(if flags & MSG_TRUNC != 0 {
                    full as u64
                } else {
                    count as u64
                }),
                Err(error) => LinuxResult::Error(SocketErrno::marshal(error)),
            };
        }
        if matches!(&socket.kind, RuntimeSocketKind::Host { .. }) {
            return self.recv_host(&socket, &imported, flags, &abi);
        }
        let RuntimeSocketKind::Unix { pair, endpoint } = &socket.kind else {
            unreachable!()
        };
        let nonblocking = flags & MSG_DONTWAIT != 0
            || socket
                .snapshot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .nonblocking;
        let mut deadline = None;
        let mut timeout_armed = false;
        loop {
            let result = self.receive_unix(&socket, &pair.endpoints[*endpoint], &imported, flags, &abi);
            match result {
                LinuxResult::Error(Errno::EAGAIN) if blocking && !nonblocking => {}
                result => return result,
            }
            if !timeout_armed {
                deadline = match self.socket_deadline(&socket) {
                    Ok(value) => value,
                    Err(error) => return LinuxResult::Error(error),
                };
                timeout_armed = true;
            }
            match self.wait_for_message(&pair.endpoints[*endpoint], deadline) {
                Ok(true) => {}
                Ok(false) => return LinuxResult::Error(Errno::EAGAIN),
                Err(error) => return LinuxResult::Error(error),
            }
        }
    }

    pub(super) fn socket_deadline(&self, socket: &RuntimeSocket<H>) -> Result<Option<hl_time::Deadline>, Errno> {
        let Some(GuestSocketOption::Timeval { seconds, microseconds }) = socket.option(SOL_SOCKET, SO_RCVTIMEO) else {
            return Ok(None);
        };
        if seconds == 0 && microseconds == 0 {
            return Ok(None);
        }
        let duration = (seconds as u64)
            .checked_mul(1_000_000_000)
            .and_then(|value| value.checked_add(microseconds as u64 * 1_000))
            .ok_or(Errno::EINVAL)?;
        let wait = self.wait.as_ref().ok_or(Errno::ENOSYS)?;
        let now = wait.monotonic_now().map_err(|_| Errno::EIO)?;
        Ok(Some(now.deadline_after(hl_time::Duration::from_nanoseconds(duration))))
    }

    fn receive_unix(
        &self,
        socket: &RuntimeSocket<H>,
        endpoint: &hl_network::UnixSocketEndpoint,
        imported: &MessageImport,
        flags: u32,
        abi: &NetworkAbi<'_, M>,
    ) -> LinuxResult {
        let reported = Cell::new(0_usize);
        let failure = Cell::new(None);
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        let received = endpoint.receive_message_transactional(
            &self.descriptors,
            imported.header.control_length,
            flags & MSG_CMSG_CLOEXEC != 0,
            flags & MSG_PEEK != 0,
            |payload, control| {
                let count = payload.len().min(imported.vectors.total_length as usize);
                let result = MessageCopyoutResult {
                    address: None,
                    data: payload[..count].to_vec(),
                    controls: control.controls.clone(),
                    flags: (if control.truncated { MSG_CTRUNC } else { 0 })
                        | (if count < payload.len() { MSG_TRUNC } else { 0 }),
                };
                let staged = abi.prepare_receive(&imported, &result).map_err(|error| {
                    failure.set(Some(SocketErrno::marshal(error)));
                    ControlError::Fault
                })?;
                staged.commit(&marshaller).map_err(|error| {
                    failure.set(Some(SocketErrno::marshal(error)));
                    ControlError::Fault
                })?;
                let record_oriented = socket
                    .snapshot
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .socket_type
                    != hl_network::SocketType::Stream;
                reported.set(if record_oriented && flags & MSG_TRUNC != 0 {
                    payload.len()
                } else {
                    count
                });
                Ok(())
            },
        );
        match received {
            Ok(Some(_)) => LinuxResult::Value(reported.get() as u64),
            Ok(None) if endpoint.readable_bytes() != 0 => {
                let available = endpoint.readable_bytes();
                let mut payload = vec![0; imported.vectors.total_length as usize];
                let count = match socket.read_with(&mut payload, true) {
                    Ok(count) => count,
                    Err(error) => return LinuxResult::Error(FileErrno::object(error)),
                };
                payload.truncate(count);
                let controls = if endpoint.passcred() {
                    match &socket.kind {
                        RuntimeSocketKind::Unix { pair, endpoint } => pair
                            .peer_credentials(*endpoint)
                            .map(|credentials| {
                                vec![hl_network::ControlMessage::Credentials {
                                    process: credentials.process,
                                    user: credentials.user,
                                    group: credentials.group,
                                }]
                            })
                            .unwrap_or_default(),
                        _ => Vec::new(),
                    }
                } else {
                    Vec::new()
                };
                let result = MessageCopyoutResult {
                    address: None,
                    data: payload,
                    controls,
                    flags: if count < available { MSG_TRUNC } else { 0 },
                };
                let staged = match abi.prepare_receive(imported, &result) {
                    Ok(value) => value,
                    Err(error) => return LinuxResult::Error(SocketErrno::marshal(error)),
                };
                match staged.commit(&marshaller) {
                    Ok(()) => LinuxResult::Value(count as u64),
                    Err(error) => LinuxResult::Error(SocketErrno::marshal(error)),
                }
            }
            Ok(None) if endpoint.message_closed() => {
                let result = MessageCopyoutResult {
                    address: None,
                    data: Vec::new(),
                    controls: Vec::new(),
                    flags: 0,
                };
                let staged = match abi.prepare_receive(imported, &result) {
                    Ok(value) => value,
                    Err(error) => return LinuxResult::Error(SocketErrno::marshal(error)),
                };
                match staged.commit(&marshaller) {
                    Ok(()) => LinuxResult::Value(0),
                    Err(error) => LinuxResult::Error(SocketErrno::marshal(error)),
                }
            }
            Ok(None) => LinuxResult::Error(Errno::EAGAIN),
            Err(error) => LinuxResult::Error(failure.get().unwrap_or_else(|| Self::unix_error(error))),
        }
    }

    fn unix_error(error: UnixTransportError) -> Errno {
        match error {
            UnixTransportError::Invalid => Errno::EINVAL,
            UnixTransportError::WouldBlock => Errno::EAGAIN,
            UnixTransportError::BrokenPipe => Errno::EPIPE,
            UnixTransportError::Canceled => Errno::EINTR,
            UnixTransportError::Control(error) => SocketErrno::marshal(NetworkMarshalError::Control(error)),
        }
    }
}
