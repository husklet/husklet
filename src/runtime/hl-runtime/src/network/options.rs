use hl_linux::{
    BpfInstruction, Errno, GuestAccess, GuestMarshaller, GuestMemory, GuestSocketOption, LinuxResult, NetworkAbi,
};
use hl_network::{SocketConnectError, SocketConnectStatus, SocketProtocol, SocketState, SocketType};

use crate::{
    RuntimeNetworkHost, RuntimeNetworkSyscalls, RuntimeSocket, RuntimeSocketKind, network::errno::SocketErrno,
};

const SOL_SOCKET: i32 = 1;
const SO_PASSCRED: i32 = 16;
const SO_PEERCRED: i32 = 17;
const SO_ATTACH_FILTER: i32 = 26;
const SOL_TCP: i32 = 6;
const TCP_INFO: i32 = 11;
const SOL_IPV6: i32 = 41;

impl<H: RuntimeNetworkHost, M: GuestMemory> RuntimeNetworkSyscalls<H, M> {
    pub(crate) fn setsockopt(
        &self,
        descriptor: i32,
        level: i32,
        option: i32,
        pointer: u64,
        length: u32,
    ) -> LinuxResult {
        if level == SOL_SOCKET && option == SO_ATTACH_FILTER {
            let socket = match self.lookup(descriptor) {
                Ok(value) => value,
                Err(error) => return LinuxResult::Error(error),
            };
            let value = match self.socket_filter(pointer, length) {
                Ok(value) => GuestSocketOption::Filter(value),
                Err(error) => return LinuxResult::Error(error),
            };
            if let RuntimeSocketKind::Host { token, .. } = &socket.kind {
                let Some(host) = &self.host else {
                    return LinuxResult::Error(Errno::ENOSYS);
                };
                if let Err(error) = host.set_option(*token, level, option, value.clone()) {
                    return LinuxResult::Error(SocketErrno::runtime(error));
                }
            }
            socket.set_option(level, option, value);
            return LinuxResult::Value(0);
        }
        let form = match Self::option_form(level, option, false) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let value = match NetworkAbi::new(&self.memory, self.architecture).decode_socket_option(pointer, length, form) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(SocketErrno::marshal(error)),
        };
        if let GuestSocketOption::Timeval { seconds, microseconds } = value {
            if seconds < 0 || !(0..1_000_000).contains(&microseconds) {
                return LinuxResult::Error(Errno::EINVAL);
            }
        }
        if matches!(value, GuestSocketOption::Bytes(_)) {
            return LinuxResult::Error(Errno::ENOPROTOOPT);
        }
        let passcred = match (level, option, &value) {
            (SOL_SOCKET, SO_PASSCRED, GuestSocketOption::Scalar(value)) => Some(*value != 0),
            _ => None,
        };
        let socket = match self.lookup(descriptor) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        if let RuntimeSocketKind::Host { token, .. } = &socket.kind {
            let Some(host) = &self.host else {
                return LinuxResult::Error(Errno::ENOSYS);
            };
            if let Err(error) = host.set_option(*token, level, option, value.clone()) {
                return LinuxResult::Error(SocketErrno::runtime(error));
            }
        }
        if let (RuntimeSocketKind::Unix { pair, endpoint }, Some(enabled)) = (&socket.kind, passcred) {
            pair.endpoints[*endpoint].set_passcred(enabled);
        }
        if let Some(enabled) = passcred {
            if let Some((pair, endpoint)) = socket.standalone_connection() {
                pair.endpoints[endpoint].set_passcred(enabled);
            }
        }
        socket.set_option(level, option, value);
        LinuxResult::Value(0)
    }

    fn socket_filter(&self, pointer: u64, length: u32) -> Result<Vec<BpfInstruction>, Errno> {
        if length < 16 {
            return Err(Errno::EINVAL);
        }
        let mut header = [0_u8; 16];
        if self.memory.read(pointer, &mut header) != Ok(header.len()) {
            return Err(Errno::EFAULT);
        }
        let count = usize::from(u16::from_le_bytes([header[0], header[1]]));
        if count == 0 || count > 4096 {
            return Err(Errno::EINVAL);
        }
        let address = u64::from_le_bytes(header[8..16].try_into().expect("fixed filter header"));
        if address == 0 {
            return Err(Errno::EFAULT);
        }
        let byte_count = count.checked_mul(8).ok_or(Errno::EINVAL)?;
        let mut bytes = vec![0_u8; byte_count];
        if self.memory.read(address, &mut bytes) != Ok(byte_count) {
            return Err(Errno::EFAULT);
        }
        Ok(bytes
            .chunks_exact(8)
            .map(|raw| BpfInstruction {
                code: u16::from_le_bytes([raw[0], raw[1]]),
                jump_true: raw[2],
                jump_false: raw[3],
                value: u32::from_le_bytes(raw[4..8].try_into().expect("fixed instruction")),
            })
            .collect())
    }

    pub(crate) fn getsockopt(
        &self,
        descriptor: i32,
        level: i32,
        option: i32,
        pointer: u64,
        length_pointer: u64,
    ) -> LinuxResult {
        let default = match Self::option_form(level, option, true) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let socket = match self.lookup(descriptor) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let mut delivered_connect_error = None;
        let value = match option {
            3 => GuestSocketOption::Scalar(Self::socket_type(&socket)),
            4 if level == SOL_SOCKET => {
                let RuntimeSocketKind::Host { .. } = &socket.kind else {
                    return self.copy_option(pointer, length_pointer, GuestSocketOption::Scalar(0));
                };
                match socket.connect_status() {
                    Err(_) => return LinuxResult::Error(Errno::EIO),
                    Ok(SocketConnectStatus::Failed(error)) => {
                        delivered_connect_error = Some(error);
                        GuestSocketOption::Scalar(Self::connect_errno(error).raw())
                    }
                    Ok(_) => GuestSocketOption::Scalar(0),
                }
            }
            SO_PASSCRED if level == SOL_SOCKET => match &socket.kind {
                RuntimeSocketKind::Unix { pair, endpoint } => {
                    GuestSocketOption::Scalar(i32::from(pair.endpoints[*endpoint].passcred()))
                }
                RuntimeSocketKind::Host { .. } => socket.option(level, option).unwrap_or(default),
                RuntimeSocketKind::UnixStandalone { .. } => socket.standalone_connection().map_or_else(
                    || socket.option(level, option).unwrap_or(default),
                    |(pair, endpoint)| GuestSocketOption::Scalar(i32::from(pair.endpoints[endpoint].passcred())),
                ),
            },
            SO_PEERCRED if level == SOL_SOCKET => match &socket.kind {
                RuntimeSocketKind::Unix { pair, endpoint } => {
                    let Some(credentials) = pair.peer_credentials(*endpoint) else {
                        return LinuxResult::Error(Errno::ENOTCONN);
                    };
                    GuestSocketOption::Credentials {
                        process: credentials.process,
                        user: credentials.user,
                        group: credentials.group,
                    }
                }
                RuntimeSocketKind::Host { token, .. } => {
                    let Some(host) = &self.host else {
                        return LinuxResult::Error(Errno::ENOSYS);
                    };
                    match host.get_option(*token, level, option) {
                        Ok(value) => value,
                        Err(error) => return LinuxResult::Error(SocketErrno::runtime(error)),
                    }
                }
                RuntimeSocketKind::UnixStandalone { .. } => {
                    let Some((pair, endpoint)) = socket.standalone_connection() else {
                        return LinuxResult::Error(Errno::ENOTCONN);
                    };
                    let Some(credentials) = pair.peer_credentials(endpoint) else {
                        return LinuxResult::Error(Errno::ENOTCONN);
                    };
                    GuestSocketOption::Credentials {
                        process: credentials.process,
                        user: credentials.user,
                        group: credentials.group,
                    }
                }
            },
            30 => GuestSocketOption::Scalar(i32::from(matches!(
                socket.snapshot.lock().unwrap_or_else(|error| error.into_inner()).state,
                SocketState::Listening { .. },
            ))),
            38 => GuestSocketOption::Scalar(Self::protocol(&socket)),
            39 => GuestSocketOption::Scalar(Self::domain(&socket)),
            TCP_INFO if level == SOL_TCP => match self.host_option(&socket, level, option) {
                Ok(Some(value)) => value,
                Ok(None) | Err(Errno::ENOPROTOOPT | Errno::EOPNOTSUPP | Errno::ENOSYS) => {
                    let state = socket.snapshot.lock().unwrap_or_else(|error| error.into_inner()).state;
                    let mut bytes = vec![0; 512];
                    bytes[0] = if matches!(state, SocketState::Connected { .. }) {
                        1
                    } else {
                        7
                    };
                    GuestSocketOption::Bytes(bytes)
                }
                Err(error) => return LinuxResult::Error(error),
            },
            _ if level == SOL_TCP => match socket.option(level, option) {
                Some(value) => value,
                None => match self.host_option(&socket, level, option) {
                    Ok(Some(value)) => value,
                    Ok(None) => default,
                    Err(error) => return LinuxResult::Error(error),
                },
            },
            _ => socket.option(level, option).unwrap_or(default),
        };
        let result = self.copy_option(pointer, length_pointer, value);
        Self::complete_error(&socket, delivered_connect_error, result)
    }

    fn complete_error(
        socket: &RuntimeSocket<H>,
        delivered: Option<SocketConnectError>,
        result: LinuxResult,
    ) -> LinuxResult {
        if result != LinuxResult::Value(0) {
            return result;
        }
        let Some(error) = delivered else { return result };
        let RuntimeSocketKind::Host { description, .. } = &socket.kind else {
            return result;
        };
        description.commit_connect_error(error);
        match socket.connect_status() {
            Ok(_) => result,
            Err(_) => LinuxResult::Error(Errno::EIO),
        }
    }

    fn host_option(
        &self,
        socket: &RuntimeSocket<H>,
        level: i32,
        option: i32,
    ) -> Result<Option<GuestSocketOption>, Errno> {
        let RuntimeSocketKind::Host { token, .. } = &socket.kind else {
            return Ok(None);
        };
        let Some(host) = &self.host else {
            return Err(Errno::ENOSYS);
        };
        host.get_option(*token, level, option)
            .map(Some)
            .map_err(SocketErrno::runtime)
    }

    fn copy_option(&self, pointer: u64, length_pointer: u64, value: GuestSocketOption) -> LinuxResult {
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        let capacity = match marshaller.socklen(length_pointer, 65_536) {
            Ok(value) => value as usize,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let bytes = NetworkAbi::<M>::encode_socket_option(value);
        let count = capacity.min(bytes.len());
        for (address, length) in [(pointer, count), (length_pointer, 4)] {
            match marshaller.probe(address, length, GuestAccess::Write) {
                Ok(available) if available == length => {}
                _ => return LinuxResult::Error(Errno::EFAULT),
            }
        }
        if marshaller.copy_to(pointer, &bytes[..count]).fault.is_some()
            || marshaller.write_socklen(length_pointer, count as u32).is_err()
        {
            return LinuxResult::Error(Errno::EFAULT);
        }
        LinuxResult::Value(0)
    }

    fn option_form(level: i32, option: i32, reading: bool) -> Result<GuestSocketOption, Errno> {
        if level == SOL_SOCKET && !reading && matches!(option, 3 | 4 | 30 | 38 | 39) {
            return Err(Errno::ENOPROTOOPT);
        }
        match (level, option) {
            (SOL_SOCKET, 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 15 | 16 | 27 | 30 | 38 | 39) => {
                Ok(GuestSocketOption::Scalar(0))
            }
            (SOL_SOCKET, SO_PEERCRED) if reading => Ok(GuestSocketOption::Credentials {
                process: 0,
                user: 0,
                group: 0,
            }),
            (SOL_SOCKET, 13) => Ok(GuestSocketOption::Linger { enabled: 0, seconds: 0 }),
            (SOL_SOCKET, 20 | 21) => Ok(GuestSocketOption::Timeval {
                seconds: 0,
                microseconds: 0,
            }),
            (0, 1 | 2 | 8 | 10 | 11 | 12 | 13 | 15)
            | (SOL_TCP, 1 | 2 | 3 | 4 | 5 | 6 | 12)
            | (SOL_IPV6, 16 | 26 | 49 | 51 | 66 | 67) => Ok(GuestSocketOption::Scalar(0)),
            (SOL_TCP, TCP_INFO) if reading => Ok(GuestSocketOption::Bytes(vec![0; 512])),
            _ => Err(Errno::ENOPROTOOPT),
        }
    }

    fn socket_type(socket: &crate::RuntimeSocket<H>) -> i32 {
        match socket
            .snapshot
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .socket_type
        {
            SocketType::Stream => 1,
            SocketType::Datagram => 2,
            SocketType::Raw => 3,
            SocketType::SequencePacket => 5,
        }
    }

    fn domain(socket: &crate::RuntimeSocket<H>) -> i32 {
        match socket.snapshot.lock().unwrap_or_else(|error| error.into_inner()).family {
            hl_network::AddressFamily::Unix => 1,
            hl_network::AddressFamily::Inet4 => 2,
            hl_network::AddressFamily::Inet6 => 10,
        }
    }

    fn protocol(socket: &crate::RuntimeSocket<H>) -> i32 {
        let snapshot = socket.snapshot.lock().unwrap_or_else(|error| error.into_inner());
        match (snapshot.protocol, snapshot.family, snapshot.socket_type) {
            (
                SocketProtocol::Default,
                hl_network::AddressFamily::Inet4 | hl_network::AddressFamily::Inet6,
                SocketType::Stream,
            ) => 6,
            (
                SocketProtocol::Default,
                hl_network::AddressFamily::Inet4 | hl_network::AddressFamily::Inet6,
                SocketType::Datagram,
            ) => 17,
            (SocketProtocol::Default, _, _) => 0,
            (SocketProtocol::Icmp, _, _) => 1,
            (SocketProtocol::Tcp, _, _) => 6,
            (SocketProtocol::Udp, _, _) => 17,
        }
    }
}
