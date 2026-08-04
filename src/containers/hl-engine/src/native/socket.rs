use super::{Descriptor, DescriptorSyscalls, EventSyscalls, HostError, PollSource};
use std::sync::Arc;

const UNIX_PATH_MAX: usize = 107;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SocketDomain {
    Ipv4,
    Unix,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SocketType {
    Stream,
    Datagram,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SocketAddress {
    Ipv4Loopback(u16),
    UnixPath(Vec<u8>),
    UnixAbstract(Vec<u8>),
}

impl SocketAddress {
    pub fn unix_path(path: &[u8]) -> Result<Self, HostError> {
        Self::unix(path, false)
    }

    pub fn unix_abstract(name: &[u8]) -> Result<Self, HostError> {
        Self::unix(name, true)
    }

    fn unix(value: &[u8], abstract_name: bool) -> Result<Self, HostError> {
        if value.is_empty() || value.len() > UNIX_PATH_MAX || (!abstract_name && value.contains(&0)) {
            return Err(HostError::Invalid);
        }
        Ok(if abstract_name {
            Self::UnixAbstract(value.to_vec())
        } else {
            Self::UnixPath(value.to_vec())
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShutdownDirection {
    Read,
    Write,
    Both,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SocketOption {
    ReuseAddress,
    KeepAlive,
    NoDelay,
}

pub trait SocketSyscalls: DescriptorSyscalls {
    fn socket_create(&self, domain: SocketDomain, kind: SocketType) -> Result<i32, HostError>;
    fn socket_bind(&self, descriptor: i32, address: &SocketAddress) -> Result<(), HostError>;
    fn socket_listen(&self, descriptor: i32, backlog: u32) -> Result<(), HostError>;
    fn socket_connect(&self, descriptor: i32, address: &SocketAddress) -> Result<(), HostError>;
    fn socket_accept(&self, descriptor: i32) -> Result<i32, HostError>;
    fn socket_send(&self, descriptor: i32, input: &[u8]) -> Result<usize, HostError>;
    fn socket_recv(&self, descriptor: i32, output: &mut [u8]) -> Result<usize, HostError>;
    fn socket_shutdown(&self, descriptor: i32, direction: ShutdownDirection) -> Result<(), HostError>;
    fn socket_set_option(&self, descriptor: i32, option: SocketOption, enabled: bool) -> Result<(), HostError>;
    fn socket_error(&self, descriptor: i32) -> Result<Option<HostError>, HostError>;
    fn socket_local_address(&self, descriptor: i32) -> Result<SocketAddress, HostError>;
}

pub struct Socket<S: SocketSyscalls> {
    descriptor: Descriptor<S>,
}

impl<S: SocketSyscalls> Socket<S> {
    pub fn create(syscalls: Arc<S>, domain: SocketDomain, kind: SocketType) -> Result<Self, HostError> {
        let raw = syscalls.socket_create(domain, kind)?;
        Ok(Self {
            descriptor: Descriptor::from_raw(syscalls, raw)?,
        })
    }

    pub fn bind(&self, address: &SocketAddress) -> Result<(), HostError> {
        self.syscalls().socket_bind(self.raw(), address)
    }

    pub fn listen(&self, backlog: u32) -> Result<(), HostError> {
        if backlog > i32::MAX as u32 {
            return Err(HostError::Invalid);
        }
        self.syscalls().socket_listen(self.raw(), backlog)
    }

    pub fn connect(&self, address: &SocketAddress) -> Result<(), HostError> {
        self.syscalls().socket_connect(self.raw(), address)
    }

    pub fn accept(&self) -> Result<Self, HostError> {
        let raw = self.syscalls().socket_accept(self.raw())?;
        Ok(Self {
            descriptor: Descriptor::from_raw(Arc::clone(&self.descriptor.syscalls), raw)?,
        })
    }

    pub fn send(&self, input: &[u8]) -> Result<usize, HostError> {
        self.syscalls().socket_send(self.raw(), input)
    }

    pub fn receive(&self, output: &mut [u8]) -> Result<usize, HostError> {
        self.syscalls().socket_recv(self.raw(), output)
    }

    pub fn shutdown(&self, direction: ShutdownDirection) -> Result<(), HostError> {
        self.syscalls().socket_shutdown(self.raw(), direction)
    }

    pub fn set_option(&self, option: SocketOption, enabled: bool) -> Result<(), HostError> {
        self.syscalls().socket_set_option(self.raw(), option, enabled)
    }

    pub fn pending_error(&self) -> Result<Option<HostError>, HostError> {
        self.syscalls().socket_error(self.raw())
    }

    pub fn local_address(&self) -> Result<SocketAddress, HostError> {
        self.syscalls().socket_local_address(self.raw())
    }

    fn raw(&self) -> i32 {
        self.descriptor.raw()
    }

    fn syscalls(&self) -> &S {
        self.descriptor.syscalls()
    }
}

impl<S: SocketSyscalls + EventSyscalls> PollSource<S> for Socket<S> {
    fn poll_descriptor(&self) -> &Descriptor<S> {
        &self.descriptor
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct FakeSockets {
        closed: Mutex<Vec<i32>>,
        send_result: Mutex<Result<usize, HostError>>,
        receive_result: Mutex<Result<usize, HostError>>,
    }

    impl DescriptorSyscalls for FakeSockets {
        fn duplicate_cloexec(&self, _: i32, _: i32) -> Result<i32, HostError> {
            Err(HostError::Unsupported)
        }

        fn close_descriptor(&self, descriptor: i32) {
            self.closed.lock().unwrap().push(descriptor);
        }
    }

    impl SocketSyscalls for FakeSockets {
        fn socket_create(&self, _: SocketDomain, _: SocketType) -> Result<i32, HostError> {
            Ok(41)
        }
        fn socket_bind(&self, _: i32, _: &SocketAddress) -> Result<(), HostError> {
            Ok(())
        }
        fn socket_listen(&self, _: i32, _: u32) -> Result<(), HostError> {
            Ok(())
        }
        fn socket_connect(&self, _: i32, _: &SocketAddress) -> Result<(), HostError> {
            Err(HostError::WouldBlock)
        }
        fn socket_accept(&self, _: i32) -> Result<i32, HostError> {
            Err(HostError::Interrupted)
        }
        fn socket_send(&self, _: i32, _: &[u8]) -> Result<usize, HostError> {
            *self.send_result.lock().unwrap()
        }
        fn socket_recv(&self, _: i32, _: &mut [u8]) -> Result<usize, HostError> {
            *self.receive_result.lock().unwrap()
        }
        fn socket_shutdown(&self, _: i32, _: ShutdownDirection) -> Result<(), HostError> {
            Ok(())
        }
        fn socket_set_option(&self, _: i32, _: SocketOption, _: bool) -> Result<(), HostError> {
            Ok(())
        }
        fn socket_error(&self, _: i32) -> Result<Option<HostError>, HostError> {
            Ok(None)
        }
        fn socket_local_address(&self, _: i32) -> Result<SocketAddress, HostError> {
            Ok(SocketAddress::Ipv4Loopback(1234))
        }
    }

    #[test]
    fn preserves_partial_and() {
        let host = Arc::new(FakeSockets {
            closed: Mutex::new(Vec::new()),
            send_result: Mutex::new(Ok(2)),
            receive_result: Mutex::new(Err(HostError::WouldBlock)),
        });
        let socket = Socket::create(Arc::clone(&host), SocketDomain::Ipv4, SocketType::Stream).unwrap();
        assert_eq!(socket.send(b"four").unwrap(), 2);
        assert_eq!(socket.receive(&mut [0; 4]), Err(HostError::WouldBlock));
        assert_eq!(
            socket.connect(&SocketAddress::Ipv4Loopback(1)),
            Err(HostError::WouldBlock)
        );
        assert_eq!(socket.accept().err(), Some(HostError::Interrupted));
        drop(socket);
        assert_eq!(*host.closed.lock().unwrap(), [41]);
    }

    #[test]
    fn validates_unix_names() {
        assert_eq!(SocketAddress::unix_path(b"a\0b"), Err(HostError::Invalid));
        assert_eq!(SocketAddress::unix_abstract(&[]), Err(HostError::Invalid));
        let host = Arc::new(FakeSockets {
            closed: Mutex::new(Vec::new()),
            send_result: Mutex::new(Ok(0)),
            receive_result: Mutex::new(Ok(0)),
        });
        let socket = Socket::create(host, SocketDomain::Unix, SocketType::Stream).unwrap();
        assert_eq!(socket.listen(i32::MAX as u32 + 1), Err(HostError::Invalid));
    }
}
