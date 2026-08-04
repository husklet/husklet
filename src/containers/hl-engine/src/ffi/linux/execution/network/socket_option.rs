#![allow(unsafe_code)]

use std::mem::{size_of, zeroed};

use hl_linux::GuestSocketOption;
use hl_runtime::RuntimeNetworkError;

use super::Native;

pub(super) fn set(
    descriptor: i32,
    level: i32,
    option: i32,
    value: GuestSocketOption,
) -> Result<(), RuntimeNetworkError> {
    #[cfg(target_os = "macos")]
    if (level, option) == (6, 12) {
        return Ok(());
    }
    let (level, option) = HostOption::resolve(level, option)?;
    let result = match value {
        GuestSocketOption::Scalar(scalar) => {
            // SAFETY: scalar is initialized and readable for the supplied length.
            unsafe {
                libc::setsockopt(
                    descriptor,
                    level,
                    option,
                    (&scalar as *const i32).cast(),
                    size_of::<i32>() as _,
                )
            }
        }
        GuestSocketOption::Linger { enabled, seconds } => {
            let linger = libc::linger {
                l_onoff: enabled,
                l_linger: seconds,
            };
            // SAFETY: linger is initialized and readable for the supplied length.
            unsafe {
                libc::setsockopt(
                    descriptor,
                    level,
                    option,
                    (&linger as *const libc::linger).cast(),
                    size_of::<libc::linger>() as _,
                )
            }
        }
        GuestSocketOption::Timeval { seconds, microseconds } => {
            let timeout = libc::timeval {
                tv_sec: seconds as _,
                tv_usec: microseconds as _,
            };
            // SAFETY: timeout is initialized and readable for the supplied length.
            unsafe {
                libc::setsockopt(
                    descriptor,
                    level,
                    option,
                    (&timeout as *const libc::timeval).cast(),
                    size_of::<libc::timeval>() as _,
                )
            }
        }
        GuestSocketOption::Credentials { .. } => return Err(RuntimeNetworkError::Unsupported),
        GuestSocketOption::Bytes(_) => return Err(RuntimeNetworkError::Unsupported),
        GuestSocketOption::Filter(instructions) => {
            #[cfg(target_os = "linux")]
            {
                let filters: Vec<libc::sock_filter> = instructions
                    .into_iter()
                    .map(|instruction| libc::sock_filter {
                        code: instruction.code,
                        jt: instruction.jump_true,
                        jf: instruction.jump_false,
                        k: instruction.value,
                    })
                    .collect();
                let program = libc::sock_fprog {
                    len: filters.len().try_into().map_err(|_| RuntimeNetworkError::Invalid)?,
                    filter: filters.as_ptr().cast_mut(),
                };
                unsafe {
                    libc::setsockopt(
                        descriptor,
                        level,
                        option,
                        (&raw const program).cast(),
                        std::mem::size_of::<libc::sock_fprog>() as libc::socklen_t,
                    )
                }
            }
            #[cfg(not(target_os = "linux"))]
            return Err(RuntimeNetworkError::Unsupported);
        }
    };
    if result == 0 {
        Ok(())
    } else {
        Err(Native::runtime_error())
    }
}

pub(super) fn get(descriptor: i32, level: i32, option: i32) -> Result<GuestSocketOption, RuntimeNetworkError> {
    let guest = (level, option);
    if guest == (6, 11) {
        return tcp_info(descriptor);
    }
    let (level, option) = HostOption::resolve(level, option)?;
    if guest == (1, 13) {
        // SAFETY: zero is a valid initialization for linger.
        let mut linger = unsafe { zeroed::<libc::linger>() };
        let mut length = size_of::<libc::linger>() as libc::socklen_t;
        // SAFETY: linger and length are writable for getsockopt.
        let result = unsafe {
            libc::getsockopt(
                descriptor,
                level,
                option,
                (&mut linger as *mut libc::linger).cast(),
                &mut length,
            )
        };
        return if result == 0 {
            Ok(GuestSocketOption::Linger {
                enabled: linger.l_onoff,
                seconds: linger.l_linger,
            })
        } else {
            Err(Native::runtime_error())
        };
    }
    if matches!(guest, (1, 20 | 21)) {
        // SAFETY: zero is a valid initialization for timeval.
        let mut timeout = unsafe { zeroed::<libc::timeval>() };
        let mut length = size_of::<libc::timeval>() as libc::socklen_t;
        // SAFETY: timeout and length are writable for getsockopt.
        let result = unsafe {
            libc::getsockopt(
                descriptor,
                level,
                option,
                (&mut timeout as *mut libc::timeval).cast(),
                &mut length,
            )
        };
        return if result == 0 {
            Ok(GuestSocketOption::Timeval {
                seconds: timeout.tv_sec as i64,
                microseconds: timeout.tv_usec as i64,
            })
        } else {
            Err(Native::runtime_error())
        };
    }
    let mut scalar = 0_i32;
    let mut length = size_of::<i32>() as libc::socklen_t;
    // SAFETY: scalar and length are writable for getsockopt.
    let result = unsafe { libc::getsockopt(descriptor, level, option, (&mut scalar as *mut i32).cast(), &mut length) };
    if result == 0 {
        Ok(GuestSocketOption::Scalar(scalar))
    } else {
        Err(Native::runtime_error())
    }
}

#[cfg(target_os = "linux")]
fn tcp_info(descriptor: i32) -> Result<GuestSocketOption, RuntimeNetworkError> {
    let mut bytes = vec![0_u8; 512];
    let mut length = bytes.len() as libc::socklen_t;
    // SAFETY: bytes owns writable storage for the advertised capacity and length is a live output cell.
    let result = unsafe {
        libc::getsockopt(
            descriptor,
            libc::IPPROTO_TCP,
            libc::TCP_INFO,
            bytes.as_mut_ptr().cast(),
            &mut length,
        )
    };
    if result != 0 {
        return Err(Native::runtime_error());
    }
    bytes.truncate(length as usize);
    Ok(GuestSocketOption::Bytes(bytes))
}

#[cfg(target_os = "macos")]
fn tcp_info(_: i32) -> Result<GuestSocketOption, RuntimeNetworkError> {
    Err(RuntimeNetworkError::Unsupported)
}

struct HostOption;

impl HostOption {
    fn resolve(level: i32, option: i32) -> Result<(i32, i32), RuntimeNetworkError> {
        match (level, option) {
            (1, 1) => Ok((libc::SOL_SOCKET, libc::SO_DEBUG)),
            (1, 2) => Ok((libc::SOL_SOCKET, libc::SO_REUSEADDR)),
            (1, 5) => Ok((libc::SOL_SOCKET, libc::SO_DONTROUTE)),
            (1, 6) => Ok((libc::SOL_SOCKET, libc::SO_BROADCAST)),
            (1, 7) => Ok((libc::SOL_SOCKET, libc::SO_SNDBUF)),
            (1, 8) => Ok((libc::SOL_SOCKET, libc::SO_RCVBUF)),
            (1, 9) => Ok((libc::SOL_SOCKET, libc::SO_KEEPALIVE)),
            (1, 10) => Ok((libc::SOL_SOCKET, libc::SO_OOBINLINE)),
            (1, 13) => Ok((libc::SOL_SOCKET, libc::SO_LINGER)),
            (1, 15) => Ok((libc::SOL_SOCKET, libc::SO_REUSEPORT)),
            (1, 20) => Ok((libc::SOL_SOCKET, libc::SO_RCVTIMEO)),
            (1, 21) => Ok((libc::SOL_SOCKET, libc::SO_SNDTIMEO)),
            #[cfg(target_os = "linux")]
            (1, 26) => Ok((libc::SOL_SOCKET, libc::SO_ATTACH_FILTER)),
            #[cfg(target_os = "linux")]
            (1, 27) => Ok((libc::SOL_SOCKET, libc::SO_DETACH_FILTER)),
            (0, 2) => Ok((libc::IPPROTO_IP, libc::IP_TTL)),
            (0, 1) => Ok((libc::IPPROTO_IP, libc::IP_TOS)),
            (0, 8) => Ok((libc::IPPROTO_IP, libc::IP_PKTINFO)),
            (0, 10) => Ok((libc::IPPROTO_IP, libc::IP_MTU_DISCOVER)),
            (0, 11) => Ok((libc::IPPROTO_IP, libc::IP_RECVERR)),
            (0, 12) => Ok((libc::IPPROTO_IP, libc::IP_RECVTTL)),
            (0, 13) => Ok((libc::IPPROTO_IP, libc::IP_RECVTOS)),
            (0, 15) => Ok((libc::IPPROTO_IP, libc::IP_FREEBIND)),
            (6, 1) => Ok((libc::IPPROTO_TCP, libc::TCP_NODELAY)),
            (6, 2) => Ok((libc::IPPROTO_TCP, libc::TCP_MAXSEG)),
            (6, 3) => cork(),
            (6, 4) => Ok((libc::IPPROTO_TCP, keep_idle())),
            (6, 5) => Ok((libc::IPPROTO_TCP, libc::TCP_KEEPINTVL)),
            (6, 6) => Ok((libc::IPPROTO_TCP, libc::TCP_KEEPCNT)),
            (6, 12) => quick_ack(),
            (41, 26) => Ok((libc::IPPROTO_IPV6, libc::IPV6_V6ONLY)),
            (41, 16) => Ok((libc::IPPROTO_IPV6, libc::IPV6_UNICAST_HOPS)),
            (41, 49) => Ok((libc::IPPROTO_IPV6, libc::IPV6_RECVPKTINFO)),
            (41, 51) => Ok((libc::IPPROTO_IPV6, libc::IPV6_RECVHOPLIMIT)),
            (41, 66) => Ok((libc::IPPROTO_IPV6, libc::IPV6_RECVTCLASS)),
            (41, 67) => Ok((libc::IPPROTO_IPV6, libc::IPV6_TCLASS)),
            _ => Err(RuntimeNetworkError::Unsupported),
        }
    }
}

#[cfg(target_os = "macos")]
const fn keep_idle() -> i32 {
    libc::TCP_KEEPALIVE
}

#[cfg(not(target_os = "macos"))]
const fn keep_idle() -> i32 {
    libc::TCP_KEEPIDLE
}

#[cfg(target_os = "macos")]
fn cork() -> Result<(i32, i32), RuntimeNetworkError> {
    Ok((libc::IPPROTO_TCP, libc::TCP_NOPUSH))
}

#[cfg(not(target_os = "macos"))]
fn cork() -> Result<(i32, i32), RuntimeNetworkError> {
    Ok((libc::IPPROTO_TCP, libc::TCP_CORK))
}

#[cfg(target_os = "macos")]
fn quick_ack() -> Result<(i32, i32), RuntimeNetworkError> {
    Err(RuntimeNetworkError::Unsupported)
}

#[cfg(not(target_os = "macos"))]
fn quick_ack() -> Result<(i32, i32), RuntimeNetworkError> {
    Ok((libc::IPPROTO_TCP, libc::TCP_QUICKACK))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::{get, set};
    use hl_linux::{BpfInstruction, GuestSocketOption};

    fn returning(value: u32) -> GuestSocketOption {
        GuestSocketOption::Filter(vec![BpfInstruction {
            code: 0x06,
            jump_true: 0,
            jump_false: 0,
            value,
        }])
    }

    #[test]
    fn filter_drops_and_accepts_datagrams() {
        let mut descriptors = [-1_i32; 2];
        // SAFETY: descriptors points to two writable integers.
        assert_eq!(
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_DGRAM, 0, descriptors.as_mut_ptr()) },
            0
        );
        set(descriptors[1], 1, 26, returning(0)).unwrap();
        assert_eq!(unsafe { libc::send(descriptors[0], c"drop".as_ptr().cast(), 4, 0) }, 4);
        let mut byte = 0_u8;
        assert_eq!(
            unsafe { libc::recv(descriptors[1], (&raw mut byte).cast(), 1, libc::MSG_DONTWAIT) },
            -1
        );
        assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::EAGAIN));
        set(descriptors[1], 1, 26, returning(u32::MAX)).unwrap();
        assert_eq!(unsafe { libc::send(descriptors[0], c"pass".as_ptr().cast(), 4, 0) }, 4);
        let mut bytes = [0_u8; 4];
        assert_eq!(
            unsafe { libc::recv(descriptors[1], bytes.as_mut_ptr().cast(), bytes.len(), 0) },
            4
        );
        assert_eq!(&bytes, b"pass");
        for descriptor in descriptors {
            assert_eq!(unsafe { libc::close(descriptor) }, 0);
        }
    }

    #[test]
    fn ipv4_round_trip() {
        // SAFETY: socket returns a newly owned descriptor and takes no borrowed storage.
        let descriptor = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
        assert!(descriptor >= 0);
        let options = [(1, 0x10), (2, 42), (10, libc::IP_PMTUDISC_DO), (8, 1), (12, 1), (13, 1)];
        for (option, value) in options {
            set(descriptor, 0, option, GuestSocketOption::Scalar(value)).unwrap();
            assert_eq!(get(descriptor, 0, option).unwrap(), GuestSocketOption::Scalar(value));
        }
        // SAFETY: descriptor is the live socket owned by this test and is closed exactly once.
        assert_eq!(unsafe { libc::close(descriptor) }, 0);
    }

    #[test]
    fn ipv6_round_trip() {
        // SAFETY: socket returns a newly owned descriptor and takes no borrowed storage.
        let descriptor = unsafe { libc::socket(libc::AF_INET6, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
        assert!(descriptor >= 0);
        let options = [(26, 1), (16, 55), (49, 1), (51, 1), (66, 1), (67, 0x20)];
        for (option, value) in options {
            set(descriptor, 41, option, GuestSocketOption::Scalar(value)).unwrap();
            assert_eq!(get(descriptor, 41, option).unwrap(), GuestSocketOption::Scalar(value));
        }
        // SAFETY: descriptor is the live socket owned by this test and is closed exactly once.
        assert_eq!(unsafe { libc::close(descriptor) }, 0);
    }

    #[test]
    fn tcp_round_trip() {
        // SAFETY: socket returns a newly owned descriptor and takes no borrowed storage.
        let descriptor = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
        assert!(descriptor >= 0);
        for (option, value) in [(1, 1), (3, 1), (4, 55), (5, 7), (6, 7), (12, 1)] {
            set(descriptor, 6, option, GuestSocketOption::Scalar(value)).unwrap();
            assert_eq!(get(descriptor, 6, option).unwrap(), GuestSocketOption::Scalar(value));
        }
        let GuestSocketOption::Scalar(maximum) = get(descriptor, 6, 2).unwrap() else {
            panic!("TCP_MAXSEG must be scalar");
        };
        assert!(maximum > 0);
        let GuestSocketOption::Bytes(info) = get(descriptor, 6, 11).unwrap() else {
            panic!("TCP_INFO must be an opaque Linux record");
        };
        assert!(!info.is_empty());
        assert_eq!(info[0], 7);
        // SAFETY: descriptor is the live socket owned by this test and is closed exactly once.
        assert_eq!(unsafe { libc::close(descriptor) }, 0);
    }
}
