use std::os::fd::RawFd;
use std::sync::Arc;

use hl_isa::GuestAddress;
use hl_memory::{ExternalSpan, Protection};
use hl_runtime::{VectorDirection, VectorError, VectorPosition, VectorRequest, VectorTerminal};

use super::path::{FileIntent, FileOperation, FileTransferRegistry};
use super::process_memory::ProcessMemory;
use super::space::SpaceLease;

const HOST_VECTOR_MAXIMUM: usize = 1024;
const RWF_APPEND: u32 = 0x10;
const RWF_NOAPPEND: u32 = 0x20;

/// Linux application adapter joining one admitted address space and native OFD.
pub(super) struct VectorAdapter {
    memory: ProcessMemory,
    files: Arc<FileTransferRegistry>,
}

struct Projection {
    lease: SpaceLease,
    spans: Vec<ExternalSpan>,
    faulted: bool,
    total: u64,
}

impl VectorAdapter {
    pub(super) fn new(memory: ProcessMemory, files: Arc<FileTransferRegistry>) -> Self {
        Self { memory, files }
    }

    fn project(&self, request: VectorRequest<'_>) -> Result<Projection, VectorError> {
        let lease = self.memory.lease();
        let mappings = lease.mappings();
        let protection = match request.direction {
            VectorDirection::Read => Protection::WRITE,
            VectorDirection::Write => Protection::READ,
        };
        let mut spans = Vec::with_capacity(request.vectors.len().min(HOST_VECTOR_MAXIMUM));
        let mut total = 0_u64;
        for vector in request.vectors {
            let mut address = vector.base;
            let mut remaining = vector.length;
            while remaining != 0 {
                if spans.len() == HOST_VECTOR_MAXIMUM {
                    return Ok(Projection {
                        lease,
                        spans,
                        faulted: false,
                        total,
                    });
                }
                let available = match mappings.access_prefix(GuestAddress::new(address), remaining, protection) {
                    Ok(0) | Err(_) => {
                        return Ok(Projection {
                            lease,
                            spans,
                            faulted: true,
                            total,
                        });
                    }
                    Ok(available) => available.min(remaining),
                };
                spans.push(ExternalSpan {
                    address: GuestAddress::new(address),
                    length: available,
                });
                total = total.checked_add(available).ok_or(VectorError::Fault)?;
                address = address.checked_add(available).ok_or(VectorError::Fault)?;
                remaining -= available;
            }
        }
        Ok(Projection {
            lease,
            spans,
            faulted: false,
            total,
        })
    }

    fn file_request(
        descriptor: &hl_descriptor::OperationLease,
        request: VectorRequest<'_>,
        total: u64,
    ) -> FileOperation {
        let flags = request.flags.unwrap_or(0);
        let status_append = descriptor.status().bits() & hl_descriptor::StatusFlags::APPEND != 0;
        FileOperation {
            intent: match request.direction {
                VectorDirection::Read => FileIntent::Read,
                VectorDirection::Write => FileIntent::Write,
            },
            position: match request.position {
                VectorPosition::Shared => None,
                VectorPosition::At(offset) => Some(offset),
            },
            append: request.direction == VectorDirection::Write
                && (flags & RWF_APPEND != 0 || status_append && flags & RWF_NOAPPEND == 0),
            total,
        }
    }

    fn call(
        descriptor: RawFd,
        request: VectorRequest<'_>,
        lease: &SpaceLease,
        spans: &[ExternalSpan],
    ) -> Result<usize, VectorError> {
        let arena = lease.arena();
        let mut vectors = Vec::with_capacity(spans.len().max(1));
        for span in spans {
            let (address, length) = arena
                .host_range(span.address.get(), span.length)
                .map_err(|_| VectorError::Fault)?;
            vectors.push(libc::iovec {
                iov_base: address,
                iov_len: length,
            });
        }
        if vectors.is_empty() {
            vectors.push(libc::iovec {
                iov_base: std::ptr::NonNull::<u8>::dangling().as_ptr().cast(),
                iov_len: 0,
            });
        }
        let count = i32::try_from(vectors.len()).map_err(|_| VectorError::Errno(hl_linux::Errno::EINVAL))?;
        let offset = match request.position {
            VectorPosition::Shared => -1,
            VectorPosition::At(offset) => {
                i64::try_from(offset).map_err(|_| VectorError::Errno(hl_linux::Errno::EINVAL))?
            }
        };
        #[cfg(not(target_os = "linux"))]
        let offset = if request.direction == VectorDirection::Write
            && request.flags.is_some_and(|flags| flags & RWF_APPEND != 0)
        {
            // SAFETY: status is uniquely writable, descriptor stays live and
            // locked, fstat retains no pointer, and cannot unwind.
            let mut status = unsafe { std::mem::zeroed::<libc::stat>() };
            // SAFETY: status points to a valid libc::stat for the duration of
            // this call and descriptor remains live and locked.
            if unsafe { libc::fstat(descriptor, std::ptr::from_mut(&mut status)) } < 0 {
                return Err(Self::last_errno());
            }
            status.st_size
        } else {
            offset
        };
        // SAFETY: every nonempty iovec comes from a live, direction-validated
        // SpaceLease; the descriptor stays owned and locked, vectors outlive
        // this non-retaining libc call, and zero-length sentinels are not read.
        #[cfg(target_os = "linux")]
        // SAFETY: `vectors` outlives the call and holds `count` iovecs backed by live,
        // direction-validated SpaceLeases; the descriptor stays owned and locked and libc
        // retains no pointer past return.
        let result = unsafe {
            match (request.direction, request.flags, request.position) {
                (VectorDirection::Read, Some(flags), _) => {
                    libc::preadv2(descriptor, vectors.as_ptr(), count, offset, flags as i32)
                }
                (VectorDirection::Write, Some(flags), _) => {
                    libc::pwritev2(descriptor, vectors.as_ptr(), count, offset, flags as i32)
                }
                (VectorDirection::Read, None, VectorPosition::Shared) => {
                    libc::readv(descriptor, vectors.as_ptr(), count)
                }
                (VectorDirection::Write, None, VectorPosition::Shared) => {
                    libc::writev(descriptor, vectors.as_ptr(), count)
                }
                (VectorDirection::Read, None, VectorPosition::At(_)) => {
                    libc::preadv(descriptor, vectors.as_ptr(), count, offset)
                }
                (VectorDirection::Write, None, VectorPosition::At(_)) => {
                    libc::pwritev(descriptor, vectors.as_ptr(), count, offset)
                }
            }
        };
        // Non-Linux hosts lack preadv2/pwritev2. Validation admits only flag
        // zero and write-side RWF_APPEND; the latter uses the EOF offset above.
        #[cfg(not(target_os = "linux"))]
        // SAFETY: same invariant as the Linux arm — `vectors` outlives the call with
        // `count` live lease-backed iovecs and the descriptor stays owned and locked.
        let result = unsafe {
            match (request.direction, request.position) {
                (VectorDirection::Read, VectorPosition::Shared) => libc::readv(descriptor, vectors.as_ptr(), count),
                (VectorDirection::Write, VectorPosition::Shared) => libc::writev(descriptor, vectors.as_ptr(), count),
                (VectorDirection::Read, VectorPosition::At(_)) => {
                    libc::preadv(descriptor, vectors.as_ptr(), count, offset)
                }
                (VectorDirection::Write, VectorPosition::At(_)) => {
                    libc::pwritev(descriptor, vectors.as_ptr(), count, offset)
                }
            }
        };
        if result < 0 {
            let raw = std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO);
            Err(VectorError::Errno(hl_linux::Errno::from_host(
                hl_linux::Errno::from_raw(raw),
            )))
        } else {
            usize::try_from(result).map_err(|_| VectorError::Errno(hl_linux::Errno::EIO))
        }
    }

    fn first_fault(descriptor: RawFd, request: VectorRequest<'_>) -> Result<usize, VectorError> {
        match request.direction {
            VectorDirection::Write => {
                // SAFETY: F_GETFL observes one locked, live descriptor and
                // retains no pointer or state.
                let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
                if flags < 0 || flags & libc::O_ACCMODE == libc::O_RDONLY {
                    return Err(VectorError::Errno(hl_linux::Errno::EBADF));
                }
                Err(VectorError::Fault)
            }
            VectorDirection::Read => {
                // SAFETY: F_GETFL observes one locked, live descriptor and
                // retains no pointer or state.
                let file_flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
                if file_flags < 0 || file_flags & libc::O_ACCMODE == libc::O_WRONLY {
                    return Err(VectorError::Errno(hl_linux::Errno::EBADF));
                }
                let offset = match request.position {
                    VectorPosition::At(offset) => {
                        Some(i64::try_from(offset).map_err(|_| VectorError::Errno(hl_linux::Errno::EINVAL))?)
                    }
                    VectorPosition::Shared => {
                        // SAFETY: lseek observes the locked, live descriptor and
                        // retains no pointer or state.
                        let current = unsafe { libc::lseek(descriptor, 0, libc::SEEK_CUR) };
                        (current >= 0).then_some(current)
                    }
                };
                if let Some(offset) = offset {
                    let mut byte = 0_u8;
                    // SAFETY: byte is uniquely writable, the descriptor is
                    // locked and live, pread does not change its shared offset,
                    // retains no pointer, and cannot unwind.
                    let result = unsafe { libc::pread(descriptor, std::ptr::from_mut(&mut byte).cast(), 1, offset) };
                    return match result {
                        0 => Ok(0),
                        value if value > 0 => Err(VectorError::Fault),
                        _ => Err(Self::last_errno()),
                    };
                }
                let mut ready = libc::pollfd {
                    fd: descriptor,
                    events: libc::POLLIN | libc::POLLRDHUP,
                    revents: 0,
                };
                // SAFETY: ready is uniquely writable for one pollfd, the
                // descriptor is live, the zero timeout cannot block, no pointer
                // is retained, and poll cannot unwind.
                let polled = unsafe { libc::poll(std::ptr::from_mut(&mut ready), 1, 0) };
                if polled < 0 {
                    return Err(Self::last_errno());
                }
                if polled > 0 && ready.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLRDHUP) != 0 {
                    let mut pending = 0_i32;
                    // SAFETY: pending is uniquely writable, FIONREAD observes
                    // the live descriptor, retains no pointer, and cannot unwind.
                    let counted = unsafe { libc::ioctl(descriptor, libc::FIONREAD, std::ptr::from_mut(&mut pending)) };
                    if (counted == 0 && pending == 0) || (counted != 0 && ready.revents & libc::POLLHUP != 0) {
                        return Ok(0);
                    }
                } else if polled == 0 && file_flags & libc::O_NONBLOCK != 0 {
                    return Err(VectorError::Errno(hl_linux::Errno::EAGAIN));
                }
                Err(VectorError::Fault)
            }
        }
    }

    fn last_errno() -> VectorError {
        let raw = std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO);
        VectorError::Errno(hl_linux::Errno::from_host(hl_linux::Errno::from_raw(raw)))
    }
}

impl VectorTerminal for VectorAdapter {
    fn execute(
        &self,
        descriptor: &hl_descriptor::OperationLease,
        request: VectorRequest<'_>,
    ) -> Result<usize, VectorError> {
        let identity = descriptor.description_identity();
        if !self.files.supports(identity) {
            return Err(VectorError::Unsupported);
        }
        #[cfg(not(target_os = "linux"))]
        if let Some(flags) = request.flags {
            let admitted = request.direction == VectorDirection::Write && flags == RWF_APPEND;
            if flags != 0 && !admitted {
                return Err(VectorError::Errno(hl_linux::Errno::EOPNOTSUPP));
            }
        }
        let projection = self.project(request)?;
        let file_request = Self::file_request(descriptor, request, projection.total);
        if projection.faulted && projection.spans.is_empty() {
            return self.files.operate(
                identity,
                FileOperation {
                    intent: FileIntent::Probe,
                    ..file_request
                },
                |native| Self::first_fault(native, request),
            );
        }
        let operation = || {
            self.files.operate(identity, file_request, |native| {
                Self::call(native, request, &projection.lease, &projection.spans)
            })
        };
        let mappings = projection.lease.mappings();

        match request.direction {
            VectorDirection::Read => mappings.write_vectors(&projection.spans, operation),
            VectorDirection::Write => mappings.read_vectors(&projection.spans, operation),
        }
        .map_err(|_| VectorError::Fault)?
    }
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::io::{Seek, Write};
    use std::os::fd::OwnedFd;
    use std::os::unix::fs::FileExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    use hl_descriptor::DescriptorTable;
    use hl_linux::GuestIovec;
    use hl_memory::{Backing, MapRequest, MappingCoordinator, Placement};

    use super::*;
    use crate::ffi::linux::{MappingHostAdapter, VirtualMemory};

    const PAGE: usize = 4096;
    static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

    struct Harness {
        adapter: VectorAdapter,
        arena: Arc<VirtualMemory>,
        table: DescriptorTable,
        descriptor: i32,
        observer: std::fs::File,
    }

    fn native_file(bytes: &[u8], writable: bool) -> (std::fs::File, std::fs::File) {
        let sequence = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("hl-vector-{}-{sequence}", std::process::id()));
        let mut creator = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        creator.write_all(bytes).unwrap();
        creator.rewind().unwrap();
        let operation = if writable {
            creator.try_clone().unwrap()
        } else {
            OpenOptions::new().read(true).open(&path).unwrap()
        };
        std::fs::remove_file(path).unwrap();
        (operation, creator)
    }

    fn harness(bytes: &[u8], writable: bool) -> Harness {
        let arena = Arc::new(VirtualMemory::reserve(PAGE).unwrap());
        let mappings = Arc::new(MappingCoordinator::new(MappingHostAdapter::new(Arc::clone(&arena))));
        mappings
            .map(MapRequest {
                placement: Placement::Fixed(GuestAddress::new(0)),
                length: PAGE as u64,
                alignment: PAGE as u64,
                protection: Protection::READ.union(Protection::WRITE),
                backing: Backing::Anonymous {
                    identity: 1,
                    shared: false,
                },
                backing_offset: 0,
            })
            .unwrap();
        let memory = ProcessMemory::new(super::super::space::AddressSpace::new(arena.clone(), mappings));
        let files = Arc::new(FileTransferRegistry::default());
        let (operation, observer) = native_file(bytes, writable);
        let table = DescriptorTable::new(4).unwrap();
        let transfer = files.import(OwnedFd::from(operation)).unwrap();
        let descriptor = transfer
            .prepare(&table, true)
            .unwrap()
            .publish_after(|_| Ok::<_, ()>(()))
            .unwrap()[0];
        Harness {
            adapter: VectorAdapter::new(memory, files),
            arena,
            table,
            descriptor,
            observer,
        }
    }

    #[test]
    fn vectors_preserve_positions() {
        let mut write = harness(&[], true);
        write.arena.write(16, b"ab").unwrap();
        write.arena.write(64, b"cde").unwrap();
        let vectors = [GuestIovec { base: 16, length: 2 }, GuestIovec { base: 64, length: 3 }];
        let lease = write.table.pin(write.descriptor).unwrap();
        assert_eq!(
            write.adapter.execute(
                &lease,
                VectorRequest {
                    vectors: &vectors,
                    direction: VectorDirection::Write,
                    position: VectorPosition::Shared,
                    flags: None,
                },
            ),
            Ok(5),
        );
        let mut observed = [0_u8; 5];
        assert_eq!(write.observer.read_at(&mut observed, 0).unwrap(), 5);
        assert_eq!(&observed, b"abcde");
        assert_eq!(write.observer.stream_position().unwrap(), 5);

        let mut read = harness(b"abcdef", true);
        let lease = read.table.pin(read.descriptor).unwrap();
        assert_eq!(
            read.adapter.execute(
                &lease,
                VectorRequest {
                    vectors: &vectors,
                    direction: VectorDirection::Read,
                    position: VectorPosition::At(1),
                    flags: None,
                },
            ),
            Ok(5),
        );
        let mut first = [0_u8; 2];
        let mut second = [0_u8; 3];
        read.arena.read(16, &mut first).unwrap();
        read.arena.read(64, &mut second).unwrap();
        assert_eq!((&first, &second), (b"bc", b"def"));
        assert_eq!(read.observer.stream_position().unwrap(), 0);
    }

    #[test]
    fn first_fault_precedence() {
        let prefix = harness(&[], true);
        prefix.arena.write(16, b"ab").unwrap();
        let partial = [
            GuestIovec { base: 16, length: 2 },
            GuestIovec {
                base: PAGE as u64,
                length: 1,
            },
        ];
        let lease = prefix.table.pin(prefix.descriptor).unwrap();
        assert_eq!(
            prefix.adapter.execute(
                &lease,
                VectorRequest {
                    vectors: &partial,
                    direction: VectorDirection::Write,
                    position: VectorPosition::Shared,
                    flags: None,
                },
            ),
            Ok(2),
        );
        let mut observed = [0_u8; 2];
        assert_eq!(prefix.observer.read_at(&mut observed, 0).unwrap(), 2);
        assert_eq!(&observed, b"ab");

        let vectors = [GuestIovec {
            base: PAGE as u64,
            length: 1,
        }];
        let empty = harness(&[], true);
        let lease = empty.table.pin(empty.descriptor).unwrap();
        assert_eq!(
            empty.adapter.execute(
                &lease,
                VectorRequest {
                    vectors: &vectors,
                    direction: VectorDirection::Read,
                    position: VectorPosition::Shared,
                    flags: None,
                },
            ),
            Ok(0),
        );

        let read_only = harness(b"data", false);
        let lease = read_only.table.pin(read_only.descriptor).unwrap();
        assert_eq!(
            read_only.adapter.execute(
                &lease,
                VectorRequest {
                    vectors: &vectors,
                    direction: VectorDirection::Write,
                    position: VectorPosition::Shared,
                    flags: None,
                },
            ),
            Err(VectorError::Errno(hl_linux::Errno::EBADF)),
        );
    }

    #[test]
    fn rwf_append_uses_eof() {
        let append = harness(b"head", true);
        append.arena.write(16, b"tail").unwrap();
        let vectors = [GuestIovec { base: 16, length: 4 }];
        let lease = append.table.pin(append.descriptor).unwrap();
        assert_eq!(
            append.adapter.execute(
                &lease,
                VectorRequest {
                    vectors: &vectors,
                    direction: VectorDirection::Write,
                    position: VectorPosition::At(0),
                    flags: Some(RWF_APPEND),
                },
            ),
            Ok(4),
        );
        let mut observed = [0_u8; 8];
        assert_eq!(append.observer.read_at(&mut observed, 0).unwrap(), 8);
        assert_eq!(&observed, b"headtail");
    }
}
