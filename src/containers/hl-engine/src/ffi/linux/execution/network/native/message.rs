use std::mem::{size_of, zeroed};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use hl_network::{BindRoute, SocketAddress};
use hl_runtime::{HostControl, HostReceive, HostSend, HostSendResult, RuntimeNetworkError, RuntimeNetworkHost};

use super::Native;

const CONTROL_MAXIMUM: usize = 65_536;
const RIGHTS_MAXIMUM: usize = 253;

struct ControlBuffer {
    words: Vec<usize>,
    length: usize,
}

impl ControlBuffer {
    fn new(length: usize) -> Result<Self, RuntimeNetworkError> {
        let words = length
            .checked_add(size_of::<usize>() - 1)
            .ok_or(RuntimeNetworkError::NoMemory)?
            / size_of::<usize>();
        let mut storage = Vec::new();
        storage
            .try_reserve_exact(words)
            .map_err(|_| RuntimeNetworkError::NoMemory)?;
        storage.resize(words, 0);
        Ok(Self { words: storage, length })
    }

    fn pointer(&mut self) -> *mut libc::c_void {
        self.words.as_mut_ptr().cast()
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: the byte view is bounded by initialized word storage.
        unsafe { std::slice::from_raw_parts(self.words.as_ptr().cast(), self.length) }
    }
}

impl Native {
    pub(super) fn send_control(
        &self,
        token: u64,
        message: HostSend<OwnedFd>,
    ) -> Result<HostSendResult, RuntimeNetworkError> {
        let descriptor = self.descriptor(token)?;
        let rights = Self::collect_rights(&message.controls)?;
        let mut control = Self::encode_rights(&rights)?;
        let mut vector = libc::iovec {
            iov_base: message.payload.as_ptr().cast_mut().cast(),
            iov_len: message.payload.len(),
        };
        // SAFETY: zero is a valid msghdr initialization.
        let mut header = unsafe { zeroed::<libc::msghdr>() };
        header.msg_iov = &mut vector;
        header.msg_iovlen = 1;
        if let Some(route) = message.route {
            let address = if let Some(interface) = route.interface {
                let SocketAddress::Inet4 { address, port } = route.address else {
                    return Err(RuntimeNetworkError::Invalid);
                };
                if port == 0 || self.socket_type(token)? != libc::SOCK_DGRAM {
                    return Err(RuntimeNetworkError::Invalid);
                }
                let needs_source = self
                    .shared
                    .sockets
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .get(&token)
                    .is_none_or(|entry| entry.guest_local.is_none());
                if needs_source {
                    <Self as RuntimeNetworkHost>::bind_route(
                        self,
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
                let (_, path) = Self::switch_path(&interface, address, port)?;
                SocketAddress::Unix(path)
            } else {
                route.address
            };
            let (storage, length) = Self::socket_address(&address)?;
            header.msg_name = std::ptr::from_ref(&storage).cast_mut().cast();
            header.msg_namelen = length;
            return self.send_header(
                descriptor,
                &mut header,
                control.as_mut(),
                message.nonblocking,
                !rights.is_empty(),
                message.record,
            );
        }
        self.send_header(
            descriptor,
            &mut header,
            control.as_mut(),
            message.nonblocking,
            !rights.is_empty(),
            message.record,
        )
    }

    fn collect_rights(controls: &[HostControl<OwnedFd>]) -> Result<Vec<i32>, RuntimeNetworkError> {
        let mut rights = Vec::new();
        for control in controls {
            Self::append_rights(&mut rights, control)?;
        }
        Ok(rights)
    }

    fn append_rights(rights: &mut Vec<i32>, control: &HostControl<OwnedFd>) -> Result<(), RuntimeNetworkError> {
        let HostControl::Rights(values) = control else {
            return Err(RuntimeNetworkError::Unsupported);
        };
        if rights.len().saturating_add(values.len()) > RIGHTS_MAXIMUM {
            return Err(RuntimeNetworkError::Invalid);
        }
        rights.extend(values.iter().map(AsRawFd::as_raw_fd));
        Ok(())
    }

    fn send_header(
        &self,
        descriptor: i32,
        header: &mut libc::msghdr,
        control: Option<&mut ControlBuffer>,
        nonblocking: bool,
        has_rights: bool,
        record: bool,
    ) -> Result<HostSendResult, RuntimeNetworkError> {
        if let Some(control) = control {
            header.msg_control = control.pointer();
            header.msg_controllen = control.length;
        }
        let flags = if nonblocking { libc::MSG_DONTWAIT } else { 0 };
        // SAFETY: header points to live payload and aligned ancillary storage for one non-retaining call.
        let result = unsafe { libc::sendmsg(descriptor, header, flags) };
        if result < 0 {
            return Err(Self::runtime_error());
        }
        Ok(HostSendResult {
            count: result as usize,
            rights_consumed: has_rights && (result > 0 || record),
        })
    }

    pub(super) fn receive_control(
        &self,
        token: u64,
        payload_limit: usize,
        control_limit: usize,
        nonblocking: bool,
        peek: bool,
    ) -> Result<HostReceive<OwnedFd>, RuntimeNetworkError> {
        let descriptor = self.descriptor(token)?;
        let mut payload = Vec::new();
        payload
            .try_reserve_exact(payload_limit)
            .map_err(|_| RuntimeNetworkError::NoMemory)?;
        payload.resize(payload_limit, 0);
        let mut control = ControlBuffer::new(control_limit.min(CONTROL_MAXIMUM))?;
        let mut vector = libc::iovec {
            iov_base: payload.as_mut_ptr().cast(),
            iov_len: payload.len(),
        };
        // SAFETY: zero is valid initialization for sockaddr and msghdr storage.
        let mut source = unsafe { zeroed::<libc::sockaddr_storage>() };
        let mut header = unsafe { zeroed::<libc::msghdr>() };
        header.msg_name = std::ptr::from_mut(&mut source).cast();
        header.msg_namelen = size_of::<libc::sockaddr_storage>() as libc::socklen_t;
        header.msg_iov = &mut vector;
        header.msg_iovlen = 1;
        header.msg_control = control.pointer();
        header.msg_controllen = control.length;
        let flags = (if nonblocking { libc::MSG_DONTWAIT } else { 0 })
            | (if peek { libc::MSG_PEEK } else { 0 })
            | libc::MSG_TRUNC;
        // SAFETY: header contains writable payload, address, and aligned control storage for one call.
        let result = unsafe { libc::recvmsg(descriptor, &mut header, flags) };
        self.arm_read(token);
        if result < 0 {
            return Err(Self::runtime_error());
        }
        let count = (result as usize).min(payload.len());
        payload.truncate(count);
        control.length = header.msg_controllen.min(control.length);
        let controls = Self::decode_rights(&control)?;
        let source = if header.msg_namelen == 0 {
            None
        } else {
            let source = Self::decode_address(&source, header.msg_namelen)?;
            Some(match source {
                SocketAddress::Unix(path) => Self::switch_source(&path).ok_or(RuntimeNetworkError::Invalid)?,
                source => source,
            })
        };
        Ok(HostReceive {
            payload,
            full_length: result as usize,
            source,
            controls,
            payload_truncated: header.msg_flags & libc::MSG_TRUNC != 0,
            control_truncated: header.msg_flags & libc::MSG_CTRUNC != 0,
        })
    }

    fn encode_rights(rights: &[i32]) -> Result<Option<ControlBuffer>, RuntimeNetworkError> {
        if rights.is_empty() {
            return Ok(None);
        }
        let data = rights
            .len()
            .checked_mul(size_of::<i32>())
            .ok_or(RuntimeNetworkError::Invalid)?;
        let header = Self::align(size_of::<libc::cmsghdr>())?;
        let length = header.checked_add(data).ok_or(RuntimeNetworkError::Invalid)?;
        let mut control = ControlBuffer::new(Self::align(length)?)?;
        let message = control.pointer().cast::<libc::cmsghdr>();
        // SAFETY: control is aligned and large enough for cmsghdr plus every descriptor integer.
        unsafe {
            (*message).cmsg_len = length as _;
            (*message).cmsg_level = libc::SOL_SOCKET;
            (*message).cmsg_type = libc::SCM_RIGHTS;
            std::ptr::copy_nonoverlapping(
                rights.as_ptr(),
                control.pointer().cast::<u8>().add(header).cast(),
                rights.len(),
            );
        }
        Ok(Some(control))
    }

    fn decode_rights(control: &ControlBuffer) -> Result<Vec<HostControl<OwnedFd>>, RuntimeNetworkError> {
        let bytes = control.bytes();
        let header = Self::align(size_of::<libc::cmsghdr>())?;
        let mut offset = 0;
        let mut rights = Vec::new();
        while bytes.len().saturating_sub(offset) >= size_of::<libc::cmsghdr>() {
            // SAFETY: control storage is word-aligned and offset advances by word alignment.
            let message = unsafe { &*bytes.as_ptr().add(offset).cast::<libc::cmsghdr>() };
            let length = message.cmsg_len as usize;
            if length < header || length > bytes.len() - offset {
                return Err(RuntimeNetworkError::Invalid);
            }
            if message.cmsg_level == libc::SOL_SOCKET && message.cmsg_type == libc::SCM_RIGHTS {
                Self::read_rights(bytes, offset + header, length - header, &mut rights)?;
            }
            offset = Self::align(offset.checked_add(length).ok_or(RuntimeNetworkError::Invalid)?)?;
        }
        Ok(if rights.is_empty() {
            Vec::new()
        } else {
            vec![HostControl::Rights(rights)]
        })
    }

    fn read_rights(
        bytes: &[u8],
        start: usize,
        length: usize,
        rights: &mut Vec<OwnedFd>,
    ) -> Result<(), RuntimeNetworkError> {
        let count = length / size_of::<i32>();
        if rights.len().saturating_add(count) > RIGHTS_MAXIMUM {
            return Err(RuntimeNetworkError::Invalid);
        }
        for index in 0..count {
            // SAFETY: each integer lies inside the validated cmsg payload.
            let descriptor =
                unsafe { std::ptr::read_unaligned(bytes.as_ptr().add(start + index * size_of::<i32>()).cast::<i32>()) };
            if descriptor < 0 {
                return Err(RuntimeNetworkError::Invalid);
            }
            // SAFETY: SCM_RIGHTS returns a newly owned descriptor exactly once.
            let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
            // SAFETY: F_SETFD mutates only this newly owned descriptor.
            if unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
                return Err(Self::runtime_error());
            }
            rights.push(descriptor);
        }
        Ok(())
    }

    fn align(value: usize) -> Result<usize, RuntimeNetworkError> {
        value
            .checked_add(size_of::<usize>() - 1)
            .map(|value| value & !(size_of::<usize>() - 1))
            .ok_or(RuntimeNetworkError::Invalid)
    }
}

#[cfg(test)]
mod test {
    use std::os::fd::OwnedFd;

    use hl_network::SocketHostIo;
    use hl_runtime::{HostControl, HostSend};

    use super::Native;

    #[test]
    fn socketpair_rights() {
        let native = Native::new();
        let mut sockets = [-1_i32; 2];
        // SAFETY: sockets points to two writable integers; success returns two uniquely owned descriptors.
        assert_eq!(
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sockets.as_mut_ptr()) },
            0
        );
        let sender = native.insert(sockets[0]).unwrap();
        let receiver = native.insert(sockets[1]).unwrap();
        let file = std::fs::File::open("/dev/null").unwrap();
        let attachment: OwnedFd = file.into();
        let sent = native
            .send_control(
                sender,
                HostSend {
                    payload: vec![7],
                    route: None,
                    controls: vec![HostControl::Rights(vec![attachment])],
                    nonblocking: true,
                    record: false,
                },
            )
            .unwrap();
        assert_eq!(sent.count, 1);
        assert!(sent.rights_consumed);
        let received = native.receive_control(receiver, 1, 1024, true, false).unwrap();
        assert_eq!(received.payload, [7]);
        let HostControl::Rights(rights) = &received.controls[0] else {
            panic!("SCM_RIGHTS missing");
        };
        assert_eq!(rights.len(), 1);
        native.close(sender);
        native.close(receiver);
    }
}
