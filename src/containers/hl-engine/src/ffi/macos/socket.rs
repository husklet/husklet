use super::super::macos_plan::DarwinPlan;
use super::{DarwinHost, last_error};
use crate::native_host::{
    HostError, ShutdownDirection, SocketAddress, SocketDomain, SocketOption, SocketSyscalls, SocketType,
};
use std::mem::size_of;

impl SocketSyscalls for DarwinHost {
    fn socket_create(&self, domain: SocketDomain, kind: SocketType) -> Result<i32, HostError> {
        let domain = match domain {
            SocketDomain::Ipv4 => libc::AF_INET,
            SocketDomain::Unix => libc::AF_UNIX,
        };
        let kind = match kind {
            SocketType::Stream => libc::SOCK_STREAM,
            SocketType::Datagram => libc::SOCK_DGRAM,
        };
        // SAFETY: scalar arguments only; success returns an owned descriptor.
        let descriptor = unsafe { libc::socket(domain, kind, 0) };
        if descriptor < 0 {
            return Err(last_error());
        }
        if let Err(error) = SocketCall::configure(descriptor) {
            // SAFETY: descriptor has not escaped and is rolled back once.
            let _ = unsafe { libc::close(descriptor) };
            return Err(error);
        }
        Ok(descriptor)
    }

    fn socket_bind(&self, descriptor: i32, address: &SocketAddress) -> Result<(), HostError> {
        let address = EncodedAddress::new(address)?;
        let (pointer, length) = address.parts();
        // SAFETY: pointer refers to address through this synchronous call.
        SocketCall::check(unsafe { libc::bind(descriptor, pointer, length) })
    }

    fn socket_listen(&self, descriptor: i32, backlog: u32) -> Result<(), HostError> {
        // SAFETY: scalar arguments only.
        SocketCall::check(unsafe { libc::listen(descriptor, backlog as i32) })
    }

    fn socket_connect(&self, descriptor: i32, address: &SocketAddress) -> Result<(), HostError> {
        let address = EncodedAddress::new(address)?;
        let (pointer, length) = address.parts();
        // SAFETY: pointer refers to address through this synchronous call.
        SocketCall::check(unsafe { libc::connect(descriptor, pointer, length) })
    }

    fn socket_accept(&self, descriptor: i32) -> Result<i32, HostError> {
        // SAFETY: null pointers request no peer address; success owns a new fd.
        let accepted = unsafe { libc::accept(descriptor, std::ptr::null_mut(), std::ptr::null_mut()) };
        if accepted < 0 {
            return Err(last_error());
        }
        if let Err(error) = SocketCall::configure(accepted) {
            // SAFETY: accepted has not escaped and is rolled back once.
            let _ = unsafe { libc::close(accepted) };
            return Err(error);
        }
        Ok(accepted)
    }

    fn socket_send(&self, descriptor: i32, input: &[u8]) -> Result<usize, HostError> {
        // SAFETY: input is readable for its exact length and is not retained.
        let count = unsafe { libc::send(descriptor, input.as_ptr().cast(), input.len(), 0) };
        count.try_into().map_err(|_| last_error())
    }

    fn socket_recv(&self, descriptor: i32, output: &mut [u8]) -> Result<usize, HostError> {
        // SAFETY: output is uniquely writable for its exact length and not retained.
        let count = unsafe { libc::recv(descriptor, output.as_mut_ptr().cast(), output.len(), 0) };
        count.try_into().map_err(|_| last_error())
    }

    fn socket_shutdown(&self, descriptor: i32, direction: ShutdownDirection) -> Result<(), HostError> {
        let direction = match direction {
            ShutdownDirection::Read => libc::SHUT_RD,
            ShutdownDirection::Write => libc::SHUT_WR,
            ShutdownDirection::Both => libc::SHUT_RDWR,
        };
        // SAFETY: scalar arguments only.
        SocketCall::check(unsafe { libc::shutdown(descriptor, direction) })
    }

    fn socket_set_option(&self, descriptor: i32, option: SocketOption, enabled: bool) -> Result<(), HostError> {
        let (level, name) = match option {
            SocketOption::ReuseAddress => (libc::SOL_SOCKET, libc::SO_REUSEADDR),
            SocketOption::KeepAlive => (libc::SOL_SOCKET, libc::SO_KEEPALIVE),
            SocketOption::NoDelay => (libc::IPPROTO_TCP, libc::TCP_NODELAY),
        };
        let value: i32 = enabled.into();
        // SAFETY: value is initialized, correctly sized, and not retained.
        SocketCall::check(unsafe {
            libc::setsockopt(
                descriptor,
                level,
                name,
                (&value as *const i32).cast(),
                size_of::<i32>() as libc::socklen_t,
            )
        })
    }

    fn socket_error(&self, descriptor: i32) -> Result<Option<HostError>, HostError> {
        let mut value = 0;
        let mut length = size_of::<i32>() as libc::socklen_t;
        // SAFETY: value and length are uniquely writable and correctly sized.
        SocketCall::check(unsafe {
            libc::getsockopt(
                descriptor,
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                (&mut value as *mut i32).cast(),
                &mut length,
            )
        })?;
        if length as usize != size_of::<i32>() {
            return Err(HostError::Failed);
        }
        Ok((value != 0).then(|| SocketCall::errno(value)))
    }

    fn socket_local_address(&self, descriptor: i32) -> Result<SocketAddress, HostError> {
        // SAFETY: zero is valid initialization for sockaddr_in.
        let mut address: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        let mut length = size_of::<libc::sockaddr_in>() as libc::socklen_t;
        // SAFETY: address and length are uniquely writable and bounded.
        SocketCall::check(unsafe {
            libc::getsockname(descriptor, (&mut address as *mut libc::sockaddr_in).cast(), &mut length)
        })?;
        if address.sin_family != libc::AF_INET as u8 || length as usize != size_of::<libc::sockaddr_in>() {
            return Err(HostError::Unsupported);
        }
        Ok(SocketAddress::Ipv4Loopback(u16::from_be(address.sin_port)))
    }
}

struct SocketCall;

impl SocketCall {
    fn configure(descriptor: i32) -> Result<(), HostError> {
        // SAFETY: fcntl receives scalar arguments for an owned descriptor.
        let status = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
        let no_sigpipe = 1_i32;
        // SAFETY: the option value is initialized, correctly sized, and not retained.
        let signal_result = unsafe {
            libc::setsockopt(
                descriptor,
                libc::SOL_SOCKET,
                libc::SO_NOSIGPIPE,
                (&no_sigpipe as *const i32).cast(),
                size_of::<i32>() as libc::socklen_t,
            )
        };
        // SAFETY: fcntl receives scalar flags for the owned descriptor.
        let nonblocking_result = unsafe { libc::fcntl(descriptor, libc::F_SETFL, status | libc::O_NONBLOCK) };
        // SAFETY: fcntl receives scalar flags for the owned descriptor.
        let cloexec_result = unsafe { libc::fcntl(descriptor, libc::F_SETFD, libc::FD_CLOEXEC) };
        if status < 0 || nonblocking_result < 0 || signal_result < 0 || cloexec_result < 0 {
            Err(last_error())
        } else {
            Ok(())
        }
    }

    fn check(result: i32) -> Result<(), HostError> {
        (result == 0).then_some(()).ok_or_else(last_error)
    }

    fn errno(value: i32) -> HostError {
        match value {
            libc::EINTR => HostError::Interrupted,
            libc::EAGAIN | libc::EINPROGRESS | libc::EALREADY => HostError::WouldBlock,
            libc::EINVAL => HostError::Invalid,
            libc::EACCES | libc::EPERM => HostError::Denied,
            libc::ENOENT => HostError::NotFound,
            libc::EEXIST => HostError::Exists,
            libc::ENOTSUP | libc::EAFNOSUPPORT => HostError::Unsupported,
            _ => HostError::Failed,
        }
    }
}

enum EncodedAddress {
    Inet(libc::sockaddr_in),
    Unix(libc::sockaddr_un, libc::socklen_t),
}

impl EncodedAddress {
    fn new(address: &SocketAddress) -> Result<Self, HostError> {
        match address {
            SocketAddress::Ipv4Loopback(port) => {
                // SAFETY: zero is valid initialization for sockaddr_in.
                let mut value: libc::sockaddr_in = unsafe { std::mem::zeroed() };
                value.sin_len = size_of::<libc::sockaddr_in>() as u8;
                value.sin_family = libc::AF_INET as u8;
                value.sin_port = port.to_be();
                value.sin_addr.s_addr = u32::from_ne_bytes([127, 0, 0, 1]);
                Ok(Self::Inet(value))
            }
            SocketAddress::UnixPath(path) => {
                // SAFETY: zero is valid initialization for sockaddr_un.
                let mut value: libc::sockaddr_un = unsafe { std::mem::zeroed() };
                let length = DarwinPlan::unix_path_length(path.len())?;
                value.sun_len = length;
                value.sun_family = libc::AF_UNIX as u8;
                for (output, input) in value.sun_path.iter_mut().zip(path) {
                    *output = *input as i8;
                }
                Ok(Self::Unix(value, libc::socklen_t::from(length)))
            }
            SocketAddress::UnixAbstract(_) => Err(HostError::Unsupported),
        }
    }

    fn parts(&self) -> (*const libc::sockaddr, libc::socklen_t) {
        match self {
            Self::Inet(value) => (
                (value as *const libc::sockaddr_in).cast(),
                size_of::<libc::sockaddr_in>() as libc::socklen_t,
            ),
            Self::Unix(value, length) => ((value as *const libc::sockaddr_un).cast(), *length),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn darwin_sockaddr_layouts() {
        assert_eq!(std::mem::size_of::<libc::sockaddr_in>(), 16);
        assert_eq!(std::mem::size_of::<libc::sockaddr_un>(), 106);
    }
}
