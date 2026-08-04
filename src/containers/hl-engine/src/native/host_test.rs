use super::*;
use std::sync::Mutex;

struct FakeSyscalls {
    calls: Mutex<Vec<&'static str>>,
    read: Mutex<Result<usize, HostError>>,
}

impl Default for FakeSyscalls {
    fn default() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            read: Mutex::new(Ok(0)),
        }
    }
}

impl HostSyscalls for FakeSyscalls {
    fn clock_ns(&self, _: ClockKind) -> Result<u64, HostError> {
        Ok(7)
    }
    fn close_file(&self, _: u64) {
        self.calls.lock().unwrap().push("close");
    }
    fn read(&self, _: u64, _: &mut [u8]) -> Result<usize, HostError> {
        *self.read.lock().unwrap()
    }
    fn write(&self, _: u64, input: &[u8]) -> Result<usize, HostError> {
        Ok(input.len().min(2))
    }
    fn read_at(&self, _: u64, _: u64, output: &mut [u8]) -> Result<usize, HostError> {
        Ok(output.len().min(2))
    }
    fn write_at(&self, _: u64, _: u64, input: &[u8]) -> Result<usize, HostError> {
        Ok(input.len().min(2))
    }
    fn metadata(&self, _: u64) -> Result<FileMetadata, HostError> {
        Err(HostError::Unsupported)
    }
    fn directory_next(&self, _: u64, _: u64, _: &mut [u8]) -> Result<Option<(DirectoryEntry, usize)>, HostError> {
        Ok(None)
    }
    fn page_size(&self) -> Result<usize, HostError> {
        Ok(16_384)
    }
    fn map(&self, _: usize, _: Protection) -> Result<u64, HostError> {
        self.calls.lock().unwrap().push("map");
        Ok(9)
    }
    fn protect(&self, _: u64, _: usize, protection: Protection) -> Result<(), HostError> {
        assert!(!(protection.writable() && protection.executable()));
        self.calls.lock().unwrap().push("protect");
        Ok(())
    }
    fn unmap(&self, _: u64, _: usize) -> Result<(), HostError> {
        self.calls.lock().unwrap().push("unmap");
        Ok(())
    }
}

#[test]
fn file_preserves_partial() {
    let syscalls = Arc::new(FakeSyscalls {
        read: Mutex::new(Err(HostError::WouldBlock)),
        ..Default::default()
    });
    {
        let file = OwnedFile::from_host_handle(Arc::clone(&syscalls), 3);
        assert_eq!(file.read(&mut [0; 4]), Err(HostError::WouldBlock));
        assert_eq!(file.write(&[1, 2, 3]), Ok(2));
    }
    assert_eq!(*syscalls.calls.lock().unwrap(), ["close"]);
}

#[test]
fn mapping_validates_apple() {
    let syscalls = Arc::new(FakeSyscalls::default());
    assert!(matches!(
        OwnedMapping::allocate(Arc::clone(&syscalls), 4096, Protection::READ),
        Err(HostError::Invalid)
    ));
    {
        let mapping = OwnedMapping::allocate(Arc::clone(&syscalls), 16_384, Protection::READ).unwrap();
        mapping.protect(Protection::READ).unwrap();
    }
    assert_eq!(*syscalls.calls.lock().unwrap(), ["map", "protect", "unmap"]);
}

#[test]
fn jit_requires_external() {
    let syscalls = Arc::new(FakeSyscalls::default());
    assert!(matches!(
        JitCapability::allocate(syscalls, 16_384, JitAuthorization::unavailable()),
        Err(HostError::Unsupported)
    ));
}

#[derive(Default)]
struct Wire {
    sent: Mutex<Vec<u8>>,
    incoming: Mutex<Vec<u8>>,
    closed: Mutex<usize>,
}

impl ForkWireSyscalls for Wire {
    fn close_channel(&self, _: u64) {
        *self.closed.lock().unwrap() += 1;
    }

    fn send(&self, _: u64, bytes: &[u8]) -> Result<usize, HostError> {
        let count = bytes.len().min(2);
        self.sent.lock().unwrap().extend_from_slice(&bytes[..count]);
        Ok(count)
    }

    fn receive(&self, _: u64, bytes: &mut [u8]) -> Result<usize, HostError> {
        let mut incoming = self.incoming.lock().unwrap();
        let count = bytes.len().min(incoming.len()).min(3);
        bytes[..count].copy_from_slice(&incoming[..count]);
        incoming.drain(..count);
        Ok(count)
    }
}

impl DescriptorSyscalls for Wire {
    fn duplicate_cloexec(&self, descriptor: i32, _: i32) -> Result<i32, HostError> {
        Ok(descriptor + 100)
    }

    fn close_descriptor(&self, _: i32) {}
}

#[test]
fn fork_wire_frames() {
    let syscalls = Arc::new(Wire::default());
    let frame = ForkFrame::new(b"launch".to_vec()).unwrap();
    {
        let mut sender = ChildChannel::from_host_handle(Arc::clone(&syscalls), 1);
        sender.begin_send(&frame).unwrap();
        assert_eq!(sender.flush(), Ok(true));
    }
    *syscalls.incoming.lock().unwrap() = syscalls.sent.lock().unwrap().clone();
    {
        let mut receiver = ChildChannel::from_host_handle(Arc::clone(&syscalls), 2);
        assert_eq!(receiver.receive().unwrap(), Some(frame));
    }
    assert_eq!(*syscalls.closed.lock().unwrap(), 2);
}

struct ShortWire {
    attachment_results: Mutex<Vec<Result<usize, HostError>>>,
    attachment_calls: Mutex<usize>,
    ordinary_calls: Mutex<usize>,
    closed_descriptors: Mutex<Vec<i32>>,
}

impl ForkWireSyscalls for ShortWire {
    fn close_channel(&self, _: u64) {}
    fn send(&self, _: u64, bytes: &[u8]) -> Result<usize, HostError> {
        *self.ordinary_calls.lock().unwrap() += 1;
        Ok(bytes.len().min(2))
    }
    fn receive(&self, _: u64, _: &mut [u8]) -> Result<usize, HostError> {
        Err(HostError::WouldBlock)
    }
    fn send_attachments(&self, _: u64, _: &[u8], _: &[i32]) -> Result<usize, HostError> {
        *self.attachment_calls.lock().unwrap() += 1;
        self.attachment_results.lock().unwrap().remove(0)
    }
}

impl DescriptorSyscalls for ShortWire {
    fn duplicate_cloexec(&self, descriptor: i32, _: i32) -> Result<i32, HostError> {
        Ok(descriptor + 100)
    }
    fn close_descriptor(&self, descriptor: i32) {
        self.closed_descriptors.lock().unwrap().push(descriptor);
    }
}

#[test]
fn attachment_short_send() {
    let syscalls = Arc::new(ShortWire {
        attachment_results: Mutex::new(vec![Err(HostError::Interrupted), Err(HostError::WouldBlock), Ok(1)]),
        attachment_calls: Mutex::new(0),
        ordinary_calls: Mutex::new(0),
        closed_descriptors: Mutex::new(Vec::new()),
    });
    let source = Descriptor::from_raw(Arc::clone(&syscalls), 5).unwrap();
    let mut channel = ChildChannel::from_host_handle(Arc::clone(&syscalls), 1);
    let frame = ForkFrame::new(b"short-send".to_vec()).unwrap();
    assert_eq!(
        channel.send_with_descriptors(&frame, &[&source]),
        Err(ForkWireError::Host(HostError::WouldBlock))
    );
    assert_eq!(channel.flush(), Ok(true));
    assert_eq!(*syscalls.attachment_calls.lock().unwrap(), 3);
    assert!(*syscalls.ordinary_calls.lock().unwrap() > 0);
    assert_eq!(*syscalls.closed_descriptors.lock().unwrap(), [105]);
}

#[test]
fn cancelling_unsent_attachments() {
    let syscalls = Arc::new(ShortWire {
        attachment_results: Mutex::new(vec![Ok(0)]),
        attachment_calls: Mutex::new(0),
        ordinary_calls: Mutex::new(0),
        closed_descriptors: Mutex::new(Vec::new()),
    });
    let source = Descriptor::from_raw(Arc::clone(&syscalls), 8).unwrap();
    let mut channel = ChildChannel::from_host_handle(Arc::clone(&syscalls), 1);
    let frame = ForkFrame::new(b"cancel".to_vec()).unwrap();
    assert_eq!(
        channel.send_with_descriptors(&frame, &[&source]),
        Err(ForkWireError::Host(HostError::WouldBlock))
    );
    channel.cancel_send();
    channel.cancel_send();
    assert_eq!(*syscalls.closed_descriptors.lock().unwrap(), [108]);
}

#[derive(Default)]
struct Processes {
    closed: Mutex<Vec<u64>>,
}

#[derive(Default)]
struct Descriptors {
    next: Mutex<i32>,
    closed: Mutex<Vec<i32>>,
}

impl DescriptorSyscalls for Descriptors {
    fn duplicate_cloexec(&self, _: i32, minimum: i32) -> Result<i32, HostError> {
        let mut next = self.next.lock().unwrap();
        *next = (*next).max(minimum);
        let result = *next;
        *next += 1;
        Ok(result)
    }

    fn close_descriptor(&self, descriptor: i32) {
        self.closed.lock().unwrap().push(descriptor);
    }
}

#[test]
fn private_descriptors_are() {
    let syscalls = Arc::new(Descriptors::default());
    let source = Descriptor::from_raw(Arc::clone(&syscalls), 7).unwrap();
    let allocator = PrivateDescriptorAllocator::new(Arc::clone(&syscalls), 512, 2).unwrap();
    let stale = allocator.adopt(&source).unwrap();
    let retained = allocator.adopt(&source).unwrap();
    assert_eq!(allocator.adopt(&source), Err(HostError::Exhausted));
    allocator.set_inherit(retained, true).unwrap();
    allocator.release(stale).unwrap();
    let replacement = allocator.adopt(&source).unwrap();
    assert_eq!(allocator.release(stale), Err(HostError::Invalid));
    assert_eq!(allocator.exec_sweep(), Ok(1));
    assert_eq!(allocator.release(replacement), Err(HostError::Invalid));
    allocator.release(retained).unwrap();
    drop(source);
    assert_eq!(*syscalls.closed.lock().unwrap(), [512, 514, 513, 7]);
}

impl ProcessSyscalls for Processes {
    fn spawn(&self, _: &SpawnRequest) -> Result<(ProcessId, u64), HostError> {
        Ok((ProcessId::new(17)?, 9))
    }
    fn close_process(&self, token: u64) {
        self.closed.lock().unwrap().push(token);
    }
    fn wait(&self, _: ProcessId) -> Result<Option<ChildExit>, HostError> {
        Ok(Some(ChildExit::Code(0)))
    }
    fn wait_blocking(&self, _: ProcessId) -> Result<ChildExit, HostError> {
        Ok(ChildExit::Code(0))
    }
    fn signal(&self, _: ProcessId, _: ProcessSignal) -> Result<(), HostError> {
        Ok(())
    }
    fn signal_group(&self, _: ProcessId, _: ProcessSignal) -> Result<(), HostError> {
        Ok(())
    }
}

#[test]
fn process_handle_owns() {
    let syscalls = Arc::new(Processes::default());
    let request = SpawnRequest {
        program: std::ffi::CString::new("/bin/true").unwrap(),
        arguments: Vec::new(),
        environment: Vec::new(),
        process_group: ProcessGroup::Inherit,
        file_actions: Vec::new(),
    };
    {
        let process = ProcessHandle::spawn(Arc::clone(&syscalls), &request).unwrap();
        assert_eq!(process.id().get(), 17);
        assert_eq!(process.wait(), Ok(Some(ChildExit::Code(0))));
    }
    assert_eq!(*syscalls.closed.lock().unwrap(), [9]);
}
