//! Socket address encoding and decoding for the native network host.

#![allow(unsafe_code)]

use std::mem::{size_of, zeroed};

use hl_network::SocketAddress;
use hl_runtime::RuntimeNetworkError;

use super::Native;

impl Native {
    pub(super) fn socket_address(
        address: &SocketAddress,
    ) -> Result<(libc::sockaddr_storage, u32), RuntimeNetworkError> {
        // SAFETY: zero is a valid initialization for sockaddr storage and concrete sockaddr values.
        let mut storage = unsafe { zeroed::<libc::sockaddr_storage>() };
        match address {
            SocketAddress::Inet4 { address, port } => {
                // SAFETY: storage is aligned and large enough for sockaddr_in.
                let value = unsafe { &mut *(&raw mut storage).cast::<libc::sockaddr_in>() };
                value.sin_family = libc::AF_INET as _;
                value.sin_port = port.to_be();
                value.sin_addr.s_addr = u32::from_ne_bytes(*address);
                Ok((storage, size_of::<libc::sockaddr_in>() as u32))
            }
            SocketAddress::Inet6 { address, port, scope } => {
                // SAFETY: storage is aligned and large enough for sockaddr_in6.
                let value = unsafe { &mut *(&raw mut storage).cast::<libc::sockaddr_in6>() };
                value.sin6_family = libc::AF_INET6 as _;
                value.sin6_port = port.to_be();
                value.sin6_scope_id = *scope;
                value.sin6_addr.s6_addr = *address;
                Ok((storage, size_of::<libc::sockaddr_in6>() as u32))
            }
            SocketAddress::Unix(path) => {
                if path.len() > size_of::<libc::sockaddr_un>() - std::mem::offset_of!(libc::sockaddr_un, sun_path) {
                    return Err(RuntimeNetworkError::Invalid);
                }
                // SAFETY: storage is aligned and large enough for sockaddr_un.
                let value = unsafe { &mut *(&raw mut storage).cast::<libc::sockaddr_un>() };
                value.sun_family = libc::AF_UNIX as _;
                for (target, source) in value.sun_path.iter_mut().zip(path) {
                    *target = *source as libc::c_char;
                }
                let length = std::mem::offset_of!(libc::sockaddr_un, sun_path) + path.len();
                Ok((storage, length as u32))
            }
        }
    }

    pub(in crate::ffi::linux::network) fn decode_address(
        storage: &libc::sockaddr_storage,
        length: u32,
    ) -> Result<SocketAddress, RuntimeNetworkError> {
        match i32::from(storage.ss_family) {
            libc::AF_INET => {
                // SAFETY: family identifies initialized sockaddr_in storage.
                let value = unsafe { &*std::ptr::from_ref(storage).cast::<libc::sockaddr_in>() };
                Ok(SocketAddress::Inet4 {
                    address: value.sin_addr.s_addr.to_ne_bytes(),
                    port: u16::from_be(value.sin_port),
                })
            }
            libc::AF_INET6 => {
                // SAFETY: family identifies initialized sockaddr_in6 storage.
                let value = unsafe { &*std::ptr::from_ref(storage).cast::<libc::sockaddr_in6>() };
                Ok(SocketAddress::Inet6 {
                    address: value.sin6_addr.s6_addr,
                    port: u16::from_be(value.sin6_port),
                    scope: value.sin6_scope_id,
                })
            }
            libc::AF_UNIX => {
                // SAFETY: family identifies initialized sockaddr_un storage.
                let value = unsafe { &*std::ptr::from_ref(storage).cast::<libc::sockaddr_un>() };
                let available = (length as usize)
                    .saturating_sub(std::mem::offset_of!(libc::sockaddr_un, sun_path))
                    .min(value.sun_path.len());
                let mut bytes: Vec<u8> = value.sun_path[..available]
                    .iter()
                    .map(|byte| u8::from_ne_bytes(byte.to_ne_bytes()))
                    .collect();
                if bytes.first() != Some(&0) {
                    Self::trim_unix(&mut bytes);
                }
                Ok(SocketAddress::Unix(bytes))
            }
            _ => Err(RuntimeNetworkError::Invalid),
        }
    }

    pub(super) fn trim_unix(bytes: &mut Vec<u8>) {
        let length = bytes.iter().position(|byte| *byte == 0).unwrap_or(bytes.len());
        bytes.truncate(length);
    }

    pub(super) fn address_of(&self, token: u64, peer: bool) -> Result<SocketAddress, RuntimeNetworkError> {
        {
            let sockets = self
                .shared
                .sockets
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let projected = sockets.get(&token).and_then(|entry| {
                if peer {
                    entry.guest_peer.clone()
                } else {
                    entry.guest_local.clone()
                }
            });
            if let Some(address) = projected {
                return Ok(address);
            }
        }
        let descriptor = self.descriptor(token)?;
        // SAFETY: zero is a valid sockaddr_storage initialization.
        let mut storage = unsafe { zeroed::<libc::sockaddr_storage>() };
        let mut length = size_of::<libc::sockaddr_storage>() as libc::socklen_t;
        // SAFETY: pointers reference writable storage of the supplied length for the duration of the call.
        let result = unsafe {
            if peer {
                libc::getpeername(descriptor, (&raw mut storage).cast(), &raw mut length)
            } else {
                libc::getsockname(descriptor, (&raw mut storage).cast(), &raw mut length)
            }
        };
        if result == 0 {
            Self::decode_address(&storage, length)
        } else {
            Err(Self::runtime_error())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Native;
    use hl_network::SocketAddress;

    #[test]
    fn unix_addresses_round_trip_across_host_c_char_signedness() {
        for address in [
            SocketAddress::Unix(b"/tmp/husklet.sock".to_vec()),
            SocketAddress::Unix(vec![0, 0x80, 0xff]),
        ] {
            let (storage, length) = Native::socket_address(&address).unwrap();
            assert_eq!(Native::decode_address(&storage, length), Ok(address));
        }
    }
}
