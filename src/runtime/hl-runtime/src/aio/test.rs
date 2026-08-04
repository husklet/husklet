use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use hl_descriptor::{
    CancellationNotification, CancellationSubscription, DescriptorFlags, DescriptorTable, ObjectError,
    OpenFileDescription, OperationCancellation,
};
use hl_linux::{
    AioSyscalls, GuestAccess, GuestArchitecture, GuestFault, GuestMemory, LinuxResult, SyscallFamily, SyscallOperation,
};

use super::RuntimeAioSyscalls;

#[derive(Clone)]
struct Memory(Arc<Mutex<Vec<u8>>>);

impl GuestMemory for Memory {
    fn probe(&self, address: u64, length: usize, access: GuestAccess) -> Result<usize, GuestFault> {
        if (address as usize)
            .checked_add(length)
            .is_some_and(|end| end <= self.0.lock().unwrap().len())
        {
            Ok(length)
        } else {
            Err(GuestFault { address, access })
        }
    }
    fn read(&self, address: u64, output: &mut [u8]) -> Result<usize, GuestFault> {
        self.probe(address, output.len(), GuestAccess::Read)?;
        output.copy_from_slice(&self.0.lock().unwrap()[address as usize..address as usize + output.len()]);
        Ok(output.len())
    }
    fn write(&self, address: u64, input: &[u8]) -> Result<usize, GuestFault> {
        self.probe(address, input.len(), GuestAccess::Write)?;
        self.0.lock().unwrap()[address as usize..address as usize + input.len()].copy_from_slice(input);
        Ok(input.len())
    }
}

#[derive(Debug)]
struct File;
impl OpenFileDescription for File {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> Result<usize, ObjectError> {
        let source = b"aio-data";
        let start = (offset as usize).min(source.len());
        let count = output.len().min(source.len() - start);
        output[..count].copy_from_slice(&source[start..start + count]);
        Ok(count)
    }
}

struct Cancellation {
    pending: AtomicBool,
    notifications: Mutex<Vec<Arc<dyn CancellationNotification>>>,
}
struct Subscription;
impl CancellationSubscription for Subscription {}
impl Cancellation {
    fn new() -> Self {
        Self {
            pending: AtomicBool::new(false),
            notifications: Mutex::new(Vec::new()),
        }
    }
    fn interrupt(&self) {
        self.pending.store(true, Ordering::Release);
        for notification in self.notifications.lock().unwrap().iter() {
            notification.notify();
        }
    }
}
impl OperationCancellation for Cancellation {
    fn interrupted(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }
    fn subscribe(&self, notification: Arc<dyn CancellationNotification>) -> Box<dyn CancellationSubscription> {
        self.notifications.lock().unwrap().push(notification);
        Box::new(Subscription)
    }
}

fn operation(number: u16, name: &'static str) -> SyscallOperation {
    SyscallOperation {
        canonical_number: number,
        name,
        family: SyscallFamily::Aio,
    }
}

fn fixture(
    architecture: GuestArchitecture,
) -> (
    RuntimeAioSyscalls<Memory>,
    Memory,
    Arc<hl_aio::Catalog>,
    Arc<DescriptorTable>,
    Arc<Cancellation>,
) {
    let memory = Memory(Arc::new(Mutex::new(vec![0; 1024])));
    let catalog = Arc::new(hl_aio::Catalog::default());
    let descriptors = Arc::new(DescriptorTable::new(32).unwrap());
    descriptors
        .install(3, Arc::new(File), DescriptorFlags::default())
        .unwrap();
    let cancellation = Arc::new(Cancellation::new());
    let runtime = RuntimeAioSyscalls::new(
        Arc::clone(&catalog),
        Arc::clone(&descriptors),
        memory.clone(),
        architecture,
        cancellation.clone(),
    );
    (runtime, memory, catalog, descriptors, cancellation)
}

fn setup(runtime: &mut RuntimeAioSyscalls<Memory>, memory: &Memory) -> u64 {
    assert_eq!(
        runtime.handle(operation(0, "io_setup"), [4, 8, 0, 0, 0, 0]),
        LinuxResult::Value(0)
    );
    u64::from_ne_bytes(memory.0.lock().unwrap()[8..16].try_into().unwrap())
}

fn control(memory: &Memory, address: u64, opcode: u16) {
    let mut control = [0_u8; 64];
    control[0..8].copy_from_slice(&77_u64.to_ne_bytes());
    control[16..18].copy_from_slice(&opcode.to_ne_bytes());
    control[20..24].copy_from_slice(&3_u32.to_ne_bytes());
    control[24..32].copy_from_slice(&512_u64.to_ne_bytes());
    control[32..40].copy_from_slice(&8_u64.to_ne_bytes());
    memory.write(address, &control).unwrap();
}

fn pointers(memory: &Memory, address: u64, values: &[u64]) {
    let bytes = values.iter().flat_map(|value| value.to_ne_bytes()).collect::<Vec<_>>();
    memory.write(address, &bytes).unwrap();
}

#[test]
fn pread_roundtrip_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let (mut runtime, memory, _, _, _) = fixture(architecture);
        let context = setup(&mut runtime, &memory);
        control(&memory, 128, 0);
        pointers(&memory, 64, &[128]);
        assert_eq!(
            runtime.handle(operation(2, "io_submit"), [context, 1, 64, 0, 0, 0]),
            LinuxResult::Value(1)
        );
        assert_eq!(
            runtime.handle(operation(4, "io_getevents"), [context, 1, 1, 256, 0, 0]),
            LinuxResult::Value(1)
        );
        assert_eq!(&memory.0.lock().unwrap()[512..520], b"aio-data");
        assert_eq!(
            i64::from_ne_bytes(memory.0.lock().unwrap()[272..280].try_into().unwrap()),
            8
        );
    }
}

#[test]
fn null_control_preserves_submitted_prefix_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let (mut runtime, memory, _, _, _) = fixture(architecture);
        let context = setup(&mut runtime, &memory);
        control(&memory, 128, 0);
        pointers(&memory, 64, &[128, 0]);

        assert_eq!(
            runtime.handle(operation(2, "io_submit"), [context, 2, 64, 0, 0, 0]),
            LinuxResult::Value(1)
        );
        assert_eq!(
            runtime.handle(operation(4, "io_getevents"), [context, 0, 2, 256, 0, 0]),
            LinuxResult::Value(1)
        );
        assert_eq!(
            u64::from_ne_bytes(memory.0.lock().unwrap()[264..272].try_into().unwrap()),
            128
        );
    }
}

#[test]
fn sole_null_control_faults_without_completion_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let (mut runtime, memory, _, _, _) = fixture(architecture);
        let context = setup(&mut runtime, &memory);
        pointers(&memory, 64, &[0]);

        assert_eq!(
            runtime.handle(operation(2, "io_submit"), [context, 1, 64, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EFAULT)
        );
        assert_eq!(
            runtime.handle(operation(4, "io_getevents"), [context, 0, 1, 256, 0, 0]),
            LinuxResult::Value(0)
        );
    }
}

#[test]
fn unsupported_control_preserves_submitted_prefix_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let (mut runtime, memory, _, _, _) = fixture(architecture);
        let context = setup(&mut runtime, &memory);
        control(&memory, 128, 0);
        control(&memory, 192, 6);
        pointers(&memory, 64, &[128, 192]);

        assert_eq!(
            runtime.handle(operation(2, "io_submit"), [context, 2, 64, 0, 0, 0]),
            LinuxResult::Value(1)
        );
        assert_eq!(
            runtime.handle(operation(4, "io_getevents"), [context, 0, 2, 256, 0, 0]),
            LinuxResult::Value(1)
        );

        let (mut runtime, memory, _, _, _) = fixture(architecture);
        let context = setup(&mut runtime, &memory);
        control(&memory, 192, 6);
        pointers(&memory, 64, &[192]);
        assert_eq!(
            runtime.handle(operation(2, "io_submit"), [context, 1, 64, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EINVAL)
        );
        assert_eq!(
            runtime.handle(operation(4, "io_getevents"), [context, 0, 1, 256, 0, 0]),
            LinuxResult::Value(0)
        );
    }
}

#[test]
fn pointer_array_fault_precedes_submission_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let (mut runtime, memory, _, _, _) = fixture(architecture);
        let context = setup(&mut runtime, &memory);
        control(&memory, 128, 0);
        pointers(&memory, 1016, &[128]);

        assert_eq!(
            runtime.handle(operation(2, "io_submit"), [context, 2, 1016, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EFAULT)
        );
        assert_eq!(
            runtime.handle(operation(4, "io_getevents"), [context, 0, 1, 256, 0, 0]),
            LinuxResult::Value(0)
        );
    }
}

#[test]
fn blocked_wait_interrupts() {
    let (mut runtime, memory, catalog, descriptors, cancellation) = fixture(GuestArchitecture::Aarch64);
    assert_eq!(
        runtime.handle(operation(0, "io_setup"), [1, 8, 0, 0, 0, 0]),
        LinuxResult::Value(0)
    );
    let context = u64::from_ne_bytes(memory.0.lock().unwrap()[8..16].try_into().unwrap());
    let worker_memory = memory.clone();
    let worker_cancel = Arc::clone(&cancellation);
    let worker = std::thread::spawn(move || {
        let mut runtime = RuntimeAioSyscalls::new(
            catalog,
            descriptors,
            worker_memory,
            GuestArchitecture::Aarch64,
            worker_cancel,
        );
        runtime.handle(operation(4, "io_getevents"), [context, 1, 1, 256, 0, 0])
    });
    while cancellation.notifications.lock().unwrap().is_empty() {
        std::thread::yield_now();
    }
    cancellation.interrupt();
    assert_eq!(worker.join().unwrap(), LinuxResult::Error(hl_linux::Errno::EINTR));
}
