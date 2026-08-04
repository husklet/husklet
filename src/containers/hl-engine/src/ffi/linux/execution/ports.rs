use std::sync::Arc;

#[cfg(test)]
use std::io::Read;

use hl_descriptor::SeekPosition;
#[cfg(test)]
use hl_linux::SyscallOperation;
use hl_linux::{Errno, LinuxResult};

use super::VirtualMemory;
use super::descriptor::Set;

mod command;
pub(super) mod random;

const TRANSFER_LIMIT: usize = 1024 * 1024;
const VECTOR_LIMIT: usize = 1024;

pub(super) const SUPPORTED: &[&str] = &[
    "accept",
    "accept4",
    "access",
    "adjtimex",
    "alarm",
    "arch_prctl",
    "bind",
    "brk",
    "capget",
    "capset",
    "clock_adjtime",
    "clock_getres",
    "clock_gettime",
    "clock_nanosleep",
    "clock_settime",
    "clone",
    "close",
    "close_range",
    "connect",
    "dup",
    "dup2",
    "dup3",
    "epoll_create1",
    "epoll_ctl",
    "epoll_wait",
    "eventfd2",
    "execve",
    "execveat",
    "exit",
    "exit_group",
    "faccessat",
    "faccessat2",
    "fadvise64",
    "fallocate",
    "fchmod",
    "fchmodat",
    "fchown",
    "fchownat",
    "fcntl",
    "fdatasync",
    "flock",
    "fork",
    "fstat",
    "fstatfs",
    "fsync",
    "ftruncate",
    "futex",
    "get_robust_list",
    "getcwd",
    "getdents64",
    "getegid",
    "geteuid",
    "getgid",
    "getgroups",
    "getitimer",
    "getpeername",
    "getpgid",
    "getpgrp",
    "getpid",
    "getppid",
    "getrandom",
    "getresgid",
    "getresuid",
    "getrlimit",
    "getsid",
    "getsockname",
    "getsockopt",
    "gettid",
    "gettimeofday",
    "getuid",
    "inotify_add_watch",
    "inotify_init1",
    "inotify_rm_watch",
    "io_cancel",
    "io_destroy",
    "io_getevents",
    "io_setup",
    "io_submit",
    "ioctl",
    "kill",
    "linkat",
    "listen",
    "lseek",
    "madvise",
    "membarrier",
    "memfd_create",
    "mincore",
    "mkdirat",
    "mlock",
    "mlock2",
    "mlockall",
    "mmap",
    "mprotect",
    "mremap",
    "msgctl",
    "msgget",
    "msgrcv",
    "msgsnd",
    "msync",
    "munlock",
    "munlockall",
    "munmap",
    "nanosleep",
    "newfstatat",
    "openat",
    "pidfd_getfd",
    "pidfd_open",
    "pidfd_send_signal",
    "pipe2",
    "poll",
    "ppoll",
    "prctl",
    "pread64",
    "preadv",
    "preadv2",
    "prlimit64",
    "pselect6",
    "pwrite64",
    "pwritev",
    "pwritev2",
    "read",
    "readahead",
    "readlinkat",
    "readv",
    "recvfrom",
    "recvmsg",
    "renameat",
    "rt_sigaction",
    "rt_sigpending",
    "rt_sigprocmask",
    "rt_sigqueueinfo",
    "rt_sigreturn",
    "rt_tgsigqueueinfo",
    "sched_get_priority_max",
    "sched_get_priority_min",
    "sched_getaffinity",
    "sched_getattr",
    "sched_getparam",
    "sched_getscheduler",
    "sched_rr_get_interval",
    "sched_setaffinity",
    "sched_setattr",
    "sched_setparam",
    "sched_setscheduler",
    "seccomp",
    "select",
    "semctl",
    "semget",
    "semop",
    "semtimedop",
    "sendmsg",
    "sendto",
    "set_robust_list",
    "set_tid_address",
    "setdomainname",
    "setfsgid",
    "setfsuid",
    "setgid",
    "setgroups",
    "sethostname",
    "setitimer",
    "setns",
    "setpgid",
    "setregid",
    "setresgid",
    "setresuid",
    "setreuid",
    "setrlimit",
    "setsid",
    "setsockopt",
    "setuid",
    "shmat",
    "shmctl",
    "shmdt",
    "shmget",
    "shutdown",
    "sigaltstack",
    "signalfd4",
    "socket",
    "socketpair",
    "statfs",
    "statx",
    "symlinkat",
    "syncfs",
    "sysinfo",
    "tgkill",
    "time",
    "timer_create",
    "timer_delete",
    "timer_getoverrun",
    "timer_gettime",
    "timer_settime",
    "timerfd_create",
    "timerfd_gettime",
    "timerfd_settime",
    "tkill",
    "truncate",
    "umask",
    "uname",
    "unlinkat",
    "unshare",
    "wait4",
    "write",
    "writev",
];

pub(super) struct DescriptorPort {
    memory: Arc<VirtualMemory>,
    descriptors: Arc<Set>,
    entropy: Arc<dyn random::EntropySource>,
}

impl DescriptorPort {
    pub(super) fn new(
        memory: Arc<VirtualMemory>,
        descriptors: Arc<Set>,
        entropy: Arc<dyn random::EntropySource>,
    ) -> Self {
        Self {
            memory,
            descriptors,
            entropy,
        }
    }

    #[cfg(test)]
    fn with_input(memory: Arc<VirtualMemory>, input: Box<dyn Read + Send>) -> Self {
        Self::new(
            memory,
            Arc::new(Set::with_input(input).unwrap()),
            Arc::new(crate::ffi::linux::execution::image_data::Entropy),
        )
    }

    fn read(&mut self, descriptor: u64, address: u64, length: u64) -> LinuxResult {
        let lease = match self.descriptors.pin(descriptor as i32) {
            Ok(lease) => lease,
            Err(error) => return LinuxResult::Error(error),
        };
        let Ok(length) = Self::length(length) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        if length == 0 {
            return LinuxResult::Value(0);
        }
        if self.memory.probe_write(address, length as u64).is_err() {
            return LinuxResult::Error(Errno::EFAULT);
        }
        let mut bytes = vec![0; length];
        let read = match lease.read(&mut bytes) {
            Ok(read) => read,
            Err(error) => return LinuxResult::Error(Set::object_errno(error)),
        };
        if self.memory.write(address, &bytes[..read]).is_err() {
            return LinuxResult::Error(Errno::EFAULT);
        }
        LinuxResult::Value(read as u64)
    }

    fn readv(&mut self, descriptor: u64, address: u64, count: u64) -> LinuxResult {
        let lease = match self.descriptors.pin(descriptor as i32) {
            Ok(lease) => lease,
            Err(error) => return LinuxResult::Error(error),
        };
        let vectors = match self.vectors(address, count) {
            Ok(vectors) => vectors,
            Err(error) => return LinuxResult::Error(error),
        };
        if vectors.is_empty() {
            return LinuxResult::Value(0);
        }
        for (pointer, length) in &vectors {
            if self.memory.probe_write(*pointer, *length as u64).is_err() {
                return LinuxResult::Error(Errno::EFAULT);
            }
        }
        let total = vectors.iter().map(|(_, length)| length).sum();
        let mut bytes = vec![0; total];
        let read = match lease.read(&mut bytes) {
            Ok(read) => read,
            Err(error) => return LinuxResult::Error(Set::object_errno(error)),
        };
        let mut remaining = read;
        let mut offset = 0;
        let writes = vectors
            .iter()
            .filter_map(|(pointer, vector_length)| {
                let length = remaining.min(*vector_length);
                let start = offset;
                offset += *vector_length;
                remaining -= length;
                (length != 0).then_some((*pointer, &bytes[start..start + length]))
            })
            .collect::<Vec<_>>();
        if self.memory.write_scatter(&writes).is_err() {
            return LinuxResult::Error(Errno::EFAULT);
        }
        LinuxResult::Value(read as u64)
    }

    fn write(&self, descriptor: u64, address: u64, length: u64) -> LinuxResult {
        let lease = match self.descriptors.pin(descriptor as i32) {
            Ok(lease) => lease,
            Err(error) => return LinuxResult::Error(error),
        };
        let Ok(length) = usize::try_from(length) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        if length > TRANSFER_LIMIT {
            return LinuxResult::Error(Errno::EINVAL);
        }
        if length == 0 {
            return LinuxResult::Value(0);
        }
        let mut bytes = vec![0; length];
        if self.memory.read(address, &mut bytes).is_err() {
            return LinuxResult::Error(Errno::EFAULT);
        }
        lease
            .write(&bytes)
            .map(|written| LinuxResult::Value(written as u64))
            .unwrap_or_else(|error| LinuxResult::Error(Set::object_errno(error)))
    }

    fn seek(&self, descriptor: i32, offset: u64, whence: u32) -> LinuxResult {
        let lease = match self.descriptors.pin(descriptor) {
            Ok(lease) => lease,
            Err(error) => return LinuxResult::Error(error),
        };
        let signed = offset as i64;
        let position = match whence {
            0 if signed >= 0 => SeekPosition::Start(offset),
            1 => SeekPosition::Current(signed),
            2 => SeekPosition::End(signed),
            3 if signed >= 0 => SeekPosition::Data(offset),
            4 if signed >= 0 => SeekPosition::Hole(offset),
            3 | 4 => return LinuxResult::Error(Errno::ENXIO),
            _ => return LinuxResult::Error(Errno::EINVAL),
        };
        lease
            .seek(position)
            .map(LinuxResult::Value)
            .unwrap_or_else(|error| LinuxResult::Error(Set::object_errno(error)))
    }

    fn writev(&self, descriptor: u64, address: u64, count: u64) -> LinuxResult {
        let lease = match self.descriptors.pin(descriptor as i32) {
            Ok(lease) => lease,
            Err(error) => return LinuxResult::Error(error),
        };
        let Ok(count) = usize::try_from(count) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        if count > VECTOR_LIMIT {
            return LinuxResult::Error(Errno::EINVAL);
        }
        if count == 0 {
            return LinuxResult::Value(0);
        }
        let mut vectors = Vec::with_capacity(count);
        let mut total = 0_usize;
        for index in 0..count {
            let Some(record) = address.checked_add((index as u64) * 16) else {
                return LinuxResult::Error(Errno::EFAULT);
            };
            let mut encoded = [0_u8; 16];
            if self.memory.read(record, &mut encoded).is_err() {
                return LinuxResult::Error(Errno::EFAULT);
            }
            let pointer = u64::from_le_bytes(encoded[..8].try_into().expect("fixed slice"));
            let length = u64::from_le_bytes(encoded[8..].try_into().expect("fixed slice"));
            let Ok(length) = usize::try_from(length) else {
                return LinuxResult::Error(Errno::EINVAL);
            };
            let Some(next) = total.checked_add(length) else {
                return LinuxResult::Error(Errno::EINVAL);
            };
            if next > TRANSFER_LIMIT {
                return LinuxResult::Error(Errno::EINVAL);
            }
            let mut bytes = vec![0; length];
            if self.memory.read(pointer, &mut bytes).is_err() {
                return LinuxResult::Error(Errno::EFAULT);
            }
            total = next;
            vectors.push(bytes);
        }
        let bytes = vectors.into_iter().flatten().collect::<Vec<_>>();
        lease
            .write(&bytes)
            .map(|written| LinuxResult::Value(written as u64))
            .unwrap_or_else(|error| LinuxResult::Error(Set::object_errno(error)))
    }

    fn length(length: u64) -> Result<usize, ()> {
        let length = usize::try_from(length).map_err(|_| ())?;
        if length > TRANSFER_LIMIT { Err(()) } else { Ok(length) }
    }

    fn vectors(&self, address: u64, count: u64) -> Result<Vec<(u64, usize)>, Errno> {
        let count = usize::try_from(count).map_err(|_| Errno::EINVAL)?;
        if count > VECTOR_LIMIT {
            return Err(Errno::EINVAL);
        }
        let mut vectors = Vec::with_capacity(count);
        let mut total = 0_usize;
        for index in 0..count {
            let offset = (index as u64).checked_mul(16).ok_or(Errno::EFAULT)?;
            let record = address.checked_add(offset).ok_or(Errno::EFAULT)?;
            let mut encoded = [0_u8; 16];
            self.memory.read(record, &mut encoded).map_err(|_| Errno::EFAULT)?;
            let pointer = u64::from_le_bytes(encoded[..8].try_into().expect("fixed slice"));
            let length = u64::from_le_bytes(encoded[8..].try_into().expect("fixed slice"));
            let length = usize::try_from(length).map_err(|_| Errno::EINVAL)?;
            total = total.checked_add(length).ok_or(Errno::EINVAL)?;
            if total > TRANSFER_LIMIT {
                return Err(Errno::EINVAL);
            }
            vectors.push((pointer, length));
        }
        Ok(vectors)
    }
}

#[cfg(test)]
mod test {
    use std::io::IoSliceMut;

    use super::*;

    use hl_descriptor::StatusFlags;
    use hl_isa::GuestAddress;
    use hl_linux::{DescriptorIoSyscalls, SyscallFamily};
    use hl_memory::{Backing, MapRequest, MappingHost, Placement, Protection};

    use crate::ffi::linux::MappingHostAdapter;

    #[test]
    fn scheduling_family_is_production_admitted() {
        for operation in [
            "sched_getaffinity",
            "sched_getattr",
            "sched_getparam",
            "sched_get_priority_max",
            "sched_get_priority_min",
            "sched_getscheduler",
            "sched_rr_get_interval",
            "sched_setaffinity",
            "sched_setattr",
            "sched_setparam",
            "sched_setscheduler",
        ] {
            assert!(SUPPORTED.contains(&operation), "{operation} must be admitted");
        }
    }

    const PAGE: usize = 4096;

    struct SliceInput {
        bytes: Vec<u8>,
        offset: usize,
    }

    impl Read for SliceInput {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            let length = output.len().min(self.bytes.len() - self.offset);
            output[..length].copy_from_slice(&self.bytes[self.offset..self.offset + length]);
            self.offset += length;
            Ok(length)
        }

        fn read_vectored(&mut self, outputs: &mut [IoSliceMut<'_>]) -> std::io::Result<usize> {
            let mut total = 0;
            for output in outputs {
                let read = self.read(output)?;
                total += read;
                if read != output.len() {
                    break;
                }
            }
            Ok(total)
        }
    }

    fn memory() -> Arc<VirtualMemory> {
        let memory = Arc::new(VirtualMemory::reserve(PAGE).unwrap());
        let host = MappingHostAdapter::new(Arc::clone(&memory));
        let request = MapRequest {
            placement: Placement::Fixed(GuestAddress::new(0)),
            length: PAGE as u64,
            alignment: PAGE as u64,
            protection: Protection::READ.union(Protection::WRITE),
            backing: Backing::Anonymous {
                identity: 1,
                shared: false,
            },
            backing_offset: 0,
        };
        let token = host.stage_map(GuestAddress::new(0), request).unwrap();
        host.commit(&[token]).unwrap();
        memory
    }

    fn operation(name: &'static str) -> SyscallOperation {
        SyscallOperation {
            canonical_number: 0,
            name,
            family: SyscallFamily::DescriptorIo,
        }
    }

    fn close() -> SyscallOperation {
        operation("close")
    }

    #[test]
    fn descriptors_are_isolated() {
        let first_memory = Arc::new(VirtualMemory::reserve(4096).unwrap());
        let second_memory = Arc::new(VirtualMemory::reserve(4096).unwrap());
        let mut first = DescriptorPort::new(
            first_memory,
            Arc::new(Set::new().unwrap()),
            Arc::new(crate::ffi::linux::execution::image_data::Entropy),
        );
        let mut second = DescriptorPort::new(
            second_memory,
            Arc::new(Set::new().unwrap()),
            Arc::new(crate::ffi::linux::execution::image_data::Entropy),
        );
        assert_eq!(first.handle(close(), [1, 0, 0, 0, 0, 0]), LinuxResult::Value(0),);
        assert_eq!(
            first.handle(close(), [1, 0, 0, 0, 0, 0]),
            LinuxResult::Error(Errno::EBADF),
        );
        assert_eq!(second.handle(close(), [1, 0, 0, 0, 0, 0]), LinuxResult::Value(0),);
    }

    #[test]
    fn aliases_share_ofd() {
        let memory = memory();
        let input = SliceInput {
            bytes: b"alias".to_vec(),
            offset: 0,
        };
        let mut port = DescriptorPort::with_input(Arc::clone(&memory), Box::new(input));
        assert_eq!(port.handle(operation("dup"), [0, 0, 0, 0, 0, 0]), LinuxResult::Value(3),);
        let original = port.descriptors.snapshot(0).unwrap();
        let alias = port.descriptors.snapshot(3).unwrap();
        assert_eq!(original.description_identity, alias.description_identity);
        assert_eq!(original.descriptor_references, 2);
        assert_eq!(port.handle(close(), [0, 0, 0, 0, 0, 0]), LinuxResult::Value(0),);
        assert_eq!(
            port.handle(operation("read"), [3, 128, 5, 0, 0, 0]),
            LinuxResult::Value(5),
        );
        let mut observed = [0; 5];
        memory.read(128, &mut observed).unwrap();
        assert_eq!(&observed, b"alias");
    }

    #[test]
    fn flags_remain_local() {
        let memory = memory();
        let mut port = DescriptorPort::with_input(
            memory,
            Box::new(SliceInput {
                bytes: Vec::new(),
                offset: 0,
            }),
        );
        assert_eq!(
            port.handle(operation("fcntl"), [0, 1030, 5, 0, 0, 0]),
            LinuxResult::Value(5),
        );
        assert_eq!(port.descriptors.flags(0).unwrap().bits(), 0);
        assert_eq!(port.descriptors.flags(5).unwrap().bits(), 1);
        assert_eq!(
            port.handle(operation("fcntl"), [0, 2, 1, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        assert_eq!(port.descriptors.flags(0).unwrap().bits(), 1);
        assert_eq!(port.descriptors.flags(5).unwrap().bits(), 1);
        assert_eq!(
            port.handle(operation("dup2"), [5, 5, 0, 0, 0, 0]),
            LinuxResult::Value(5),
        );
        assert_eq!(
            port.handle(operation("dup3"), [5, 5, 0, 0, 0, 0]),
            LinuxResult::Error(Errno::EINVAL),
        );
    }

    #[test]
    fn status_flags_are_shared_by_description_aliases() {
        let memory = memory();
        let mut port = DescriptorPort::with_input(
            memory,
            Box::new(SliceInput {
                bytes: Vec::new(),
                offset: 0,
            }),
        );
        assert_eq!(port.handle(operation("dup"), [0, 0, 0, 0, 0, 0]), LinuxResult::Value(3));
        assert_eq!(
            port.handle(operation("fcntl"), [0, 3, 0, 0, 0, 0]),
            LinuxResult::Value(0)
        );
        assert_eq!(
            port.handle(operation("fcntl"), [0, 4, u64::from(StatusFlags::NONBLOCKING), 0, 0, 0]),
            LinuxResult::Value(0),
        );
        assert_eq!(
            port.handle(operation("fcntl"), [3, 3, 0, 0, 0, 0]),
            LinuxResult::Value(u64::from(StatusFlags::NONBLOCKING)),
        );
    }

    #[test]
    fn descriptor_errno_order() {
        let mut port = DescriptorPort::new(
            Arc::new(VirtualMemory::reserve(4096).unwrap()),
            Arc::new(Set::new().unwrap()),
            Arc::new(crate::ffi::linux::execution::image_data::Entropy),
        );
        assert_eq!(
            port.handle(operation("fcntl"), [99, u64::MAX, 0, 0, 0, 0]),
            LinuxResult::Error(Errno::EBADF),
        );
        assert_eq!(
            port.handle(operation("fcntl"), [0, u64::MAX, 0, 0, 0, 0]),
            LinuxResult::Error(Errno::EINVAL),
        );
        assert_eq!(
            port.handle(operation("dup3"), [99, 7, 1, 0, 0, 0]),
            LinuxResult::Error(Errno::EINVAL),
        );
        assert_eq!(
            port.handle(operation("dup"), [99, 0, 0, 0, 0, 0]),
            LinuxResult::Error(Errno::EBADF),
        );
    }

    #[test]
    fn partial_read() {
        let memory = memory();
        let input = SliceInput {
            bytes: b"abc".to_vec(),
            offset: 0,
        };
        let mut port = DescriptorPort::with_input(Arc::clone(&memory), Box::new(input));
        assert_eq!(
            port.handle(operation("read"), [0, 128, 8, 0, 0, 0]),
            LinuxResult::Value(3),
        );
        let mut observed = [0; 4];
        memory.read(128, &mut observed).unwrap();
        assert_eq!(&observed, b"abc\0");
    }

    #[test]
    fn readv_preflights_copyout() {
        let memory = memory();
        let input = SliceInput {
            bytes: b"abcdef".to_vec(),
            offset: 0,
        };
        let mut port = DescriptorPort::with_input(Arc::clone(&memory), Box::new(input));
        let mut records = [0_u8; 32];
        records[..8].copy_from_slice(&256_u64.to_le_bytes());
        records[8..16].copy_from_slice(&3_u64.to_le_bytes());
        records[16..24].copy_from_slice(&8192_u64.to_le_bytes());
        records[24..].copy_from_slice(&3_u64.to_le_bytes());
        memory.write(64, &records).unwrap();
        assert_eq!(
            port.handle(operation("readv"), [0, 64, 2, 0, 0, 0]),
            LinuxResult::Error(Errno::EFAULT),
        );

        records[16..24].copy_from_slice(&320_u64.to_le_bytes());
        memory.write(64, &records).unwrap();
        assert_eq!(
            port.handle(operation("readv"), [0, 64, 2, 0, 0, 0]),
            LinuxResult::Value(6),
        );
        let mut first = [0; 3];
        let mut second = [0; 3];
        memory.read(256, &mut first).unwrap();
        memory.read(320, &mut second).unwrap();
        assert_eq!(&first, b"abc");
        assert_eq!(&second, b"def");
    }
}
