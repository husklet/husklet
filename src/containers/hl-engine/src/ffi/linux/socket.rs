use super::{ErrnoMapper, LinuxHost};
use crate::native_host::{
    HostError, ShutdownDirection, SocketAddress, SocketDomain, SocketOption, SocketSyscalls, SocketType,
};
use core::ffi::c_void;

const AF_UNIX: i32 = 1;
const AF_INET: i32 = 2;
const SOCK_STREAM: i32 = 1;
const SOCK_DGRAM: i32 = 2;
const SOCK_NONBLOCK: i32 = 0x800;
const SOCK_CLOEXEC: i32 = 0x80000;
const SOL_SOCKET: i32 = 1;
const SO_ERROR: i32 = 4;
const SO_REUSEADDR: i32 = 2;
const SO_KEEPALIVE: i32 = 9;
const IPPROTO_TCP: i32 = 6;
const TCP_NODELAY: i32 = 1;

#[repr(C)]
struct SockAddr {
    family: u16,
    data: [u8; 14],
}

#[repr(C)]
struct SockAddrIn {
    family: u16,
    port: u16,
    address: u32,
    zero: [u8; 8],
}

#[repr(C)]
struct SockAddrUnix {
    family: u16,
    path: [u8; 108],
}

enum EncodedAddress {
    Inet(SockAddrIn),
    Unix(SockAddrUnix, u32),
}

impl EncodedAddress {
    fn new(address: &SocketAddress) -> Result<Self, HostError> {
        match address {
            SocketAddress::Ipv4Loopback(port) => Ok(Self::Inet(SockAddrIn {
                family: AF_INET as u16,
                port: port.to_be(),
                address: u32::from_ne_bytes([127, 0, 0, 1]),
                zero: [0; 8],
            })),
            SocketAddress::UnixPath(path) => {
                let mut native = SockAddrUnix {
                    family: AF_UNIX as u16,
                    path: [0; 108],
                };
                native.path[..path.len()].copy_from_slice(path);
                Ok(Self::Unix(native, (2 + path.len() + 1) as u32))
            }
            SocketAddress::UnixAbstract(name) => {
                let mut native = SockAddrUnix {
                    family: AF_UNIX as u16,
                    path: [0; 108],
                };
                native.path[1..=name.len()].copy_from_slice(name);
                Ok(Self::Unix(native, (2 + 1 + name.len()) as u32))
            }
        }
    }

    fn parts(&self) -> (*const SockAddr, u32) {
        match self {
            Self::Inet(value) => (
                std::ptr::from_ref::<SockAddrIn>(value).cast(),
                core::mem::size_of::<SockAddrIn>() as u32,
            ),
            Self::Unix(value, length) => (std::ptr::from_ref::<SockAddrUnix>(value).cast(), *length),
        }
    }
}

impl SocketSyscalls for LinuxHost {
    fn socket_create(&self, domain: SocketDomain, kind: SocketType) -> Result<i32, HostError> {
        let domain = match domain {
            SocketDomain::Ipv4 => AF_INET,
            SocketDomain::Unix => AF_UNIX,
        };
        let kind = match kind {
            SocketType::Stream => SOCK_STREAM,
            SocketType::Datagram => SOCK_DGRAM,
        };
        // SAFETY: scalar arguments only; success returns a new owned descriptor.
        let raw = unsafe { socket(domain, kind | SOCK_NONBLOCK | SOCK_CLOEXEC, 0) };
        (raw >= 0).then_some(raw).ok_or_else(ErrnoMapper::current)
    }

    fn socket_bind(&self, descriptor: i32, address: &SocketAddress) -> Result<(), HostError> {
        address_call(descriptor, address, bind)
    }

    fn socket_listen(&self, descriptor: i32, backlog: u32) -> Result<(), HostError> {
        // SAFETY: scalar arguments only.
        SocketCall::check(unsafe { listen(descriptor, backlog as i32) })
    }

    fn socket_connect(&self, descriptor: i32, address: &SocketAddress) -> Result<(), HostError> {
        address_call(descriptor, address, connect)
    }

    fn socket_accept(&self, descriptor: i32) -> Result<i32, HostError> {
        // SAFETY: null address requests no peer address; success owns a new descriptor.
        let raw = unsafe {
            accept4(
                descriptor,
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                SOCK_NONBLOCK | SOCK_CLOEXEC,
            )
        };
        (raw >= 0).then_some(raw).ok_or_else(ErrnoMapper::current)
    }

    fn socket_send(&self, descriptor: i32, input: &[u8]) -> Result<usize, HostError> {
        // SAFETY: input is immutably valid for its exact length; kernel retains nothing.
        let count = unsafe { send(descriptor, input.as_ptr().cast(), input.len(), 0x4000) };
        count.try_into().map_err(|_| ErrnoMapper::current())
    }

    fn socket_recv(&self, descriptor: i32, output: &mut [u8]) -> Result<usize, HostError> {
        // SAFETY: output is uniquely writable for its exact length; kernel retains nothing.
        let count = unsafe { recv(descriptor, output.as_mut_ptr().cast(), output.len(), 0) };
        count.try_into().map_err(|_| ErrnoMapper::current())
    }

    fn socket_shutdown(&self, descriptor: i32, direction: ShutdownDirection) -> Result<(), HostError> {
        let direction = match direction {
            ShutdownDirection::Read => 0,
            ShutdownDirection::Write => 1,
            ShutdownDirection::Both => 2,
        };
        // SAFETY: scalar arguments only.
        SocketCall::check(unsafe { shutdown(descriptor, direction) })
    }

    fn socket_set_option(&self, descriptor: i32, option: SocketOption, enabled: bool) -> Result<(), HostError> {
        let (level, option) = match option {
            SocketOption::ReuseAddress => (SOL_SOCKET, SO_REUSEADDR),
            SocketOption::KeepAlive => (SOL_SOCKET, SO_KEEPALIVE),
            SocketOption::NoDelay => (IPPROTO_TCP, TCP_NODELAY),
        };
        let value: i32 = enabled.into();
        // SAFETY: value is initialized and borrowed only for this call.
        SocketCall::check(unsafe {
            setsockopt(
                descriptor,
                level,
                option,
                (&raw const value).cast(),
                core::mem::size_of::<i32>() as u32,
            )
        })
    }

    fn socket_error(&self, descriptor: i32) -> Result<Option<HostError>, HostError> {
        let mut value = 0_i32;
        let mut length = core::mem::size_of::<i32>() as u32;
        // SAFETY: value and length are uniquely writable and correctly sized.
        SocketCall::check(unsafe {
            getsockopt(
                descriptor,
                SOL_SOCKET,
                SO_ERROR,
                (&raw mut value).cast(),
                &raw mut length,
            )
        })?;
        if length as usize != core::mem::size_of::<i32>() {
            return Err(HostError::Failed);
        }
        Ok((value != 0).then(|| ErrnoMapper::from_errno(value)))
    }

    fn socket_local_address(&self, descriptor: i32) -> Result<SocketAddress, HostError> {
        let mut value = SockAddrIn {
            family: 0,
            port: 0,
            address: 0,
            zero: [0; 8],
        };
        let mut length = core::mem::size_of::<SockAddrIn>() as u32;
        // SAFETY: value and length are uniquely writable and correctly bounded.
        SocketCall::check(unsafe { getsockname(descriptor, (&raw mut value).cast(), &raw mut length) })?;
        if value.family != AF_INET as u16 || length as usize != core::mem::size_of::<SockAddrIn>() {
            return Err(HostError::Unsupported);
        }
        Ok(SocketAddress::Ipv4Loopback(u16::from_be(value.port)))
    }
}

fn address_call(
    descriptor: i32,
    address: &SocketAddress,
    call: unsafe extern "C" fn(i32, *const SockAddr, u32) -> i32,
) -> Result<(), HostError> {
    let encoded = EncodedAddress::new(address)?;
    let (pointer, length) = encoded.parts();
    // SAFETY: pointer addresses the encoded value through the synchronous call.
    SocketCall::check(unsafe { call(descriptor, pointer, length) })
}

struct SocketCall;

impl SocketCall {
    fn check(value: i32) -> Result<(), HostError> {
        (value == 0).then_some(()).ok_or_else(ErrnoMapper::current)
    }
}

unsafe extern "C" {
    fn socket(domain: i32, kind: i32, protocol: i32) -> i32;
    fn bind(descriptor: i32, address: *const SockAddr, length: u32) -> i32;
    fn listen(descriptor: i32, backlog: i32) -> i32;
    fn connect(descriptor: i32, address: *const SockAddr, length: u32) -> i32;
    fn accept4(descriptor: i32, address: *mut SockAddr, length: *mut u32, flags: i32) -> i32;
    fn send(descriptor: i32, input: *const c_void, length: usize, flags: i32) -> isize;
    fn recv(descriptor: i32, output: *mut c_void, length: usize, flags: i32) -> isize;
    fn shutdown(descriptor: i32, direction: i32) -> i32;
    fn setsockopt(descriptor: i32, level: i32, option: i32, value: *const c_void, length: u32) -> i32;
    fn getsockopt(descriptor: i32, level: i32, option: i32, value: *mut c_void, length: *mut u32) -> i32;
    fn getsockname(descriptor: i32, address: *mut SockAddr, length: *mut u32) -> i32;
}
