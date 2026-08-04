/// An errno number at the Linux personality boundary.
///
/// Unknown host errno values intentionally remain representable because the C
/// engine passes values outside each host translation table through unchanged.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct Errno(i32);

impl Errno {
    pub const EPERM: Self = Self(1);
    pub const ENOENT: Self = Self(2);
    pub const ESRCH: Self = Self(3);
    pub const EINTR: Self = Self(4);
    pub const EIO: Self = Self(5);
    pub const ENXIO: Self = Self(6);
    pub const E2BIG: Self = Self(7);
    pub const EBADF: Self = Self(9);
    pub const ECHILD: Self = Self(10);
    pub const EAGAIN: Self = Self(11);
    pub const ENOMEM: Self = Self(12);
    pub const EACCES: Self = Self(13);
    pub const EFAULT: Self = Self(14);
    pub const EBUSY: Self = Self(16);
    pub const EEXIST: Self = Self(17);
    pub const EXDEV: Self = Self(18);
    pub const ENODEV: Self = Self(19);
    pub const ENOTDIR: Self = Self(20);
    pub const EISDIR: Self = Self(21);
    pub const EINVAL: Self = Self(22);
    pub const ENFILE: Self = Self(23);
    pub const EMFILE: Self = Self(24);
    pub const ENOTTY: Self = Self(25);
    pub const EFBIG: Self = Self(27);
    pub const ENOSPC: Self = Self(28);
    pub const ESPIPE: Self = Self(29);
    pub const EROFS: Self = Self(30);
    pub const EPIPE: Self = Self(32);
    pub const ERANGE: Self = Self(34);
    pub const EDEADLK: Self = Self(35);
    pub const ENAMETOOLONG: Self = Self(36);
    pub const ENOLCK: Self = Self(37);
    pub const ENOTEMPTY: Self = Self(39);
    pub const ENOSYS: Self = Self(38);
    pub const ELOOP: Self = Self(40);
    pub const ENODATA: Self = Self(61);
    pub const EIDRM: Self = Self(43);
    pub const EOVERFLOW: Self = Self(75);
    pub const ENOTSOCK: Self = Self(88);
    pub const EDESTADDRREQ: Self = Self(89);
    pub const EMSGSIZE: Self = Self(90);
    pub const EPROTOTYPE: Self = Self(91);
    pub const ENOPROTOOPT: Self = Self(92);
    pub const EPROTONOSUPPORT: Self = Self(93);
    pub const ESOCKTNOSUPPORT: Self = Self(94);
    pub const EOPNOTSUPP: Self = Self(95);
    pub const EAFNOSUPPORT: Self = Self(97);
    pub const EADDRINUSE: Self = Self(98);
    pub const EADDRNOTAVAIL: Self = Self(99);
    pub const ENETDOWN: Self = Self(100);
    pub const ENETUNREACH: Self = Self(101);
    pub const ENETRESET: Self = Self(102);
    pub const ECONNABORTED: Self = Self(103);
    pub const ECONNRESET: Self = Self(104);
    pub const EISCONN: Self = Self(106);
    pub const ENOTCONN: Self = Self(107);
    pub const ESHUTDOWN: Self = Self(108);
    pub const ETIMEDOUT: Self = Self(110);
    pub const ECONNREFUSED: Self = Self(111);
    pub const EHOSTUNREACH: Self = Self(113);
    pub const EALREADY: Self = Self(114);
    pub const EINPROGRESS: Self = Self(115);
    pub const EDQUOT: Self = Self(122);

    #[must_use]
    pub const fn from_raw(raw: i32) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn raw(self) -> i32 {
        self.0
    }

    #[must_use]
    pub const fn negative_i64(self) -> i64 {
        -(self.0 as i64)
    }

    /// Translates the current platform's native errno namespace into Linux's.
    ///
    /// Values outside the platform table pass through exactly as in C.
    #[must_use]
    pub const fn from_host(host: Self) -> Self {
        Self::from_raw(Self::number_from_host(host.raw()))
    }

    #[cfg(target_os = "linux")]
    const fn number_from_host(host: i32) -> i32 {
        host
    }

    #[cfg(target_os = "windows")]
    const fn number_from_host(host: i32) -> i32 {
        const LOW: [i16; 43] = [
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 22, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 22, 27, 28,
            29, 30, 31, 32, 33, 34, 22, 35, 22, 36, 37, 38, 39, 84,
        ];
        const HIGH: [i16; 41] = [
            98, 99, 97, 114, 74, 125, 103, 111, 104, 89, 113, 43, 115, 106, 40, 90, 100, 102, 101, 105, 61, 67, 42, 92,
            63, 60, 107, 131, 88, 95, 95, 22, 75, 130, 71, 93, 91, 62, 110, 26, 11,
        ];

        if host >= 0 && host < 43 {
            return LOW[host as usize] as i32;
        }
        if host >= 100 && host < 141 {
            return HIGH[(host - 100) as usize] as i32;
        }
        if host > 0 && host <= 140 { 22 } else { host }
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    const fn number_from_host(host: i32) -> i32 {
        const DARWIN_TO_LINUX: [i16; 107] = [
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 35, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28,
            29, 30, 31, 32, 33, 34, 11, 115, 114, 88, 89, 90, 91, 92, 93, 94, 95, 96, 97, 98, 99, 100, 101, 102, 103,
            104, 105, 106, 107, 108, 109, 110, 111, 40, 36, 112, 113, 39, 22, 87, 122, 116, 66, 22, 22, 22, 22, 22, 37,
            38, 22, 22, 22, 22, 22, 75, 22, 22, 22, 22, 125, 43, 42, 84, 61, 74, 72, 61, 67, 63, 60, 71, 62, 95, 22,
            131, 130, 22,
        ];

        if host >= 0 && host < 107 {
            DARWIN_TO_LINUX[host as usize] as i32
        } else {
            host
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_constants_match() {
        let constants = [
            (Errno::EPERM, 1),
            (Errno::ENOENT, 2),
            (Errno::ESRCH, 3),
            (Errno::EINTR, 4),
            (Errno::EIO, 5),
            (Errno::ENXIO, 6),
            (Errno::EBADF, 9),
            (Errno::ECHILD, 10),
            (Errno::EAGAIN, 11),
            (Errno::ENOMEM, 12),
            (Errno::EACCES, 13),
            (Errno::EFAULT, 14),
            (Errno::EBUSY, 16),
            (Errno::EEXIST, 17),
            (Errno::EXDEV, 18),
            (Errno::ENOTDIR, 20),
            (Errno::EISDIR, 21),
            (Errno::EINVAL, 22),
            (Errno::ENFILE, 23),
            (Errno::EMFILE, 24),
            (Errno::EFBIG, 27),
            (Errno::ENOSPC, 28),
            (Errno::ESPIPE, 29),
            (Errno::EROFS, 30),
            (Errno::EPIPE, 32),
            (Errno::ENAMETOOLONG, 36),
            (Errno::ENOSYS, 38),
            (Errno::ENOTEMPTY, 39),
            (Errno::ELOOP, 40),
            (Errno::EIDRM, 43),
            (Errno::EOVERFLOW, 75),
            (Errno::ENOTSOCK, 88),
            (Errno::ENOPROTOOPT, 92),
            (Errno::EPROTONOSUPPORT, 93),
            (Errno::EADDRINUSE, 98),
            (Errno::EAFNOSUPPORT, 97),
            (Errno::ENETUNREACH, 101),
            (Errno::ECONNRESET, 104),
            (Errno::EISCONN, 106),
            (Errno::ENOTCONN, 107),
            (Errno::EALREADY, 114),
            (Errno::EINPROGRESS, 115),
            (Errno::ETIMEDOUT, 110),
            (Errno::ECONNREFUSED, 111),
            (Errno::EDQUOT, 122),
        ];
        for (errno, raw) in constants {
            assert_eq!(errno.raw(), raw);
            assert_eq!(errno.negative_i64(), -i64::from(raw));
        }
    }

    #[test]
    fn unknown_values_work() {
        assert_eq!(Errno::from_raw(-1).raw(), -1);
        assert_eq!(Errno::from_raw(4095).raw(), 4095);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_host_identity() {
        for raw in [-1, 0, 1, 11, 35, 62, 4095] {
            assert_eq!(Errno::from_host(Errno::from_raw(raw)).raw(), raw);
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn ucrt_table_matches() {
        let cases = [
            (40, 38),
            (42, 84),
            (100, 98),
            (114, 40),
            (115, 90),
            (129, 95),
            (130, 95),
            (132, 75),
            (139, 26),
            (140, 11),
            (15, 22),
            (60, 22),
            (4095, 4095),
        ];
        for (host, linux) in cases {
            assert_eq!(Errno::from_host(Errno::from_raw(host)).raw(), linux);
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    #[test]
    fn darwin_edges_match() {
        let cases = [
            (11, 35),
            (35, 11),
            (62, 40),
            (78, 38),
            (84, 75),
            (91, 42),
            (93, 61),
            (96, 61),
            (102, 95),
            (104, 131),
            (105, 130),
            (4095, 4095),
            (-1, -1),
        ];
        for (host, linux) in cases {
            assert_eq!(Errno::from_host(Errno::from_raw(host)).raw(), linux);
        }
    }
}
