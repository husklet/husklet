use std::io::{IoSlice, IoSliceMut};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use hl_descriptor::{
    AccessMode, CancellationNotification, CancellationSubscription, DescriptorFlags, DescriptorTable, ObjectError,
    OperationCancellation, OperationContext, Readiness, ReadinessObserver, ReadinessRegistry, ReadinessSubscription,
    StatusFlags,
};

use crate::{
    FileTransfer, GuestPathBytes, Identity, Kind, Metadata, Permissions, SeekPosition, Timestamp, VfsFileDescription,
    VfsFileHost, VfsFileToken,
};

#[derive(Clone)]
struct FakeFileHost {
    inner: Arc<FakeFileInner>,
}

struct FakeFileInner {
    data: Mutex<Vec<u8>>,
    next_read_error: Mutex<Option<ObjectError>>,
    maximum_progress: AtomicUsize,
    blocking: AtomicBool,
    waiting: AtomicBool,
    canceled: AtomicBool,
    wait: Condvar,
    wait_lock: Mutex<()>,
    closes: AtomicUsize,
    registry: ReadinessRegistry,
}

#[derive(Default)]
struct TestCancellation {
    interrupted: AtomicBool,
    notification: Mutex<Option<Arc<dyn CancellationNotification>>>,
    subscribed: Condvar,
}

struct TestSubscription;

impl CancellationSubscription for TestSubscription {}

impl OperationCancellation for TestCancellation {
    fn interrupted(&self) -> bool {
        self.interrupted.load(Ordering::Acquire)
    }

    fn subscribe(&self, notification: Arc<dyn CancellationNotification>) -> Box<dyn CancellationSubscription> {
        *self.notification.lock().unwrap() = Some(notification);
        self.subscribed.notify_all();
        Box::new(TestSubscription)
    }
}

impl TestCancellation {
    fn wait_subscribed(&self) {
        let mut notification = self.notification.lock().unwrap();
        while notification.is_none() {
            notification = self.subscribed.wait(notification).unwrap();
        }
    }

    fn interrupt(&self) {
        self.interrupted.store(true, Ordering::Release);
        if let Some(notification) = self.notification.lock().unwrap().as_ref() {
            notification.notify();
        }
    }
}

impl FakeFileHost {
    fn new(data: &[u8]) -> Self {
        Self {
            inner: Arc::new(FakeFileInner {
                data: Mutex::new(data.to_vec()),
                next_read_error: Mutex::new(None),
                maximum_progress: AtomicUsize::new(usize::MAX),
                blocking: AtomicBool::new(false),
                waiting: AtomicBool::new(false),
                canceled: AtomicBool::new(false),
                wait: Condvar::new(),
                wait_lock: Mutex::new(()),
                closes: AtomicUsize::new(0),
                registry: ReadinessRegistry::new(),
            }),
        }
    }

    fn description(&self, access: AccessMode, status: StatusFlags) -> Arc<VfsFileDescription<Self>> {
        Arc::new(VfsFileDescription::new(
            self.clone(),
            VfsFileToken::from_raw(1),
            Identity { device: 7, inode: 9 },
            GuestPathBytes::new(b"/tmp/\xff").unwrap(),
            access,
            status,
        ))
    }

    fn bytes(&self) -> Vec<u8> {
        self.inner.data.lock().unwrap().clone()
    }

    fn block_reads(&self) {
        self.inner.blocking.store(true, Ordering::Release);
    }
}

impl VfsFileHost for FakeFileHost {
    fn read_at(
        &self,
        _file: VfsFileToken,
        offset: u64,
        output: &mut [u8],
        nonblocking: bool,
    ) -> Result<usize, ObjectError> {
        if let Some(error) = self.inner.next_read_error.lock().unwrap().take() {
            return Err(error);
        }
        if self.inner.blocking.load(Ordering::Acquire) {
            if nonblocking {
                return Err(ObjectError::WouldBlock);
            }
            let mut guard = self.inner.wait_lock.lock().unwrap();
            self.inner.waiting.store(true, Ordering::Release);
            while !self.inner.canceled.load(Ordering::Acquire) {
                guard = self.inner.wait.wait(guard).unwrap();
            }
            return Err(ObjectError::Canceled);
        }
        let data = self.inner.data.lock().unwrap();
        let start = usize::try_from(offset).map_err(|_| ObjectError::InvalidArgument)?;
        if start >= data.len() {
            return Ok(0);
        }
        let count = output
            .len()
            .min(data.len() - start)
            .min(self.inner.maximum_progress.load(Ordering::Acquire));
        output[..count].copy_from_slice(&data[start..start + count]);
        Ok(count)
    }

    fn write_at(
        &self,
        _file: VfsFileToken,
        offset: u64,
        input: &[u8],
        _nonblocking: bool,
    ) -> Result<usize, ObjectError> {
        let count = input.len().min(self.inner.maximum_progress.load(Ordering::Acquire));
        let start = usize::try_from(offset).map_err(|_| ObjectError::InvalidArgument)?;
        let end = start.checked_add(count).ok_or(ObjectError::InvalidArgument)?;
        let mut data = self.inner.data.lock().unwrap();
        let needed = data.len().max(end);
        data.resize(needed, 0);
        data[start..end].copy_from_slice(&input[..count]);
        Ok(count)
    }

    fn append(&self, _file: VfsFileToken, input: &[u8], _nonblocking: bool) -> Result<(usize, u64), ObjectError> {
        let count = input.len().min(self.inner.maximum_progress.load(Ordering::Acquire));
        let mut data = self.inner.data.lock().unwrap();
        data.extend_from_slice(&input[..count]);
        Ok((count, data.len() as u64))
    }

    fn read_vector_at(
        &self,
        _file: VfsFileToken,
        offset: u64,
        output: &mut [IoSliceMut<'_>],
        _nonblocking: bool,
    ) -> Result<usize, ObjectError> {
        let data = self.inner.data.lock().unwrap();
        let mut source = usize::try_from(offset).map_err(|_| ObjectError::InvalidArgument)?;
        let mut total = 0;
        for segment in output {
            let count = segment.len().min(data.len().saturating_sub(source));
            segment[..count].copy_from_slice(&data[source..source + count]);
            source += count;
            total += count;
            if count != segment.len() {
                break;
            }
        }
        Ok(total)
    }

    fn write_vector_at(
        &self,
        _file: VfsFileToken,
        offset: u64,
        input: &[IoSlice<'_>],
        _nonblocking: bool,
    ) -> Result<usize, ObjectError> {
        let mut data = self.inner.data.lock().unwrap();
        let mut target = usize::try_from(offset).map_err(|_| ObjectError::InvalidArgument)?;
        for segment in input {
            let end = target.checked_add(segment.len()).ok_or(ObjectError::ResourceLimit)?;
            let needed = data.len().max(end);
            data.resize(needed, 0);
            data[target..end].copy_from_slice(segment);
            target = end;
        }
        Ok(target - offset as usize)
    }

    fn append_vector(
        &self,
        _file: VfsFileToken,
        input: &[IoSlice<'_>],
        _nonblocking: bool,
    ) -> Result<(usize, u64), ObjectError> {
        let mut data = self.inner.data.lock().unwrap();
        let start = data.len();
        for segment in input {
            data.extend_from_slice(segment);
        }
        Ok((data.len() - start, data.len() as u64))
    }

    fn truncate(&self, _file: VfsFileToken, size: u64) -> Result<(), ObjectError> {
        let size = usize::try_from(size).map_err(|_| ObjectError::ResourceLimit)?;
        self.inner.data.lock().unwrap().resize(size, 0);
        Ok(())
    }

    fn synchronize(&self, _file: VfsFileToken, _data_only: bool) -> Result<(), ObjectError> {
        Ok(())
    }

    fn metadata(&self, _file: VfsFileToken) -> Result<Metadata, ObjectError> {
        let timestamp = Timestamp::new(0, 0).unwrap();
        Ok(Metadata {
            identity: Identity { device: 7, inode: 9 },
            kind: Kind::Regular,
            permissions: Permissions::from_bits(0o666),
            links: 1,
            user: 0,
            group: 0,
            special_device: 0,
            size: self.inner.data.lock().unwrap().len() as u64,
            blocks_512: 0,
            block_size: 4096,
            accessed: timestamp,
            modified: timestamp,
            changed: timestamp,
        })
    }

    fn readiness(&self, _file: VfsFileToken, interests: Readiness) -> Readiness {
        interests
    }

    fn subscribe(
        &self,
        _file: VfsFileToken,
        observer: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn ReadinessSubscription>, ObjectError> {
        self.inner.registry.subscribe(observer)
    }

    fn cancel(&self, _file: VfsFileToken) {
        self.inner.canceled.store(true, Ordering::Release);
        self.inner.wait.notify_all();
        self.inner.registry.close();
    }

    fn close(&self, _file: VfsFileToken) {
        self.inner.closes.fetch_add(1, Ordering::AcqRel);
    }
}

#[test]
fn partial_io_progress() {
    let host = FakeFileHost::new(b"abcdef");
    host.inner.maximum_progress.store(2, Ordering::Release);
    let description = host.description(AccessMode::ReadWrite, StatusFlags::default());
    let table = DescriptorTable::new(8).unwrap();
    let fd = table
        .install(0, description.clone(), DescriptorFlags::default())
        .unwrap();
    let duplicate = table.duplicate(fd, 0, DescriptorFlags::default()).unwrap();
    let fork = table.fork();
    let mut output = [0; 4];
    assert_eq!(table.pin(fd).unwrap().read(&mut output), Ok(2));
    assert_eq!(table.pin(duplicate).unwrap().read(&mut output), Ok(2));
    assert_eq!(fork.pin(fd).unwrap().read(&mut output), Ok(2));
    assert_eq!(description.offset(), 6);
}

#[test]
fn interruption_would_offset() {
    let host = FakeFileHost::new(b"abc");
    let description = host.description(AccessMode::ReadOnly, StatusFlags::default());
    *host.inner.next_read_error.lock().unwrap() = Some(ObjectError::Interrupted);
    assert_eq!(
        hl_descriptor::OpenFileDescription::read(&*description, &mut [0]),
        Err(ObjectError::Interrupted)
    );
    assert_eq!(description.offset(), 0);
    host.block_reads();
    hl_descriptor::OpenFileDescription::set_status_flags(
        &*description,
        StatusFlags::from_bits(StatusFlags::NONBLOCKING),
    )
    .unwrap();
    assert_eq!(
        hl_descriptor::OpenFileDescription::read(&*description, &mut [0]),
        Err(ObjectError::WouldBlock)
    );
    assert_eq!(description.offset(), 0);
}

#[test]
fn prepared_splice_commit() {
    let host = FakeFileHost::new(b"abcdef");
    let description = host.description(AccessMode::ReadOnly, StatusFlags::default());
    let prepared = hl_descriptor::OpenFileDescription::prepare_splice_read(&*description, None, 4, false, None)
        .unwrap()
        .unwrap();
    assert_eq!(prepared.bytes(), b"abcd");
    assert!(matches!(
        hl_descriptor::OpenFileDescription::prepare_splice_read(&*description, None, 1, true, None,),
        Err(ObjectError::WouldBlock),
    ));
    let cancellation = Arc::new(TestCancellation::default());
    let waiter = {
        let description = Arc::clone(&description);
        let cancellation = Arc::clone(&cancellation);
        thread::spawn(move || {
            let mut output = [0_u8; 2];
            let result = hl_descriptor::OpenFileDescription::read_context(
                &*description,
                &mut output,
                OperationContext {
                    actor: None,
                    cancellation: Some(&*cancellation),
                },
            );
            (result, output)
        })
    };
    cancellation.wait_subscribed();
    prepared.commit(2).unwrap();
    let (result, output) = waiter.join().unwrap();
    assert_eq!(result, Ok(2));
    assert_eq!(&output, b"cd");
    assert_eq!(description.offset(), 4);
}

#[test]
fn cancellation_rollback() {
    let host = FakeFileHost::new(b"abcdef");
    let description = host.description(AccessMode::ReadOnly, StatusFlags::default());
    let prepared = hl_descriptor::OpenFileDescription::prepare_splice_read(&*description, None, 4, false, None)
        .unwrap()
        .unwrap();
    let cancellation = Arc::new(TestCancellation::default());
    let waiter = {
        let description = Arc::clone(&description);
        let cancellation = Arc::clone(&cancellation);
        thread::spawn(move || {
            hl_descriptor::OpenFileDescription::read_context(
                &*description,
                &mut [0_u8; 1],
                OperationContext {
                    actor: None,
                    cancellation: Some(&*cancellation),
                },
            )
        })
    };
    cancellation.wait_subscribed();
    cancellation.interrupt();
    assert_eq!(waiter.join().unwrap(), Err(ObjectError::Interrupted));
    assert_eq!(description.offset(), 0);
    drop(prepared);
    assert_eq!(
        hl_descriptor::OpenFileDescription::read(&*description, &mut [0_u8; 1]),
        Ok(1)
    );
}

#[test]
fn final_close_wakeup() {
    let host = FakeFileHost::new(b"abcdef");
    let description = host.description(AccessMode::ReadOnly, StatusFlags::default());
    let prepared = hl_descriptor::OpenFileDescription::prepare_splice_read(&*description, None, 4, false, None)
        .unwrap()
        .unwrap();
    let table = DescriptorTable::new(1).unwrap();
    let fd = table.install(0, description, DescriptorFlags::default()).unwrap();
    let lease = table.pin(fd).unwrap();
    let cancellation = Arc::new(TestCancellation::default());
    let waiter = {
        let cancellation = Arc::clone(&cancellation);
        thread::spawn(move || {
            lease.read_context(
                &mut [0_u8; 1],
                OperationContext {
                    actor: None,
                    cancellation: Some(&*cancellation),
                },
            )
        })
    };
    cancellation.wait_subscribed();
    table.close(fd).unwrap();
    assert_eq!(waiter.join().unwrap(), Err(ObjectError::Retired));
    assert_eq!(host.inner.closes.load(Ordering::Acquire), 1);
    drop(prepared);
    assert_eq!(host.inner.closes.load(Ordering::Acquire), 1);
}

#[test]
fn positional_splice_offset() {
    let host = FakeFileHost::new(b"abcdef");
    let description = host.description(AccessMode::ReadOnly, StatusFlags::default());
    let prepared = hl_descriptor::OpenFileDescription::prepare_splice_read(&*description, Some(3), 2, false, None)
        .unwrap()
        .unwrap();
    assert_eq!(prepared.bytes(), b"de");
    let mut output = [0_u8; 1];
    assert_eq!(
        hl_descriptor::OpenFileDescription::read(&*description, &mut output),
        Ok(1),
    );
    assert_eq!(&output, b"a");
    prepared.commit(2).unwrap();
    assert_eq!(description.offset(), 1);
}

#[test]
fn dropped_prepared_progress() {
    let host = FakeFileHost::new(b"abc");
    let description = host.description(AccessMode::ReadOnly, StatusFlags::default());
    let prepared = hl_descriptor::OpenFileDescription::prepare_splice_read(&*description, None, 3, false, None)
        .unwrap()
        .unwrap();
    drop(prepared);
    let mut output = [0_u8; 1];
    assert_eq!(
        hl_descriptor::OpenFileDescription::read(&*description, &mut output),
        Ok(1),
    );
    assert_eq!(&output, b"a");
}

#[test]
fn dual_cursor_commit_and_rollback() {
    let host = FakeFileHost::new(b"abcdefghijklmnop");
    let input = host.description(AccessMode::ReadWrite, StatusFlags::default());
    let output = host.description(AccessMode::ReadWrite, StatusFlags::default());
    output.seek(SeekPosition::Start(8)).unwrap();
    let transfer = FileTransfer::prepare(&input, None, &output, None, 4, false, None).unwrap();
    assert_eq!(transfer.input_offset(), Some(0));
    assert_eq!(transfer.output_offset(), Some(8));
    assert!(matches!(
        FileTransfer::prepare(&input, None, &output, None, 1, true, None),
        Err(ObjectError::WouldBlock)
    ));
    transfer.commit(2).unwrap();
    assert_eq!((input.offset(), output.offset()), (2, 10));

    let transfer = FileTransfer::prepare(&input, None, &output, None, 2, false, None).unwrap();
    drop(transfer);
    assert_eq!((input.offset(), output.offset()), (2, 10));
}

#[test]
fn dual_cursor_alias_and_preflight_are_atomic() {
    let host = FakeFileHost::new(b"abcdefghijklmnop");
    let description = host.description(AccessMode::ReadWrite, StatusFlags::default());
    assert!(matches!(
        FileTransfer::prepare(&description, None, &description, None, 1, false, None),
        Err(ObjectError::InvalidArgument)
    ));
    assert_eq!(description.offset(), 0);

    let output = host.description(AccessMode::ReadWrite, StatusFlags::default());
    output.seek(SeekPosition::Start(8)).unwrap();
    let transfer = FileTransfer::prepare(&description, None, &output, None, 4, false, None).unwrap();
    assert_eq!(transfer.commit(5), Err(ObjectError::InvalidArgument));
    assert_eq!((description.offset(), output.offset()), (0, 8));
}

#[test]
fn dual_cursor_cancellation_releases_first_gate() {
    let host = FakeFileHost::new(b"abcdefghijklmnop");
    let left = host.description(AccessMode::ReadWrite, StatusFlags::default());
    let right = host.description(AccessMode::ReadWrite, StatusFlags::default());
    let (input, output) =
        if super::cursor::Cursor::address(&left.cursor) < super::cursor::Cursor::address(&right.cursor) {
            (left, right)
        } else {
            (right, left)
        };
    output.seek(SeekPosition::Start(8)).unwrap();
    let blocker = hl_descriptor::OpenFileDescription::prepare_splice_read(&*output, None, 1, false, None)
        .unwrap()
        .unwrap();
    let cancellation = Arc::new(TestCancellation::default());
    let waiter = {
        let input = Arc::clone(&input);
        let output = Arc::clone(&output);
        let cancellation = Arc::clone(&cancellation);
        thread::spawn(move || FileTransfer::prepare(&input, None, &output, None, 1, false, Some(&*cancellation)))
    };
    cancellation.wait_subscribed();
    cancellation.interrupt();
    assert!(matches!(waiter.join().unwrap(), Err(ObjectError::Interrupted)));
    drop(blocker);
    assert_eq!(input.seek(SeekPosition::Current(0)), Ok(0));
    assert_eq!(output.seek(SeekPosition::Current(0)), Ok(8));
}

#[test]
fn append_pwrite_descriptions() {
    let host = FakeFileHost::new(b"");
    let status = StatusFlags::from_bits(StatusFlags::APPEND);
    let first = host.description(AccessMode::ReadWrite, status);
    let second = host.description(AccessMode::ReadWrite, status);
    let left = thread::spawn(move || {
        for _ in 0..100 {
            hl_descriptor::OpenFileDescription::write(&*first, b"AAA").unwrap();
        }
    });
    let right = thread::spawn(move || {
        for _ in 0..100 {
            second.pwrite(0, b"BBB").unwrap();
        }
    });
    left.join().unwrap();
    right.join().unwrap();
    let bytes = host.bytes();
    assert_eq!(bytes.len(), 600);
    assert!(bytes.chunks_exact(3).all(|part| part == b"AAA" || part == b"BBB"));
}

#[test]
fn seek_truncate_stable() {
    let host = FakeFileHost::new(b"abcdef");
    let description = host.description(AccessMode::ReadWrite, StatusFlags::default());
    assert_eq!(description.seek(SeekPosition::End(-2)), Ok(4));
    description.truncate(3).unwrap();
    assert_eq!(description.metadata().unwrap().size, 3);
    assert_eq!(description.identity().inode, 9);
    assert_eq!(description.path().as_bytes(), b"/tmp/\xff");
}

#[test]
fn final_descriptor_lease() {
    let host = FakeFileHost::new(b"");
    host.block_reads();
    let description = host.description(AccessMode::ReadOnly, StatusFlags::default());
    let table = Arc::new(DescriptorTable::new(2).unwrap());
    let fd = table.install(0, description, DescriptorFlags::default()).unwrap();
    let lease = table.pin(fd).unwrap();
    let worker = thread::spawn(move || lease.read(&mut [0]));
    while !host.inner.waiting.load(Ordering::Acquire) {
        thread::yield_now();
    }
    table.close(fd).unwrap();
    assert_eq!(worker.join().unwrap(), Err(ObjectError::Canceled));
    assert_eq!(host.inner.closes.load(Ordering::Acquire), 1);
}

#[test]
fn vector_offset_shared() {
    let host = FakeFileHost::new(b"abcdef");
    let description = host.description(AccessMode::ReadWrite, StatusFlags::default());
    let table = DescriptorTable::new(4).unwrap();
    let fd = table.install(0, description, DescriptorFlags::default()).unwrap();
    let duplicate = table.duplicate(fd, 0, DescriptorFlags::default()).unwrap();
    let first = table.pin(fd).unwrap();
    let second = table.pin(duplicate).unwrap();
    let mut left = [0; 2];
    let mut right = [0; 1];
    let mut output = [IoSliceMut::new(&mut left), IoSliceMut::new(&mut right)];
    let context = OperationContext {
        actor: None,
        cancellation: None,
    };
    assert_eq!(first.read_vector_context(&mut output, context), Ok(3));
    let input = [IoSlice::new(b"X"), IoSlice::new(b"YZ")];
    assert_eq!(second.write_vector_context(&input, context), Ok(3));
    assert_eq!(host.bytes(), b"abcXYZ");
}
