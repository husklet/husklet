use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use hl_descriptor::{
    DirectoryBatch, DirectoryBatchToken, LinkableInode, ObjectError, OfdMetadata, OfdTimestamp, OpenFileDescription,
    PreparedSpliceRead, Readiness, SeekPosition,
};

use super::{directory, lease, metadata, mutation, tmpfs, watch};

pub(in crate::ffi::linux::execution) struct NativeFile {
    pub(super) file: Mutex<Option<File>>,
    pub(super) directory: Mutex<Option<directory::State>>,
    pub(super) watches: Arc<watch::Hub>,
    path: PathBuf,
    link_target: Mutex<Option<Vec<u8>>>,
    pub(super) path_only: AtomicBool,
    pub(super) anonymous_exclusive: AtomicBool,
    /// Hidden name standing in for an `O_TMPFILE` inode the host filesystem cannot create anonymously.
    #[cfg(target_os = "linux")]
    pub(super) anonymous_name: super::pin::AnonymousSlot,
    pub(super) writes: lease::Registry,
    pub(super) write_lease: Mutex<Option<lease::WriteLease>>,
    ownership: Arc<metadata::Registry>,
    pub(super) shm_budget: Option<Arc<tmpfs::Budget>>,
    pub(super) shm_lease: Mutex<Option<tmpfs::Lease>>,
    splice_gate: Arc<SpliceGate>,
}

#[derive(Default)]
struct SpliceGate {
    reserved: Mutex<bool>,
    changed: Condvar,
}

struct SpliceCancellationWake(Arc<SpliceGate>);

impl hl_descriptor::CancellationNotification for SpliceCancellationWake {
    fn notify(&self) {
        let _reserved = self
            .0
            .reserved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.0.changed.notify_all();
    }
}

struct PreparedNativeSpliceRead {
    cursor: Option<File>,
    gate: Arc<SpliceGate>,
    start: u64,
    bytes: Vec<u8>,
}

impl NativeFile {
    pub(in crate::ffi::linux::execution) fn subscribe_dnotify(
        &self,
        mask: u32,
        callback: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Box<dyn hl_descriptor::ReadinessSubscription>, ObjectError> {
        if self
            .directory
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none()
        {
            return Err(ObjectError::InvalidArgument);
        }
        Ok(self.watches.subscribe_dnotify(self.path.clone(), mask, callback))
    }

    #[cfg(target_os = "linux")]
    fn anonymous(&self) -> bool {
        self.anonymous_name.present()
    }

    #[cfg(not(target_os = "linux"))]
    const fn anonymous(&self) -> bool {
        false
    }

    pub(super) fn xattr_file(&self) -> Result<File, ObjectError> {
        self.io()?;
        self.file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .ok_or(ObjectError::BadDescriptor)?
            .try_clone()
            .map_err(Self::object)
    }

    pub(super) fn new(
        watches: Arc<watch::Hub>,
        path: PathBuf,
        writes: Arc<Mutex<BTreeMap<(u64, u64), usize>>>,
        ownership: Arc<metadata::Registry>,
        shm_budget: Option<Arc<tmpfs::Budget>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            file: Mutex::new(None),
            directory: Mutex::new(None),
            watches,
            path,
            link_target: Mutex::new(None),
            path_only: AtomicBool::new(false),
            anonymous_exclusive: AtomicBool::new(false),
            #[cfg(target_os = "linux")]
            anonymous_name: super::pin::AnonymousSlot::default(),
            writes,
            write_lease: Mutex::new(None),
            ownership,
            shm_budget,
            shm_lease: Mutex::new(None),
            splice_gate: Arc::new(SpliceGate::default()),
        })
    }

    pub(super) fn ownership(&self) -> &Arc<metadata::Registry> {
        &self.ownership
    }

    pub(super) fn modified(&self, result: Result<usize, ObjectError>) -> Result<usize, ObjectError> {
        if result.as_ref().is_ok_and(|count| *count != 0) {
            self.watches.publish(&self.path, hl_event::InotifyMask::MODIFY);
        }
        result
    }

    pub(super) fn object(error: std::io::Error) -> ObjectError {
        match error.kind() {
            std::io::ErrorKind::PermissionDenied => ObjectError::PermissionDenied,
            std::io::ErrorKind::WouldBlock => ObjectError::WouldBlock,
            std::io::ErrorKind::Interrupted => ObjectError::Interrupted,
            _ => ObjectError::Io,
        }
    }

    pub(super) fn io(&self) -> Result<(), ObjectError> {
        if self.path_only.load(Ordering::Acquire) {
            Err(ObjectError::BadDescriptor)
        } else {
            Ok(())
        }
    }

    pub(super) fn publish_modified(&self, count: usize) {
        if count != 0 {
            self.watches.publish(&self.path, hl_event::InotifyMask::MODIFY);
        }
    }

    pub(super) fn read_link(&self) -> Result<Vec<u8>, super::RuntimePathError> {
        self.link_target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .ok_or(super::RuntimePathError::Invalid)
    }

    #[cfg(target_os = "linux")]
    fn write_zeros(file: &std::fs::File, start: u64, end: u64) -> Result<(), ObjectError> {
        let zeros = [0_u8; 65_536];
        let mut position = start;
        while position < end {
            let count =
                usize::try_from((end - position).min(zeros.len() as u64)).map_err(|_| ObjectError::ResourceLimit)?;
            let written = file.write_at(&zeros[..count], position).map_err(Self::object)?;
            if written == 0 {
                return Err(ObjectError::Io);
            }
            position += written as u64;
        }
        Ok(())
    }

    /// Waits one backoff step for a contended advisory lock.
    fn flock_wait(
        nonblocking: bool,
        cancellation: &dyn hl_descriptor::OperationCancellation,
    ) -> Result<(), ObjectError> {
        if nonblocking {
            return Err(ObjectError::WouldBlock);
        }
        if cancellation.interrupted() {
            return Err(ObjectError::Interrupted);
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
        Ok(())
    }

    pub(super) fn retain_link(&self, target: Option<Vec<u8>>) {
        *self
            .link_target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = target;
    }
}

impl fmt::Debug for NativeFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("NativeFile")
    }
}

impl Drop for NativeFile {
    fn drop(&mut self) {
        // The hidden name is removed as this object drops, so the inode retains no link.
        let links = if self.anonymous() {
            0
        } else {
            self.file
                .get_mut()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .and_then(|file| file.metadata().ok())
                .map_or(0, |value| value.nlink())
        };
        if let Some(lease) = self
            .shm_lease
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            lease.close(links);
        }
    }
}

impl PreparedSpliceRead for PreparedNativeSpliceRead {
    fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    fn commit(mut self: Box<Self>, count: usize) -> Result<(), ObjectError> {
        if count > self.bytes.len() {
            return Err(ObjectError::InvalidArgument);
        }
        if let Some(cursor) = self.cursor.as_mut() {
            if !*self
                .gate
                .reserved
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
            {
                return Err(ObjectError::Interrupted);
            }
            let end = self
                .start
                .checked_add(count as u64)
                .ok_or(ObjectError::InvalidArgument)?;
            let result = cursor.seek(SeekFrom::Start(end)).map_err(NativeFile::object);
            *self
                .gate
                .reserved
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = false;
            self.gate.changed.notify_all();
            self.cursor = None;
            result?;
        }
        Ok(())
    }
}

impl Drop for PreparedNativeSpliceRead {
    fn drop(&mut self) {
        if self.cursor.is_some() {
            *self
                .gate
                .reserved
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = false;
            self.gate.changed.notify_all();
        }
    }
}

impl OpenFileDescription for NativeFile {
    fn readiness(&self, interests: Readiness) -> Readiness {
        if self.path_only.load(Ordering::Acquire) {
            return Readiness::from_bits(Readiness::ERROR);
        }
        Readiness::from_bits(interests.bits() & (Readiness::READ | Readiness::WRITE))
    }

    fn domain_extension(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn copy_file_range(
        &self,
        target: &dyn OpenFileDescription,
        input_offset: Option<u64>,
        output_offset: Option<u64>,
        maximum: usize,
        _nonblocking: bool,
        cancellation: Option<&dyn hl_descriptor::OperationCancellation>,
    ) -> Result<Option<(usize, u64, u64)>, ObjectError> {
        self.io()?;
        let target = target
            .domain_extension()
            .and_then(|extension| extension.downcast_ref::<Self>())
            .ok_or(ObjectError::NotSupported)?;
        target.io()?;

        let transfer = |input: &mut Option<File>, output: &mut Option<File>| {
            let input = input.as_mut().ok_or(ObjectError::BadDescriptor)?;
            let output = output.as_mut().ok_or(ObjectError::BadDescriptor)?;
            let input_start = input_offset.map_or_else(|| input.stream_position().map_err(Self::object), Ok)?;
            let output_start = output_offset.map_or_else(|| output.stream_position().map_err(Self::object), Ok)?;
            let input_end = input_start.checked_add(maximum as u64);
            let output_end = output_start.checked_add(maximum as u64);
            if input_end.is_none() || output_end.is_none() {
                return Err(ObjectError::InvalidArgument);
            }
            if maximum != 0 {
                let input_metadata = input.metadata().map_err(Self::object)?;
                let output_metadata = output.metadata().map_err(Self::object)?;
                let same_file =
                    input_metadata.dev() == output_metadata.dev() && input_metadata.ino() == output_metadata.ino();
                let overlaps = input_start < output_end.unwrap() && output_start < input_end.unwrap();
                if same_file && overlaps {
                    return Err(ObjectError::InvalidArgument);
                }
            }

            let mut buffer = vec![0_u8; maximum.min(8192)];
            let mut done = 0;
            while done < maximum {
                let interrupted = cancellation.is_some_and(hl_descriptor::OperationCancellation::interrupted);
                if interrupted && done == 0 {
                    return Err(ObjectError::Interrupted);
                }
                if interrupted {
                    break;
                }
                let chunk = (maximum - done).min(buffer.len());
                let read = match input.read_at(&mut buffer[..chunk], input_start + done as u64) {
                    Ok(0) => break,
                    Ok(count) => count,
                    Err(error) if done == 0 => return Err(Self::object(error)),
                    Err(_) => break,
                };
                let written = match output.write_at(&buffer[..read], output_start + done as u64) {
                    Ok(count) => count.min(read),
                    Err(error) if done == 0 => return Err(Self::object(error)),
                    Err(_) => break,
                };
                done += written;
                if written < read {
                    break;
                }
            }
            if input_offset.is_none() {
                input
                    .seek(SeekFrom::Start(input_start + done as u64))
                    .map_err(Self::object)?;
            }
            if output_offset.is_none() {
                output
                    .seek(SeekFrom::Start(output_start + done as u64))
                    .map_err(Self::object)?;
            }
            Ok((done, input_start, output_start))
        };

        let result = if std::ptr::eq(self, target) {
            let mut input = self.file.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let clone = input
                .as_ref()
                .ok_or(ObjectError::BadDescriptor)?
                .try_clone()
                .map_err(Self::object)?;
            transfer(&mut input, &mut Some(clone))
        } else if (std::ptr::from_ref::<Self>(self) as usize) < (std::ptr::from_ref::<Self>(target) as usize) {
            let mut input = self.file.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut output = target.file.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            transfer(&mut input, &mut output)
        } else {
            let mut output = target.file.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut input = self.file.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            transfer(&mut input, &mut output)
        }?;
        target.publish_modified(result.0);
        Ok(Some(result))
    }

    fn kind(&self) -> hl_descriptor::ObjectKind {
        if self
            .directory
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
        {
            hl_descriptor::ObjectKind::Directory
        } else {
            hl_descriptor::ObjectKind::File
        }
    }

    fn linkable_inode(&self) -> Result<Option<Arc<dyn LinkableInode>>, ObjectError> {
        let file = self
            .file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .ok_or(ObjectError::BadDescriptor)?
            .try_clone()
            .map_err(Self::object)?;
        Ok(Some(Arc::new(mutation::NativeLink::new(
            file,
            self.anonymous_exclusive.load(Ordering::Acquire),
            #[cfg(target_os = "linux")]
            self.anonymous_name.clone(),
        ))))
    }

    fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
        self.io()?;
        let mut reserved = self
            .splice_gate
            .reserved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *reserved {
            reserved = self
                .splice_gate
                .changed
                .wait(reserved)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        let mut opened = self.file.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        drop(reserved);
        opened
            .as_mut()
            .ok_or(ObjectError::BadDescriptor)?
            .read(output)
            .map_err(Self::object)
    }

    fn write(&self, input: &[u8]) -> Result<usize, ObjectError> {
        self.io()?;
        let mut reserved = self
            .splice_gate
            .reserved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *reserved {
            reserved = self
                .splice_gate
                .changed
                .wait(reserved)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        let mut opened = self.file.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        drop(reserved);
        let file = opened.as_mut().ok_or(ObjectError::BadDescriptor)?;
        let lease = self.shm_lease.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let result = if let Some(lease) = lease.as_ref() {
            lease.write(file, input)
        } else {
            file.write(input).map_err(Self::object)
        };
        self.modified(result)
    }

    fn read_at(&self, offset: u64, output: &mut [u8]) -> Result<usize, ObjectError> {
        self.io()?;
        self.file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .ok_or(ObjectError::BadDescriptor)?
            .read_at(output, offset)
            .map_err(Self::object)
    }

    fn write_at(&self, offset: u64, input: &[u8]) -> Result<usize, ObjectError> {
        self.io()?;
        let opened = self.file.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let file = opened.as_ref().ok_or(ObjectError::BadDescriptor)?;
        let lease = self.shm_lease.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let result = lease.as_ref().map_or_else(
            || file.write_at(input, offset).map_err(Self::object),
            |lease| lease.write_at(file, offset, input),
        );
        self.modified(result)
    }

    fn seek(&self, position: SeekPosition) -> Result<u64, ObjectError> {
        self.io()?;
        if let SeekPosition::Start(cookie) = position {
            let mut directory = self.directory.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(directory) = directory.as_mut() {
                return directory.seek(cookie);
            }
        }
        let sparse = match position {
            SeekPosition::Data(value) => Some((value, libc::SEEK_DATA)),
            SeekPosition::Hole(value) => Some((value, libc::SEEK_HOLE)),
            _ => None,
        };
        let position = match position {
            SeekPosition::Start(value) => SeekFrom::Start(value),
            SeekPosition::Current(value) => SeekFrom::Current(value),
            SeekPosition::End(value) => SeekFrom::End(value),
            SeekPosition::Data(_) | SeekPosition::Hole(_) => SeekFrom::Start(0),
        };
        let mut reserved = self
            .splice_gate
            .reserved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *reserved {
            reserved = self
                .splice_gate
                .changed
                .wait(reserved)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        let mut opened = self.file.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        drop(reserved);
        let file = opened.as_mut().ok_or(ObjectError::BadDescriptor)?;
        if let Some((offset, whence)) = sparse {
            let offset = i64::try_from(offset).map_err(|_| ObjectError::NoExtent)?;
            // SAFETY: the file mutex owns a live descriptor for the duration of lseek.
            let result = unsafe { libc::lseek(file.as_raw_fd(), offset, whence) };
            if result < 0 {
                return Err(match std::io::Error::last_os_error().raw_os_error() {
                    Some(libc::ENXIO) => ObjectError::NoExtent,
                    Some(libc::ESPIPE) => ObjectError::InvalidArgument,
                    _ => ObjectError::Io,
                });
            }
            return u64::try_from(result).map_err(|_| ObjectError::NoExtent);
        }
        file.seek(position).map_err(Self::object)
    }

    fn prepare_splice_read(
        &self,
        offset: Option<u64>,
        maximum: usize,
        nonblocking: bool,
        cancellation: Option<&dyn hl_descriptor::OperationCancellation>,
    ) -> Result<Option<Box<dyn PreparedSpliceRead>>, ObjectError> {
        self.io()?;
        // This interface snapshots the source before guest copyout. Keep that
        // transaction bounded; ordinary read(2) requests above the splice
        // chunk must use the streaming file path instead of being reported as
        // an artificial 64 KiB short read. The splice syscall already bounds
        // its request to this same chunk size.
        if maximum > 65_536 {
            return Ok(None);
        }
        if cancellation.is_some_and(hl_descriptor::OperationCancellation::interrupted) {
            return Err(ObjectError::Interrupted);
        }
        let implicit = offset.is_none();
        let subscription =
            cancellation.map(|value| value.subscribe(Arc::new(SpliceCancellationWake(Arc::clone(&self.splice_gate)))));
        let mut reserved = self
            .splice_gate
            .reserved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while implicit && *reserved {
            if nonblocking {
                return Err(ObjectError::WouldBlock);
            }
            if cancellation.is_some_and(hl_descriptor::OperationCancellation::interrupted) {
                return Err(ObjectError::Interrupted);
            }
            reserved = self
                .splice_gate
                .changed
                .wait(reserved)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        if implicit {
            *reserved = true;
        }
        drop(reserved);
        drop(subscription);
        let prepared = (|| {
            let mut opened = self.file.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let file = opened.as_mut().ok_or(ObjectError::BadDescriptor)?;
            let start = offset.map_or_else(|| file.stream_position().map_err(Self::object), Ok)?;
            let mut bytes = vec![0_u8; maximum.min(65_536)];
            let count = file.read_at(&mut bytes, start).map_err(Self::object)?;
            bytes.truncate(count);
            let cursor = implicit.then(|| file.try_clone()).transpose().map_err(Self::object)?;
            Ok(PreparedNativeSpliceRead {
                cursor,
                gate: Arc::clone(&self.splice_gate),
                start,
                bytes,
            })
        })();
        match prepared {
            Ok(prepared) => Ok(Some(Box::new(prepared))),
            Err(error) => {
                if implicit {
                    *self
                        .splice_gate
                        .reserved
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = false;
                    self.splice_gate.changed.notify_all();
                }
                Err(error)
            }
        }
    }

    fn metadata(&self) -> Result<OfdMetadata, ObjectError> {
        let value = self
            .file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .ok_or(ObjectError::BadDescriptor)?
            .metadata()
            .map_err(Self::object)?;
        let timestamp = |seconds, nanoseconds: i64| OfdTimestamp {
            seconds,
            nanoseconds: u32::try_from(nanoseconds).unwrap_or(0),
        };
        let mut projected = OfdMetadata {
            device: value.dev(),
            inode: value.ino(),
            kind: if self
                .link_target
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_some()
            {
                10
            } else if value.is_dir() {
                4
            } else {
                8
            },
            permissions: (value.mode() & 0o7777) as u16,
            // An emulated anonymous inode still carries its hidden name, which the guest must not observe.
            links: if self.anonymous() { 0 } else { value.nlink() },
            user: value.uid(),
            group: value.gid(),
            special_device: value.rdev(),
            size: value.size(),
            blocks_512: value.blocks(),
            accessed: timestamp(value.atime(), value.atime_nsec()),
            modified: timestamp(value.mtime(), value.mtime_nsec()),
            changed: timestamp(value.ctime(), value.ctime_nsec()),
        };
        // An anonymous inode has no name in the image, so only a named file consults the layers.
        let declared = (!self.anonymous()).then_some(self.path.as_path());
        self.ownership.project_ofd_at(declared, &mut projected);
        Ok(projected)
    }

    fn truncate(&self, size: u64) -> Result<(), ObjectError> {
        if self.path_only.load(Ordering::Acquire) {
            return Err(ObjectError::BadDescriptor);
        }
        let opened = self.file.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let file = opened.as_ref().ok_or(ObjectError::BadDescriptor)?;
        let lease = self.shm_lease.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        lease.as_ref().map_or_else(
            || file.set_len(size).map_err(Self::object),
            |lease| lease.truncate(file, size),
        )
    }

    fn add_seals(&self, seals: u8) -> Result<u8, ObjectError> {
        let lease = self.shm_lease.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        lease.as_ref().ok_or(ObjectError::NotSupported)?.add_seals(seals)
    }

    fn seals(&self) -> Result<u8, ObjectError> {
        let lease = self.shm_lease.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        lease.as_ref().ok_or(ObjectError::NotSupported)?.seals()
    }

    fn allocate(&self, request: hl_descriptor::AllocationRequest) -> Result<(), ObjectError> {
        if self.path_only.load(Ordering::Acquire) {
            return Err(ObjectError::BadDescriptor);
        }
        let opened = self.file.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let file = opened.as_ref().ok_or(ObjectError::BadDescriptor)?;
        #[cfg(target_os = "linux")]
        {
            // SAFETY: `file` owns a live descriptor for the duration of the call;
            // integer ranges were validated by the Linux personality. The call
            // neither retains pointers nor crosses an unwind boundary.
            let result = unsafe {
                libc::fallocate(
                    file.as_raw_fd(),
                    request.mode as libc::c_int,
                    request.offset as libc::off_t,
                    request.length as libc::off_t,
                )
            };
            if result == 0 {
                self.watches.publish(&self.path, hl_event::InotifyMask::MODIFY);
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if request.mode & 0x10 != 0
                && matches!(error.raw_os_error(), Some(code) if code == libc::EOPNOTSUPP || code == libc::ENOSYS)
            {
                let old_size = file.metadata().map_err(Self::object)?.len();
                let end = request
                    .offset
                    .checked_add(request.length)
                    .ok_or(ObjectError::ResourceLimit)?;
                if request.mode & 0x01 == 0 && end > old_size {
                    file.set_len(end).map_err(Self::object)?;
                }
                let zero_end = if request.mode & 0x01 != 0 {
                    end.min(old_size)
                } else {
                    end
                };
                Self::write_zeros(file, request.offset, zero_end)?;
                self.watches.publish(&self.path, hl_event::InotifyMask::MODIFY);
                return Ok(());
            }
            match error.raw_os_error() {
                Some(libc::EINVAL) => Err(ObjectError::InvalidArgument),
                Some(libc::ENOSPC) => Err(ObjectError::NoSpace),
                Some(libc::EPERM) => Err(ObjectError::PermissionDenied),
                Some(code) if code == libc::EOPNOTSUPP || code == libc::ENOSYS => Err(ObjectError::NotSupported),
                _ => Err(ObjectError::Io),
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (file, request);
            Err(ObjectError::NotSupported)
        }
    }

    fn flock(
        &self,
        operation: u32,
        cancellation: &dyn hl_descriptor::OperationCancellation,
    ) -> Result<(), ObjectError> {
        let opened = self.file.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let file = opened.as_ref().ok_or(ObjectError::BadDescriptor)?;
        let nonblocking = operation & libc::LOCK_NB as u32 != 0;
        loop {
            // SAFETY: the OFD owns this aligned integer descriptor for the call and libc retains no
            // reference. The file mutex serializes operations using this handle, the descriptor cannot
            // close concurrently, and libc reports failure by errno without unwinding across FFI.
            if unsafe { libc::flock(file.as_raw_fd(), (operation as libc::c_int) | libc::LOCK_NB) } == 0 {
                return Ok(());
            }
            match std::io::Error::last_os_error().raw_os_error() {
                Some(code) if code == libc::EAGAIN || code == libc::EWOULDBLOCK => {
                    Self::flock_wait(nonblocking, cancellation)?;
                }
                Some(libc::EINTR) => return Err(ObjectError::Interrupted),
                Some(libc::EBADF) => return Err(ObjectError::BadDescriptor),
                Some(libc::EINVAL) => return Err(ObjectError::InvalidArgument),
                _ => return Err(ObjectError::Io),
            }
        }
    }

    fn synchronize(&self, data_only: bool) -> Result<(), ObjectError> {
        if self.path_only.load(Ordering::Acquire) {
            return Err(ObjectError::BadDescriptor);
        }
        let opened = self.file.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let file = opened.as_ref().ok_or(ObjectError::BadDescriptor)?;
        if data_only { file.sync_data() } else { file.sync_all() }.map_err(Self::object)
    }

    fn read_directory(&self, maximum: usize) -> Result<DirectoryBatch, ObjectError> {
        self.io()?;
        let mut directory = self.directory.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        directory.as_mut().ok_or(ObjectError::NotSupported)?.read(maximum)
    }

    fn commit_directory(&self, token: DirectoryBatchToken, count: usize) -> Result<(), ObjectError> {
        self.io()?;
        let mut directory = self.directory.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        directory
            .as_mut()
            .ok_or(ObjectError::NotSupported)?
            .commit(token, count)
    }
}
