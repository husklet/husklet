use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use hl_descriptor::{
    DescriptorFlags, DescriptorTable, ObjectError, ObjectKind, OpenFileDescription, PreparedSpliceRead,
};
use hl_isa::GuestArchitecture;
use hl_linux::{Errno, GuestAccess, GuestFault, GuestMemory, LinuxResult};

use crate::{RuntimeFilesystemSyscalls, RuntimePipeCancellation};

struct Memory(Mutex<Vec<u8>>);

#[derive(Debug)]
struct TestFile {
    data: Vec<u8>,
    position: Arc<Mutex<u64>>,
    prepares: Arc<AtomicUsize>,
}

struct TestPrepared {
    position: Arc<Mutex<u64>>,
    start: u64,
    bytes: Vec<u8>,
    shared: bool,
}

impl OpenFileDescription for TestFile {
    fn kind(&self) -> ObjectKind {
        ObjectKind::File
    }

    fn prepare_splice_read(
        &self,
        offset: Option<u64>,
        maximum: usize,
        _nonblocking: bool,
        _cancellation: Option<&dyn hl_descriptor::OperationCancellation>,
    ) -> Result<Option<Box<dyn PreparedSpliceRead>>, ObjectError> {
        self.prepares.fetch_add(1, Ordering::AcqRel);
        let shared = offset.is_none();
        let start = offset.unwrap_or_else(|| *self.position.lock().unwrap());
        let start_index = usize::try_from(start).map_err(|_| ObjectError::InvalidArgument)?;
        let end = self.data.len().min(start_index.saturating_add(maximum));
        let bytes = self.data.get(start_index..end).unwrap_or_default().to_vec();
        Ok(Some(Box::new(TestPrepared {
            position: Arc::clone(&self.position),
            start,
            bytes,
            shared,
        })))
    }
}

impl PreparedSpliceRead for TestPrepared {
    fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    fn commit(self: Box<Self>, count: usize) -> Result<(), ObjectError> {
        if count > self.bytes.len() {
            return Err(ObjectError::InvalidArgument);
        }
        if self.shared {
            *self.position.lock().unwrap() = self
                .start
                .checked_add(count as u64)
                .ok_or(ObjectError::InvalidArgument)?;
        }
        Ok(())
    }
}

impl Memory {
    fn new() -> Self {
        Self(Mutex::new(vec![0; 256]))
    }
    fn put(&self, address: usize, bytes: &[u8]) {
        self.0.lock().unwrap()[address..address + bytes.len()].copy_from_slice(bytes);
    }
    fn get(&self, address: usize, length: usize) -> Vec<u8> {
        self.0.lock().unwrap()[address..address + length].to_vec()
    }
    fn adapter(architecture: GuestArchitecture) -> RuntimeFilesystemSyscalls<Self> {
        RuntimeFilesystemSyscalls::new(Arc::new(DescriptorTable::new(8).unwrap()), Self::new(), architecture)
    }
}

impl GuestMemory for Memory {
    fn probe(&self, address: u64, length: usize, access: GuestAccess) -> Result<usize, GuestFault> {
        let offset = usize::try_from(address).map_err(|_| GuestFault { address, access })?;
        Ok(length.min(self.0.lock().unwrap().len().saturating_sub(offset)))
    }
    fn read(&self, address: u64, output: &mut [u8]) -> Result<usize, GuestFault> {
        let offset = address as usize;
        let bytes = self.0.lock().unwrap();
        let count = output.len().min(bytes.len().saturating_sub(offset));
        output[..count].copy_from_slice(&bytes[offset..offset + count]);
        Ok(count)
    }
    fn write(&self, address: u64, input: &[u8]) -> Result<usize, GuestFault> {
        let offset = address as usize;
        let mut bytes = self.0.lock().unwrap();
        let count = input.len().min(bytes.len().saturating_sub(offset));
        bytes[offset..offset + count].copy_from_slice(&input[..count]);
        Ok(count)
    }
}

#[test]
fn tee_bytes_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let adapter = Memory::adapter(architecture);
        assert_eq!(adapter.pipe2(0, 0o4000), LinuxResult::Value(0));
        assert_eq!(adapter.pipe2(8, 0o4000), LinuxResult::Value(0));
        adapter.memory.put(64, b"teedata");
        assert_eq!(adapter.write(1, 64, 7), LinuxResult::Value(7));
        assert_eq!(adapter.tee([0, 3, 7, 0, 0, 0]), LinuxResult::Value(7));
        assert_eq!(adapter.read(2, 80, 7), LinuxResult::Value(7));
        assert_eq!(&adapter.memory.get(80, 7), b"teedata");
        assert_eq!(adapter.splice([0, 0, 3, 0, 7, 0]), LinuxResult::Value(7));
        assert_eq!(adapter.read(2, 96, 7), LinuxResult::Value(7));
        assert_eq!(&adapter.memory.get(96, 7), b"teedata");
        assert_eq!(adapter.read(0, 112, 1), LinuxResult::Error(Errno::EAGAIN));
    }
}

#[test]
fn vmsplice_pipe_bytes() {
    let adapter = Memory::adapter(GuestArchitecture::Aarch64);
    assert_eq!(adapter.pipe2(0, 0o4000), LinuxResult::Value(0));
    adapter.memory.put(64, b"vmsplice");
    let mut vectors = Vec::new();
    for (base, length) in [(64_u64, 2_u64), (66, 6)] {
        vectors.extend_from_slice(&base.to_le_bytes());
        vectors.extend_from_slice(&length.to_le_bytes());
    }
    adapter.memory.put(16, &vectors);
    assert_eq!(adapter.vmsplice([1, 16, 2, 0, 0, 0]), LinuxResult::Value(8));
    let mut output = Vec::new();
    output.extend_from_slice(&96_u64.to_le_bytes());
    output.extend_from_slice(&8_u64.to_le_bytes());
    adapter.memory.put(48, &output);
    assert_eq!(adapter.vmsplice([0, 48, 1, 0, 0, 0]), LinuxResult::Value(8));
    assert_eq!(&adapter.memory.get(96, 8), b"vmsplice");
}

#[test]
fn splice_kinds_first() {
    let adapter = Memory::adapter(GuestArchitecture::X86_64);
    assert_eq!(adapter.pipe2(0, 0o4000), LinuxResult::Value(0));
    assert_eq!(adapter.tee([0, 1, 1, 0x10, 0, 0]), LinuxResult::Error(Errno::EINVAL));
    assert_eq!(adapter.splice([0, 64, 1, 0, 1, 0]), LinuxResult::Error(Errno::ESPIPE));
    assert_eq!(
        adapter.vmsplice([1, 16, 1, 0x10, 0, 0]),
        LinuxResult::Error(Errno::EINVAL)
    );
}

#[test]
fn blocked_consuming_source() {
    let interruption = Arc::new(hl_sync::Interruption::new());
    let adapter = Memory::adapter(GuestArchitecture::Aarch64)
        .with_pipe_cancellation(Arc::new(RuntimePipeCancellation::new(interruption.clone())));
    assert_eq!(adapter.pipe2(0, 0), LinuxResult::Value(0));
    assert_eq!(adapter.pipe2(8, 0), LinuxResult::Value(0));
    let blocked = std::thread::spawn(move || adapter.tee([0, 3, 1, 0, 0, 0]));
    std::thread::sleep(std::time::Duration::from_millis(10));
    interruption.interrupt();
    assert_eq!(blocked.join().unwrap(), LinuxResult::Error(Errno::EINTR));
}

#[test]
fn file_selected_offset() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let adapter = Memory::adapter(architecture);
        assert_eq!(adapter.pipe2(0, 0o4000), LinuxResult::Value(0));
        let position = Arc::new(Mutex::new(0));
        let file = Arc::new(TestFile {
            data: b"abcdef".to_vec(),
            position: Arc::clone(&position),
            prepares: Arc::new(AtomicUsize::new(0)),
        });
        let file_descriptor = adapter
            .descriptors
            .install(2, file, DescriptorFlags::default())
            .unwrap();
        adapter.memory.put(32, &2_i64.to_le_bytes());
        assert_eq!(
            adapter.splice([file_descriptor as u64, 32, 1, 0, 3, 0,]),
            LinuxResult::Value(3),
        );
        assert_eq!(adapter.read(0, 64, 3), LinuxResult::Value(3));
        assert_eq!(&adapter.memory.get(64, 3), b"cde");
        assert_eq!(i64::from_le_bytes(adapter.memory.get(32, 8).try_into().unwrap()), 5,);
        assert_eq!(*position.lock().unwrap(), 0);
        assert_eq!(
            adapter.splice([file_descriptor as u64, 0, 1, 0, 3, 0,]),
            LinuxResult::Value(3),
        );
        assert_eq!(adapter.read(0, 80, 3), LinuxResult::Value(3));
        assert_eq!(&adapter.memory.get(80, 3), b"abc");
        assert_eq!(*position.lock().unwrap(), 3);
    }
}

#[test]
fn invalid_pipe_progress() {
    let adapter = Memory::adapter(GuestArchitecture::Aarch64);
    assert_eq!(adapter.pipe2(0, 0o4000), LinuxResult::Value(0));
    let prepares = Arc::new(AtomicUsize::new(0));
    let file = Arc::new(TestFile {
        data: b"abc".to_vec(),
        position: Arc::new(Mutex::new(0)),
        prepares: Arc::clone(&prepares),
    });
    let file_descriptor = adapter
        .descriptors
        .install(2, file, DescriptorFlags::default())
        .unwrap();
    assert_eq!(
        adapter.splice([file_descriptor as u64, 252, 1, 0, 3, 0,]),
        LinuxResult::Error(Errno::EFAULT),
    );
    assert_eq!(prepares.load(Ordering::Acquire), 0);
    assert_eq!(adapter.read(0, 64, 1), LinuxResult::Error(Errno::EAGAIN));
}
