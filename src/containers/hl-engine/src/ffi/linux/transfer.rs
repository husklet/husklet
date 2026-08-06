use super::{ErrnoMapper, abi};
use crate::native_host::{HostError, PeerCredentials};
use std::mem;

const MAX_RIGHTS: usize = 8;
const ALIGN: usize = mem::size_of::<usize>();
const HEADER: usize = mem::size_of::<ControlHeader>();
const RIGHTS_SPACE: usize =
    ControlTransaction::align(HEADER) + ControlTransaction::align(MAX_RIGHTS * mem::size_of::<i32>());
const CREDS_SPACE: usize = ControlTransaction::align(HEADER) + ControlTransaction::align(mem::size_of::<Credentials>());
const CONTROL_SPACE: usize = RIGHTS_SPACE + CREDS_SPACE;

pub(super) struct ControlTransaction;

impl ControlTransaction {
    const fn align(value: usize) -> usize {
        (value + ALIGN - 1) & !(ALIGN - 1)
    }

    fn close_all(descriptors: &[i32]) {
        for descriptor in descriptors {
            // SAFETY: received rights are owned by this transaction until accepted.
            let _ = unsafe { abi::close(*descriptor) };
        }
    }

    pub(super) fn enable_credentials(descriptor: i32) -> Result<(), HostError> {
        let enabled: i32 = 1;
        // SAFETY: enabled is an aligned scalar valid for the supplied exact length.
        let result = unsafe {
            setsockopt(
                descriptor,
                SOL_SOCKET,
                SO_PASSCRED,
                (&raw const enabled).cast(),
                mem::size_of::<i32>() as u32,
            )
        };
        (result == 0).then_some(()).ok_or_else(ErrnoMapper::current)
    }

    fn credential_process(process: i32, rights: &[i32]) -> Result<u32, HostError> {
        if let Ok(process) = u32::try_from(process) {
            return Ok(process);
        }
        Self::close_all(rights);
        Err(HostError::Invalid)
    }
}

pub(crate) fn send(descriptor: i32, bytes: &[u8], descriptors: &[i32]) -> Result<usize, HostError> {
    if descriptors.is_empty() || descriptors.len() > MAX_RIGHTS {
        return Err(HostError::Invalid);
    }
    let mut control = [0_usize; CONTROL_SPACE / ALIGN];
    let control_bytes = std::mem::size_of_val(descriptors);
    let used = ControlTransaction::align(HEADER) + ControlTransaction::align(control_bytes);
    let header = control.as_mut_ptr().cast::<ControlHeader>();
    // SAFETY: control is usize-aligned and large enough for header and rights.
    unsafe {
        (*header).length = ControlTransaction::align(HEADER) + control_bytes;
        (*header).level = SOL_SOCKET;
        (*header).kind = SCM_RIGHTS;
        std::ptr::copy_nonoverlapping(
            descriptors.as_ptr(),
            control
                .as_mut_ptr()
                .cast::<u8>()
                .add(ControlTransaction::align(HEADER))
                .cast(),
            descriptors.len(),
        );
    }
    let mut iov = Iovec {
        base: bytes.as_ptr().cast_mut().cast(),
        length: bytes.len(),
    };
    let message = MessageHeader {
        name: std::ptr::null_mut(),
        name_length: 0,
        iov: &raw mut iov,
        iov_length: 1,
        control: control.as_mut_ptr().cast(),
        control_length: used,
        flags: 0,
    };
    // SAFETY: message and its buffers remain live for the synchronous call.
    let result = unsafe { sendmsg(descriptor, &raw const message, 0) };
    result.try_into().map_err(|_| ErrnoMapper::current())
}

pub(crate) fn receive(
    descriptor: i32,
    bytes: &mut [u8],
    capacity: usize,
) -> Result<(usize, Vec<i32>, Option<PeerCredentials>), HostError> {
    if capacity > MAX_RIGHTS {
        return Err(HostError::Invalid);
    }
    let mut control = [0_usize; CONTROL_SPACE / ALIGN];
    let mut iov = Iovec {
        base: bytes.as_mut_ptr().cast(),
        length: bytes.len(),
    };
    let mut message = MessageHeader {
        name: std::ptr::null_mut(),
        name_length: 0,
        iov: &raw mut iov,
        iov_length: 1,
        control: control.as_mut_ptr().cast(),
        control_length: control.len() * ALIGN,
        flags: 0,
    };
    // SAFETY: message and both output buffers are uniquely writable and bounded.
    let result = unsafe { recvmsg(descriptor, &raw mut message, MSG_CMSG_CLOEXEC) };
    let count: usize = result.try_into().map_err(|_| ErrnoMapper::current())?;
    let parsed = ControlTransaction::parse_control(&control, message.control_length, capacity);
    if message.flags & MSG_CTRUNC != 0 {
        if let Ok((rights, _)) = parsed {
            ControlTransaction::close_all(&rights);
        }
        return Err(HostError::Exhausted);
    }
    parsed.map(|(rights, credentials)| (count, rights, credentials))
}

impl ControlTransaction {
    fn parse_control(
        control: &[usize],
        length: usize,
        capacity: usize,
    ) -> Result<(Vec<i32>, Option<PeerCredentials>), HostError> {
        if length > control.len() * ALIGN {
            return Err(HostError::Invalid);
        }
        let bytes = control.as_ptr().cast::<u8>();
        let mut offset = 0;
        let mut rights = Vec::new();
        let mut credentials = None;
        while offset < length {
            if length - offset < HEADER {
                ControlTransaction::close_all(&rights);
                return Err(HostError::Invalid);
            }
            // SAFETY: control is aligned and offset advances by aligned records.
            let header = unsafe { &*bytes.add(offset).cast::<ControlHeader>() };
            if header.length < ControlTransaction::align(HEADER) || header.length > length - offset {
                ControlTransaction::close_all(&rights);
                return Err(HostError::Invalid);
            }
            let data_length = header.length - ControlTransaction::align(HEADER);
            // SAFETY: validated header bounds contain the complete payload.
            let data = unsafe { bytes.add(offset + ControlTransaction::align(HEADER)) };
            match (header.level, header.kind) {
                (SOL_SOCKET, SCM_RIGHTS) if data_length.is_multiple_of(mem::size_of::<i32>()) => {
                    let count = data_length / mem::size_of::<i32>();
                    // SAFETY: SCM_RIGHTS payload contains aligned native integers.
                    let found = unsafe { std::slice::from_raw_parts(data.cast::<i32>(), count) };
                    rights.extend_from_slice(found);
                }
                (SOL_SOCKET, SCM_CREDENTIALS) if data_length == mem::size_of::<Credentials>() => {
                    // SAFETY: exact validated credentials payload.
                    let found = unsafe { *data.cast::<Credentials>() };
                    let process = ControlTransaction::credential_process(found.process, &rights)?;
                    credentials = Some(PeerCredentials {
                        process,
                        user: found.user,
                        group: found.group,
                    });
                }
                _ => {
                    ControlTransaction::close_all(&rights);
                    return Err(HostError::Invalid);
                }
            }
            offset = offset
                .checked_add(ControlTransaction::align(header.length))
                .ok_or(HostError::Invalid)?;
        }
        if rights.len() > capacity {
            ControlTransaction::close_all(&rights);
            return Err(HostError::Exhausted);
        }
        Ok((rights, credentials))
    }
}

#[repr(C)]
struct Iovec {
    base: *mut abi::c_void,
    length: usize,
}

#[repr(C)]
struct MessageHeader {
    name: *mut abi::c_void,
    name_length: u32,
    iov: *mut Iovec,
    iov_length: usize,
    control: *mut abi::c_void,
    control_length: usize,
    flags: i32,
}

#[repr(C)]
struct ControlHeader {
    length: usize,
    level: i32,
    kind: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Credentials {
    process: i32,
    user: u32,
    group: u32,
}

const SOL_SOCKET: i32 = 1;
const SCM_RIGHTS: i32 = 1;
const SCM_CREDENTIALS: i32 = 2;
const SO_PASSCRED: i32 = 16;
const MSG_CTRUNC: i32 = 8;
const MSG_CMSG_CLOEXEC: i32 = 0x40000000;

unsafe extern "C" {
    fn sendmsg(descriptor: i32, message: *const MessageHeader, flags: i32) -> isize;
    fn recvmsg(descriptor: i32, message: *mut MessageHeader, flags: i32) -> isize;
    fn setsockopt(descriptor: i32, level: i32, option: i32, value: *const abi::c_void, length: u32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixDatagram;

    #[test]
    fn malformed_control_headers() {
        let mut control = [0_usize; 8];
        let header = control.as_mut_ptr().cast::<ControlHeader>();
        // SAFETY: control is aligned and contains a complete header.
        unsafe {
            (*header).length = HEADER - 1;
            (*header).level = SOL_SOCKET;
            (*header).kind = SCM_RIGHTS;
        }
        assert_eq!(
            ControlTransaction::parse_control(&control, HEADER, MAX_RIGHTS),
            Err(HostError::Invalid)
        );
        // SAFETY: same valid header storage, now claiming beyond the buffer.
        unsafe {
            (*header).length = control.len() * ALIGN + 1;
        }
        assert_eq!(
            ControlTransaction::parse_control(&control, control.len() * ALIGN, MAX_RIGHTS),
            Err(HostError::Invalid)
        );
    }

    #[test]
    fn rights_are_cloexec() {
        let (sender, receiver) = UnixDatagram::pair().unwrap();
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        assert_eq!(send(sender.as_raw_fd(), b"x", &[listener.as_raw_fd()]).unwrap(), 1);
        let mut byte = [0];
        let (count, rights, credentials) = receive(receiver.as_raw_fd(), &mut byte, 1).unwrap();
        assert_eq!(count, 1);
        assert_eq!(byte, *b"x");
        assert!(credentials.is_none());
        assert_eq!(rights.len(), 1);
        // SAFETY: the transaction returned one live owned descriptor, and fcntl
        // observes only its descriptor-local flags without retaining storage.
        let flags = unsafe { libc::fcntl(rights[0], libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
        ControlTransaction::close_all(&rights);
    }
}
