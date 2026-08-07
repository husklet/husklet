use hl_linux::{Errno, GuestMarshaller, GuestMemory, GuestNetworkAddress, LinuxResult, NetworkAbi};
use hl_network::{SocketAddress, SocketType, UnixAddress};

use crate::{RuntimeNetworkHost, RuntimeNetworkSyscalls, RuntimeSocketKind};

use super::errno::SocketErrno;

impl<H: RuntimeNetworkHost, M: GuestMemory> RuntimeNetworkSyscalls<H, M> {
    pub(crate) fn address(&self, descriptor: i32, pointer: u64, length: u64, peer: bool) -> LinuxResult {
        let socket = match self.lookup(descriptor) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        if let Some(netlink) = socket.netlink_socket() {
            if peer {
                return LinuxResult::Error(Errno::ENOTCONN);
            }
            let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
            let mut capacity = [0_u8; 4];
            if marshaller.copy_from(length, &mut capacity).fault.is_some() {
                return LinuxResult::Error(Errno::EFAULT);
            }
            let capacity = u32::from_le_bytes(capacity) as usize;
            let mut address = [0_u8; 12];
            address[..2].copy_from_slice(&16_u16.to_le_bytes());
            address[4..8].copy_from_slice(&netlink.port().to_le_bytes());
            if marshaller
                .copy_to(pointer, &address[..capacity.min(12)])
                .fault
                .is_some()
                || marshaller.copy_to(length, &12_u32.to_le_bytes()).fault.is_some()
            {
                return LinuxResult::Error(Errno::EFAULT);
            }
            return LinuxResult::Value(0);
        }
        let address = match &socket.kind {
            RuntimeSocketKind::Host { token, .. } => {
                let snapshot = socket
                    .snapshot
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if peer && snapshot.socket_type == SocketType::Datagram {
                    snapshot
                        .peer
                        .clone()
                        .map(GuestNetworkAddress::Inet)
                        .ok_or(crate::RuntimeNetworkError::NotConnected)
                } else {
                    drop(snapshot);
                    let Some(host) = &self.host else {
                        return LinuxResult::Error(Errno::ENOSYS);
                    };
                    if peer {
                        host.peer_address(*token)
                    } else {
                        host.local_address(*token)
                    }
                    .map(GuestNetworkAddress::Inet)
                }
            }
            RuntimeSocketKind::Unix { pair, endpoint } => {
                Ok(GuestNetworkAddress::Unix(pair.endpoints[*endpoint].address().clone()))
            }
            RuntimeSocketKind::UnixStandalone { .. } => {
                let snapshot = socket
                    .snapshot
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let address = if peer {
                    snapshot.peer.as_ref()
                } else {
                    snapshot.local.as_ref()
                };
                match address {
                    Some(SocketAddress::Unix(value)) => Ok(Self::guest_address(&SocketAddress::Unix(value.clone()))),
                    _ if peer => return LinuxResult::Error(Errno::ENOTCONN),
                    _ => Ok(GuestNetworkAddress::Unix(UnixAddress::Unnamed)),
                }
            }
        };
        let address = match address {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(SocketErrno::runtime(error)),
        };
        let abi = NetworkAbi::new(&self.memory, self.architecture);
        let staged = match abi.prepare_sockaddr_copyout(pointer, length, &address) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(SocketErrno::marshal(error)),
        };
        match staged.commit(&GuestMarshaller::new(&self.memory, self.architecture)) {
            Ok(()) => LinuxResult::Value(0),
            Err(error) => LinuxResult::Error(SocketErrno::marshal(error)),
        }
    }

    pub(crate) fn shutdown(&self, descriptor: i32, how: i32) -> LinuxResult {
        let (read, write) = match how {
            0 => (true, false),
            1 => (false, true),
            2 => (true, true),
            _ => return LinuxResult::Error(Errno::EINVAL),
        };
        let socket = match self.lookup(descriptor) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let result = match &socket.kind {
            RuntimeSocketKind::Host { token, .. } => self
                .host
                .as_ref()
                .ok_or(crate::RuntimeNetworkError::Unsupported)
                .and_then(|host| host.shutdown(*token, read, write)),
            RuntimeSocketKind::Unix { pair, endpoint } => {
                pair.endpoints[*endpoint].shutdown(read, write);
                Ok(())
            }
            RuntimeSocketKind::UnixStandalone { .. } => match socket.standalone_connection() {
                Some((pair, endpoint)) => {
                    pair.endpoints[endpoint].shutdown(read, write);
                    Ok(())
                }
                None => Err(crate::RuntimeNetworkError::NotConnected),
            },
        };
        match result {
            Ok(()) => {
                let mut snapshot = socket
                    .snapshot
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                snapshot.shutdown.read |= read;
                snapshot.shutdown.write |= write;
                if self
                    .current_catalog()
                    .replace_snapshot(socket.id, snapshot.clone())
                    .is_err()
                {
                    return LinuxResult::Error(Errno::EIO);
                }
                LinuxResult::Value(0)
            }
            Err(error) => LinuxResult::Error(SocketErrno::runtime(error)),
        }
    }
}
