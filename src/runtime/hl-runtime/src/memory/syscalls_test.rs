use super::*;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{AnonymousMemoryAccount, BrkSnapshot, MemfdRegistry, RuntimeFilesystemSyscalls, RuntimeMemoryError};
use hl_descriptor::{DescriptorFlags, ObjectKind, OpenFileDescription, StatusFlags};
use hl_isa::{AddressRange, GuestAddress};
use hl_linux::{DescriptorIoSyscalls, GuestAccess, GuestFault, SyscallFamily};
use hl_memory::{
    Backing, FileIdentity, MapRequest, MemoryError, Protection, SharedLimits, SharedObjectStore, SharedSeal,
};

#[derive(Debug)]
struct MmapAccount {
    limit: u64,
    current: AtomicU64,
}

impl AnonymousMemoryAccount for MmapAccount {
    fn reserve(&self, bytes: u64) -> bool {
        self.current
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(bytes).filter(|next| *next <= self.limit)
            })
            .is_ok()
    }

    fn refund(&self, bytes: u64) {
        assert!(
            self.current
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| current
                    .checked_sub(bytes))
                .is_ok()
        );
    }

    fn current(&self) -> u64 {
        self.current.load(Ordering::Acquire)
    }
}

#[derive(Clone, Debug)]
struct Memory {
    inner: Arc<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    bytes: Mutex<Vec<u8>>,
    fail_write: AtomicBool,
}

impl Memory {
    fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                bytes: Mutex::new(vec![0; 512]),
                fail_write: AtomicBool::new(false),
            }),
        }
    }

    fn put(&self, address: usize, bytes: &[u8]) {
        self.inner.bytes.lock().unwrap()[address..address + bytes.len()].copy_from_slice(bytes);
    }
}

impl GuestMemory for Memory {
    fn probe(&self, address: u64, length: usize, access: GuestAccess) -> Result<usize, GuestFault> {
        let available = self.inner.bytes.lock().unwrap().len().saturating_sub(address as usize);
        if available < length {
            return Err(GuestFault { address, access });
        }
        Ok(length)
    }

    fn read(&self, address: u64, output: &mut [u8]) -> Result<usize, GuestFault> {
        let start = address as usize;
        output.copy_from_slice(&self.inner.bytes.lock().unwrap()[start..start + output.len()]);
        Ok(output.len())
    }

    fn write(&self, address: u64, input: &[u8]) -> Result<usize, GuestFault> {
        if self.inner.fail_write.load(Ordering::Acquire) {
            return Err(GuestFault {
                address,
                access: GuestAccess::Write,
            });
        }
        let start = address as usize;
        self.inner.bytes.lock().unwrap()[start..start + input.len()].copy_from_slice(input);
        Ok(input.len())
    }
}

#[derive(Debug, Default)]
struct MappingState {
    calls: Vec<&'static str>,
    fail_commit: bool,
}

#[derive(Clone, Debug, Default)]
struct Mapping(Arc<Mutex<MappingState>>);

impl MappingHost for Mapping {
    fn stage_map(&self, _: GuestAddress, _: MapRequest) -> Result<u64, MemoryError> {
        self.0.lock().unwrap().calls.push("map");
        Ok(1)
    }

    fn stage_unmap(&self, _: AddressRange) -> Result<u64, MemoryError> {
        self.0.lock().unwrap().calls.push("unmap");
        Ok(2)
    }

    fn stage_protect(&self, _: AddressRange, _: Protection) -> Result<u64, MemoryError> {
        self.0.lock().unwrap().calls.push("protect");
        Ok(3)
    }

    fn commit(&self, _: &[u64]) -> Result<(), MemoryError> {
        let mut state = self.0.lock().unwrap();
        state.calls.push("commit");
        if state.fail_commit {
            Err(MemoryError::InvariantViolation)
        } else {
            Ok(())
        }
    }

    fn rollback(&self, _: u64) {
        self.0.lock().unwrap().calls.push("rollback");
    }

    fn stage_remap(&self, _: AddressRange, _: GuestAddress, _: MapRequest, _: bool) -> Result<u64, MemoryError> {
        self.0.lock().unwrap().calls.push("remap");
        Ok(4)
    }
}

#[derive(Debug, Default)]
struct Services {
    calls: Mutex<Vec<&'static str>>,
}

impl RuntimeMemoryHost for Services {
    fn advise(&self, _: hl_linux::AdvicePlan) -> Result<(), RuntimeMemoryError> {
        self.calls.lock().unwrap().push("advise");
        Ok(())
    }

    fn residency(&self, plan: hl_linux::MemoryRangePlan) -> Result<Vec<bool>, RuntimeMemoryError> {
        self.calls.lock().unwrap().push("residency");
        Ok(vec![true; (plan.range.length() / 4096) as usize])
    }

    fn lock(&self, _: Option<hl_linux::MemoryRangePlan>, _: bool) -> Result<(), RuntimeMemoryError> {
        self.calls.lock().unwrap().push("lock");
        Ok(())
    }

    fn unlock(&self, _: Option<hl_linux::MemoryRangePlan>) -> Result<(), RuntimeMemoryError> {
        self.calls.lock().unwrap().push("unlock");
        Ok(())
    }

    fn lock_all(&self, _: hl_linux::LockAllPlan) -> Result<(), RuntimeMemoryError> {
        self.calls.lock().unwrap().push("lock_all");
        Ok(())
    }

    fn unlock_all(&self) -> Result<(), RuntimeMemoryError> {
        self.calls.lock().unwrap().push("unlock_all");
        Ok(())
    }

    fn sync(&self, _: hl_linux::MsyncPlan) -> Result<(), RuntimeMemoryError> {
        self.calls.lock().unwrap().push("sync");
        Ok(())
    }
}

#[derive(Debug)]
struct File;

impl OpenFileDescription for File {
    fn kind(&self) -> ObjectKind {
        ObjectKind::File
    }
}

#[derive(Debug, Default)]
struct Files;

impl DescriptorMappingSource for Files {
    fn backing(
        &self,
        descriptor: &hl_descriptor::OperationLease,
        _: u64,
        _: u64,
        shared: bool,
        _: bool,
    ) -> Result<Backing, RuntimeMemoryError> {
        Ok(Backing::File {
            identity: FileIdentity {
                device: 7,
                object: descriptor.descriptor_number() as u64,
            },
            shared,
        })
    }
}

struct Fixture {
    mapping: Mapping,
    coordinator: Arc<MappingCoordinator<Mapping>>,
    descriptors: Arc<DescriptorTable>,
    memory: Memory,
    services: Arc<Services>,
}

impl Fixture {
    fn new() -> Self {
        let mapping = Mapping::default();
        Self {
            coordinator: Arc::new(MappingCoordinator::new(mapping.clone())),
            mapping,
            descriptors: Arc::new(DescriptorTable::new(8).unwrap()),
            memory: Memory::new(),
            services: Arc::new(Services::default()),
        }
    }

    fn runtime(&self, architecture: GuestArchitecture) -> RuntimeMemorySyscalls<Mapping, Memory> {
        RuntimeMemorySyscalls::new(
            self.coordinator.clone(),
            self.descriptors.clone(),
            self.memory.clone(),
            architecture,
        )
        .with_address_minimum(4096)
        .with_host(self.services.clone())
        .with_descriptor_source(Arc::new(Files))
    }

    fn operation(name: &'static str) -> SyscallOperation {
        SyscallOperation {
            canonical_number: 0,
            name,
            family: SyscallFamily::Memory,
        }
    }

    fn accounted_runtime(
        &self,
        architecture: GuestArchitecture,
        limit: u64,
    ) -> (RuntimeMemorySyscalls<Mapping, Memory>, Arc<MmapAccount>) {
        let account = Arc::new(MmapAccount {
            limit,
            current: AtomicU64::new(0),
        });
        let brk = BrkRegion::new(
            Arc::clone(&self.coordinator),
            BrkSnapshot {
                lower: GuestAddress::new(0x10_0000),
                current: GuestAddress::new(0x10_0000),
                upper: GuestAddress::new(0x20_0000),
                backing_identity: 91,
            },
        )
        .unwrap()
        .with_account(account.clone())
        .unwrap();
        (self.runtime(architecture).with_brk(brk), account)
    }
}

#[test]
fn mmap_limit_noreserve_and_exact_unmap_both_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let (mut runtime, account) = fixture.accounted_runtime(architecture, 4097);
        let mmap = Fixture::operation("mmap");
        let munmap = Fixture::operation("munmap");
        assert_eq!(
            runtime.handle(mmap, [0x4000, 4095, 3, 0x32, u64::MAX, 0]),
            LinuxResult::Value(0x4000)
        );
        assert_eq!(account.current(), 4095);
        assert_eq!(
            runtime.handle(mmap, [0x6000, 3, 3, 0x32, u64::MAX, 0]),
            LinuxResult::Error(Errno::ENOMEM)
        );
        assert_eq!(account.current(), 4095);
        assert_eq!(
            runtime.handle(mmap, [0x8000, 8193, 3, 0x4032, u64::MAX, 0]),
            LinuxResult::Value(0x8000)
        );
        assert_eq!(account.current(), 4095);
        assert_eq!(
            runtime.handle(mmap, [0x6000, 2, 3, 0x32, u64::MAX, 0]),
            LinuxResult::Value(0x6000)
        );
        assert_eq!(account.current(), 4097);
        assert_eq!(
            runtime.handle(munmap, [0x4000, 4096, 0, 0, 0, 0]),
            LinuxResult::Value(0)
        );
        assert_eq!(account.current(), 2);
        assert_eq!(
            runtime.handle(munmap, [0x4000, 4096, 0, 0, 0, 0]),
            LinuxResult::Value(0)
        );
        assert_eq!(account.current(), 2);
    }
}

#[test]
fn mmap_failure_and_fixed_replacement_are_transactional_both_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let (mut runtime, account) = fixture.accounted_runtime(architecture, 5000);
        let mmap = Fixture::operation("mmap");
        fixture.mapping.0.lock().unwrap().fail_commit = true;
        assert_eq!(
            runtime.handle(mmap, [0x4000, 3000, 3, 0x32, u64::MAX, 0]),
            LinuxResult::Error(Errno::EINVAL)
        );
        assert_eq!(account.current(), 0);
        fixture.mapping.0.lock().unwrap().fail_commit = false;
        assert_eq!(
            runtime.handle(mmap, [0x4000, 3000, 3, 0x32, u64::MAX, 0]),
            LinuxResult::Value(0x4000)
        );
        assert_eq!(
            runtime.handle(mmap, [0x5000, 4096, 3, 0x4032, u64::MAX, 0]),
            LinuxResult::Value(0x5000)
        );
        assert_eq!(account.current(), 3000);
        assert_eq!(
            runtime.handle(mmap, [0x4000, 5000, 3, 0x32, u64::MAX, 0]),
            LinuxResult::Value(0x4000)
        );
        assert_eq!(account.current(), 5000);
    }
}

#[test]
fn mremap_raw_growth_shrink_and_limit_both_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let (mut runtime, account) = fixture.accounted_runtime(architecture, 5000);
        let mmap = Fixture::operation("mmap");
        let mremap = Fixture::operation("mremap");
        assert_eq!(
            runtime.handle(mmap, [0x4000, 1, 3, 0x32, u64::MAX, 0]),
            LinuxResult::Value(0x4000)
        );
        assert_eq!(account.current(), 1);
        assert_eq!(
            runtime.handle(mremap, [0x4000, 1, 4095, 0, 0, 0]),
            LinuxResult::Value(0x4000)
        );
        assert_eq!(account.current(), 4095);
        assert_eq!(
            runtime.handle(mremap, [0x4000, 4095, 5000, 0, 0, 0]),
            LinuxResult::Value(0x4000)
        );
        assert_eq!(account.current(), 5000);
        assert_eq!(
            runtime.handle(mremap, [0x4000, 5000, 5001, 0, 0, 0]),
            LinuxResult::Error(Errno::ENOMEM)
        );
        assert_eq!(account.current(), 5000);
        assert_eq!(
            runtime.handle(mremap, [0x4000, 5000, 7, 0, 0, 0]),
            LinuxResult::Value(0x4000)
        );
        assert_eq!(account.current(), 7);
        assert!(
            fixture
                .coordinator
                .ledger()
                .resolve(GuestAddress::new(0x5000), Protection::NONE)
                .is_none()
        );
    }
}

#[test]
fn mremap_move_and_dontunmap_account_once_both_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let (mut runtime, account) = fixture.accounted_runtime(architecture, 8192);
        let mmap = Fixture::operation("mmap");
        let mremap = Fixture::operation("mremap");
        assert_eq!(
            runtime.handle(mmap, [0x4000, 4096, 3, 0x32, u64::MAX, 0]),
            LinuxResult::Value(0x4000)
        );
        assert_eq!(
            runtime.handle(mmap, [0x8000, 4096, 3, 0x32, u64::MAX, 0]),
            LinuxResult::Value(0x8000)
        );
        assert_eq!(account.current(), 8192);
        assert_eq!(
            runtime.handle(mremap, [0x4000, 4096, 4096, 3, 0x8000, 0]),
            LinuxResult::Value(0x8000)
        );
        assert_eq!(
            account.current(),
            4096,
            "fixed move replaces rather than duplicates charge"
        );
        assert_eq!(
            runtime.handle(mremap, [0x8000, 4096, 4096, 5, 0, 0]),
            LinuxResult::Value(0x9000)
        );
        assert_eq!(account.current(), 8192, "DONTUNMAP owns both mappings");
        assert_eq!(
            runtime.handle(mremap, [0x8000, 4096, 4096, 5, 0, 0]),
            LinuxResult::Error(Errno::ENOMEM)
        );
        assert_eq!(account.current(), 8192);
    }
}

#[test]
fn brk_and_mmap_share_container_limit_both_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let (mut runtime, account) = fixture.accounted_runtime(architecture, 4096);
        let brk = Fixture::operation("brk");
        let mmap = Fixture::operation("mmap");
        let munmap = Fixture::operation("munmap");
        assert_eq!(
            runtime.handle(brk, [0x10_0bb8, 0, 0, 0, 0, 0]),
            LinuxResult::Value(0x10_0bb8)
        );
        assert_eq!(account.current(), 3000);
        assert_eq!(
            runtime.handle(mmap, [0x4000, 1097, 3, 0x32, u64::MAX, 0]),
            LinuxResult::Error(Errno::ENOMEM)
        );
        assert_eq!(
            runtime.handle(mmap, [0x4000, 1096, 3, 0x32, u64::MAX, 0]),
            LinuxResult::Value(0x4000)
        );
        assert_eq!(account.current(), 4096);
        assert_eq!(
            runtime.handle(munmap, [0x4000, 4096, 0, 0, 0, 0]),
            LinuxResult::Value(0)
        );
        assert_eq!(account.current(), 3000);
        assert_eq!(
            runtime.handle(brk, [0x10_1000, 0, 0, 0, 0, 0]),
            LinuxResult::Value(0x10_1000)
        );
        assert_eq!(account.current(), 4096);
    }
}

#[test]
fn mremap_failure_refunds_growth_both_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let (mut runtime, account) = fixture.accounted_runtime(architecture, 8192);
        let mmap = Fixture::operation("mmap");
        let mremap = Fixture::operation("mremap");
        assert_eq!(
            runtime.handle(mmap, [0x4000, 4096, 3, 0x32, u64::MAX, 0]),
            LinuxResult::Value(0x4000)
        );
        fixture.mapping.0.lock().unwrap().fail_commit = true;
        assert_eq!(
            runtime.handle(mremap, [0x4000, 4096, 8192, 0, 0, 0]),
            LinuxResult::Error(Errno::EINVAL)
        );
        assert_eq!(account.current(), 4096);
        assert!(
            fixture
                .coordinator
                .ledger()
                .resolve(GuestAddress::new(0x5000), Protection::NONE)
                .is_none()
        );
    }
}

#[test]
fn process_vectors_copy_and_validate_both_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        fixture.memory.put(100, b"rust");
        let record = |base: u64, length: u64| [base.to_le_bytes(), length.to_le_bytes()].concat();
        fixture.memory.put(0, &record(200, 4));
        fixture.memory.put(32, &record(100, 4));
        let mut runtime = fixture.runtime(architecture).with_process(7);
        let operation = Fixture::operation("process_vm_readv");
        assert_eq!(
            MemorySyscalls::handle(&mut runtime, operation, [7, 0, 1, 32, 1, 0]),
            LinuxResult::Value(4)
        );
        let mut copied = [0; 4];
        fixture.memory.read(200, &mut copied).unwrap();
        assert_eq!(&copied, b"rust");
        assert_eq!(
            MemorySyscalls::handle(&mut runtime, operation, [7, 0, 1, 32, 1, 1]),
            LinuxResult::Error(Errno::EINVAL)
        );
        assert_eq!(
            MemorySyscalls::handle(&mut runtime, operation, [8, 0, 1, 32, 1, 0]),
            LinuxResult::Error(Errno::ESRCH)
        );
    }
}

#[test]
fn membarrier_commands() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture);
        let operation = Fixture::operation("membarrier");
        assert_eq!(
            hl_linux::MemorySyscalls::handle(&mut runtime, operation, [0, u64::MAX, u64::MAX, 0, 0, 0]),
            LinuxResult::Value(0x7f)
        );
        for command in [1, 2, 4, 8, 16, 32, 64] {
            assert_eq!(
                hl_linux::MemorySyscalls::handle(&mut runtime, operation, [command, u64::MAX, u64::MAX, 0, 0, 0]),
                LinuxResult::Value(0)
            );
        }
        assert_eq!(
            hl_linux::MemorySyscalls::handle(&mut runtime, operation, [3, 0, 0, 0, 0, 0]),
            LinuxResult::Error(Errno::EINVAL)
        );
    }
}

#[test]
fn isas_transaction_boundary() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture);
        assert_eq!(
            runtime.handle(Fixture::operation("mmap"), [0x4000, 1, 3, 0x22, u64::MAX, 0],),
            LinuxResult::Value(0x4000),
        );
        assert_eq!(
            runtime.handle(Fixture::operation("mprotect"), [0x4000, 4096, 1, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        assert_eq!(
            runtime.handle(Fixture::operation("munmap"), [0x4000, 4096, 0, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        assert_eq!(
            fixture.mapping.0.lock().unwrap().calls,
            ["map", "commit", "protect", "commit", "unmap", "commit"],
        );
    }
}

#[test]
fn address_limit_errno() {
    let fixture = Fixture::new();
    let mut runtime = fixture.runtime(GuestArchitecture::X86_64).with_address_limit(0x5000);
    assert_eq!(
        runtime.handle(Fixture::operation("mmap"), [0x1000, 0x4000, 3, 0x32, u64::MAX, 0]),
        LinuxResult::Value(0x1000),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("mmap"), [0, 4096, 3, 0x22, u64::MAX, 0]),
        LinuxResult::Error(Errno::ENOMEM),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("mmap"), [0x5000, 4096, 3, 0x32, u64::MAX, 0]),
        LinuxResult::Error(Errno::ENOMEM),
    );
}

#[test]
fn mmap_minimum_address_policy() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture).with_address_minimum(MMAP_MINIMUM_ADDRESS);
        let mmap = Fixture::operation("mmap");

        assert_eq!(
            runtime.handle(mmap, [0, 4096, 3, 0x32, u64::MAX, 0]),
            LinuxResult::Error(Errno::EPERM),
        );
        assert_eq!(
            runtime.handle(mmap, [0x1000, 4096, 3, 0x32, u64::MAX, 0]),
            LinuxResult::Error(Errno::EPERM),
        );
        assert_eq!(
            runtime.handle(mmap, [1, 4096, 3, 0x32, u64::MAX, 0]),
            LinuxResult::Error(Errno::EINVAL),
        );
        assert_eq!(
            runtime.handle(mmap, [0x8000, 4096, 3, 0x32, u64::MAX, 0]),
            LinuxResult::Value(0x8000),
        );
        assert_eq!(fixture.mapping.0.lock().unwrap().calls, ["map", "commit"]);
    }
}

#[test]
fn file_host_mapping() {
    let fixture = Fixture::new();
    let install = fixture
        .descriptors
        .prepare_open(0, Arc::new(File), StatusFlags::default(), DescriptorFlags::default())
        .unwrap();
    let descriptor = install.publish();
    let mut runtime = fixture.runtime(GuestArchitecture::X86_64);
    assert_eq!(
        runtime.handle(
            Fixture::operation("mmap"),
            [0x8000, 4096, 1, 0x11, descriptor as u64, 0],
        ),
        LinuxResult::Value(0x8000),
    );
    fixture.descriptors.close(descriptor).unwrap();
    assert_eq!(
        runtime.handle(
            Fixture::operation("mmap"),
            [0x9000, 4096, 1, 0x11, descriptor as u64, 0],
        ),
        LinuxResult::Error(Errno::EBADF),
    );
}

#[test]
fn file_mmap_access_mode() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let readonly = fixture
            .descriptors
            .prepare_open(0, Arc::new(File), StatusFlags::default(), DescriptorFlags::default())
            .unwrap()
            .publish();
        let writeonly = fixture
            .descriptors
            .prepare_open(0, Arc::new(File), StatusFlags::from_bits(1), DescriptorFlags::default())
            .unwrap()
            .publish();
        let readwrite = fixture
            .descriptors
            .prepare_open(0, Arc::new(File), StatusFlags::from_bits(2), DescriptorFlags::default())
            .unwrap()
            .publish();
        let mut runtime = fixture.runtime(architecture);
        let mmap = Fixture::operation("mmap");

        assert_eq!(
            runtime.handle(mmap, [0x8000, 4096, 3, 0x11, readonly as u64, 0]),
            LinuxResult::Error(Errno::EACCES),
        );
        assert_eq!(
            runtime.handle(mmap, [0x9000, 4096, 3, 0x12, readonly as u64, 0]),
            LinuxResult::Value(0x9000),
        );
        assert_eq!(
            runtime.handle(mmap, [0xa000, 4096, 1, 0x11, writeonly as u64, 0]),
            LinuxResult::Error(Errno::EACCES),
        );
        assert_eq!(
            runtime.handle(mmap, [0xb000, 4096, 3, 0x11, readwrite as u64, 0]),
            LinuxResult::Value(0xb000),
        );
    }
}

#[test]
fn unmapped_munmap_succeeds() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture);
        let munmap = Fixture::operation("munmap");

        assert_eq!(
            runtime.handle(munmap, [0x4000, 4096, 0, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        assert!(fixture.mapping.0.lock().unwrap().calls.is_empty());
        assert_eq!(
            runtime.handle(Fixture::operation("mmap"), [0x4000, 4096, 3, 0x32, u64::MAX, 0]),
            LinuxResult::Value(0x4000),
        );
        assert_eq!(
            runtime.handle(munmap, [0x4000, 4096, 0, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        let calls = fixture.mapping.0.lock().unwrap().calls.clone();
        assert_eq!(
            runtime.handle(munmap, [0x4000, 4096, 0, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        assert_eq!(fixture.mapping.0.lock().unwrap().calls, calls);
    }
}

#[test]
fn file_mmap_validates_descriptor_before_length() {
    let fixture = Fixture::new();
    let mut runtime = fixture.runtime(GuestArchitecture::X86_64);
    assert_eq!(
        runtime.handle(Fixture::operation("mmap"), [0, 0, 1, 0x2, u64::MAX, 0],),
        LinuxResult::Error(Errno::EBADF),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("mmap"), [0, 0, 1, 0x22, u64::MAX, 0],),
        LinuxResult::Error(Errno::EINVAL),
    );
}

#[test]
fn mincore_services_explicit() {
    let fixture = Fixture::new();
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64);
    runtime.handle(Fixture::operation("mmap"), [0x4000, 8192, 1, 0x22, u64::MAX, 0]);
    fixture.memory.inner.fail_write.store(true, Ordering::Release);
    assert_eq!(
        runtime.handle(Fixture::operation("mincore"), [0x4000, 8192, 32, 0, 0, 0]),
        LinuxResult::Error(Errno::EFAULT),
    );
    assert_eq!(&fixture.memory.inner.bytes.lock().unwrap()[32..34], &[0, 0]);
    fixture.memory.inner.fail_write.store(false, Ordering::Release);
    assert_eq!(
        runtime.handle(Fixture::operation("mincore"), [0x4000, 8192, 32, 0, 0, 0]),
        LinuxResult::Value(0),
    );
    assert_eq!(&fixture.memory.inner.bytes.lock().unwrap()[32..34], &[1, 1]);
    assert_eq!(
        runtime.handle(Fixture::operation("brk"), [0x12_000, 0, 0, 0, 0, 0]),
        LinuxResult::Error(Errno::ENOSYS),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("madvise"), [0x4000, 4096, 0, 0, 0, 0]),
        LinuxResult::Value(0),
    );
}

#[test]
fn mlock2_validation() {
    let fixture = Fixture::new();
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64);
    assert_eq!(
        runtime.handle(Fixture::operation("mlock2"), [0x4001, 4095, 4, 0, 0, 0]),
        LinuxResult::Error(Errno::EINVAL),
    );
    assert!(fixture.services.calls.lock().unwrap().is_empty());
    assert_eq!(
        runtime.handle(Fixture::operation("mlock2"), [0x4001, 4095, 1, 0, 0, 0]),
        LinuxResult::Value(0),
    );
    assert_eq!(&*fixture.services.calls.lock().unwrap(), &["lock"]);
}

#[test]
fn remap_atomic_commit() {
    let fixture = Fixture::new();
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64);
    assert_eq!(
        runtime.handle(Fixture::operation("mmap"), [0x4000, 4096, 3, 0x22, u64::MAX, 0],),
        LinuxResult::Value(0x4000),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("mremap"), [0x4000, 4096, 8192, 0, 0, 0],),
        LinuxResult::Value(0x4000),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("mremap"), [0x4000, 8192, 4096, 0, 0, 0],),
        LinuxResult::Value(0x4000),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("mremap"), [0x4000, 4096, 4096, 3, 0x8000, 0],),
        LinuxResult::Value(0x8000),
    );
    assert!(
        fixture
            .coordinator
            .ledger()
            .resolve(GuestAddress::new(0x4000), Protection::NONE)
            .is_none()
    );
    assert_eq!(
        runtime.handle(Fixture::operation("mremap"), [0x8000, 4096, 4096, 7, 0xc000, 0],),
        LinuxResult::Value(0xc000),
    );
    for address in [0x8000, 0xc000] {
        assert!(
            fixture
                .coordinator
                .ledger()
                .resolve(GuestAddress::new(address), Protection::NONE)
                .is_some()
        );
    }
}

#[test]
fn dontunmap_shared_rejected() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let shared = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
        let coordinator = Arc::new(MappingCoordinator::with_shared(fixture.mapping.clone(), shared.clone()));
        let mut runtime = RuntimeMemorySyscalls::new(
            coordinator.clone(),
            fixture.descriptors.clone(),
            fixture.memory.clone(),
            architecture,
        )
        .with_address_minimum(4096)
        .with_host(fixture.services.clone())
        .with_descriptor_source(Arc::new(Files))
        .with_memfd_objects(shared, 7, Arc::new(MemfdRegistry::new()));
        assert_eq!(
            runtime.handle(Fixture::operation("mmap"), [0x4000, 4096, 3, 0x21, u64::MAX, 0],),
            LinuxResult::Value(0x4000),
        );
        let calls_before = fixture.mapping.0.lock().unwrap().calls.clone();
        assert_eq!(
            runtime.handle(Fixture::operation("mremap"), [0x4000, 4096, 4096, 5, 0, 0],),
            LinuxResult::Error(Errno::EINVAL),
        );
        assert_eq!(fixture.mapping.0.lock().unwrap().calls, calls_before);
        assert!(
            coordinator
                .ledger()
                .resolve(GuestAddress::new(0x4000), Protection::NONE)
                .is_some()
        );
    }
}

#[test]
fn dontunmap_file_rejected() {
    let fixture = Fixture::new();
    let install = fixture
        .descriptors
        .prepare_open(0, Arc::new(File), StatusFlags::default(), DescriptorFlags::default())
        .unwrap();
    let descriptor = install.publish();
    let mut runtime = fixture.runtime(GuestArchitecture::X86_64);
    assert_eq!(
        runtime.handle(
            Fixture::operation("mmap"),
            [0x8000, 4096, 3, 0x12, descriptor as u64, 0],
        ),
        LinuxResult::Value(0x8000),
    );
    let calls_before = fixture.mapping.0.lock().unwrap().calls.clone();
    assert_eq!(
        runtime.handle(Fixture::operation("mremap"), [0x8000, 4096, 4096, 5, 0, 0],),
        LinuxResult::Error(Errno::EINVAL),
    );
    assert_eq!(fixture.mapping.0.lock().unwrap().calls, calls_before);
}

#[test]
fn failed_semantics_honest() {
    let fixture = Fixture::new();
    fixture.mapping.0.lock().unwrap().fail_commit = true;
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64);
    assert_eq!(
        runtime.handle(Fixture::operation("mmap"), [0x4000, 4096, 1, 0x22, u64::MAX, 0],),
        LinuxResult::Error(Errno::EINVAL),
    );
    assert!(fixture.coordinator.ledger().regions().is_empty());
    assert_eq!(
        runtime.handle(Fixture::operation("memfd_create"), [0; 6]),
        LinuxResult::Error(Errno::EFAULT),
    );
}

#[test]
fn shared_anonymous() {
    let shared = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
    let coordinator = Arc::new(MappingCoordinator::with_shared(Mapping::default(), shared.clone()));
    let mut runtime = RuntimeMemorySyscalls::new(
        coordinator.clone(),
        Arc::new(DescriptorTable::new(4).unwrap()),
        Memory::new(),
        GuestArchitecture::X86_64,
    )
    .with_address_minimum(4096)
    .with_memfd_objects(shared, 7, Arc::new(MemfdRegistry::new()));

    assert_eq!(
        runtime.handle(Fixture::operation("mmap"), [0x4000, 4096, 3, 0x21, u64::MAX, 0],),
        LinuxResult::Value(0x4000)
    );
    assert!(matches!(
        coordinator.snapshot().regions[0].backing(),
        Backing::Anonymous { shared: true, .. }
    ));
}

#[test]
fn memfd_one_identity() {
    let mapping = Mapping::default();
    let shared = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
    let coordinator = Arc::new(MappingCoordinator::with_shared(mapping, shared.clone()));
    let descriptors = Arc::new(DescriptorTable::new(8).unwrap());
    let memory = Memory::new();
    memory.put(8, b"chrome-shm\0");
    let registry = Arc::new(MemfdRegistry::new());
    let mut runtime = RuntimeMemorySyscalls::new(
        coordinator.clone(),
        descriptors.clone(),
        memory.clone(),
        GuestArchitecture::X86_64,
    )
    .with_address_minimum(4096)
    .with_memfd_objects(shared.clone(), 7, registry.clone());
    let LinuxResult::Value(descriptor) = runtime.handle(Fixture::operation("memfd_create"), [8, 3, 0, 0, 0, 0]) else {
        panic!();
    };
    assert!(descriptors.flags(descriptor as i32).unwrap().closes_on_exec());
    let duplicate = descriptors
        .duplicate(descriptor as i32, 0, DescriptorFlags::default())
        .unwrap();
    let mut filesystem = RuntimeFilesystemSyscalls::new(descriptors.clone(), memory, GuestArchitecture::X86_64)
        .with_memfds(registry.clone());
    assert_eq!(
        DescriptorIoSyscalls::handle(
            &mut filesystem,
            Fixture::operation("ftruncate"),
            [duplicate as u64, 8192, 0, 0, 0, 0],
        ),
        LinuxResult::Value(0),
    );
    let lease = descriptors.pin(duplicate).unwrap();
    let Backing::Shared(reference) = registry
        .backing(lease.description_identity(), 4096, 4096, true)
        .unwrap()
    else {
        panic!();
    };
    lease.write_at(4096, b"rust").unwrap();
    let pin = shared.pin(reference.object, false).unwrap();
    let mut bytes = [0; 4];
    pin.read(4096, &mut bytes).unwrap();
    assert_eq!(&bytes, b"rust");
    drop(pin);
    assert_eq!(
        runtime.handle(Fixture::operation("mmap"), [0x4000, 4096, 3, 1, duplicate as u64, 4096],),
        LinuxResult::Value(0x4000),
    );
    assert_eq!(
        DescriptorIoSyscalls::handle(
            &mut filesystem,
            Fixture::operation("fcntl"),
            [duplicate as u64, 1033, SharedSeal::WRITE as u64, 0, 0, 0],
        ),
        LinuxResult::Error(Errno::EBUSY),
    );
    assert_eq!(
        DescriptorIoSyscalls::handle(
            &mut filesystem,
            Fixture::operation("fcntl"),
            [duplicate as u64, 1033, SharedSeal::FUTURE_WRITE as u64, 0, 0, 0],
        ),
        LinuxResult::Value(0),
    );
    descriptors.close(descriptor as i32).unwrap();
    assert!(
        registry
            .seals(descriptors.pin(duplicate).unwrap().description_identity())
            .is_ok()
    );
    descriptors.close(duplicate).unwrap();
    assert_eq!(
        runtime.handle(Fixture::operation("munmap"), [0x4000, 4096, 0, 0, 0, 0]),
        LinuxResult::Value(0),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("memfd_create"), [8, 0, 0, 0, 0, 0]),
        LinuxResult::Value(descriptor),
    );
}

#[path = "mincore_test.rs"]
mod mincore_tests;

#[path = "brk_test.rs"]
mod brk_tests;
