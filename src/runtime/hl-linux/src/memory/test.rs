use std::sync::Mutex;

use hl_isa::{GuestArchitecture, GuestPageSize};
use hl_memory::{Placement, Protection};

use crate::{
    Advice, AdvicePlan, GuestAccess, GuestFault, GuestMarshaller, GuestMemory, MapSource, MemoryAbi, MemoryMarshalError,
};

const BASE: u64 = 0x1000;

struct Memory(Mutex<Vec<u8>>);

impl Memory {
    fn new() -> Self {
        Self(Mutex::new(vec![0; 0x4000]))
    }

    fn offset(address: u64, access: GuestAccess) -> Result<usize, GuestFault> {
        usize::try_from(address.checked_sub(BASE).ok_or(GuestFault { address, access })?)
            .map_err(|_| GuestFault { address, access })
    }

    fn put(&self, address: u64, bytes: &[u8]) {
        let offset = Self::offset(address, GuestAccess::Write).unwrap();
        self.0.lock().unwrap()[offset..offset + bytes.len()].copy_from_slice(bytes);
    }

    fn get(&self, address: u64, length: usize) -> Vec<u8> {
        let offset = Self::offset(address, GuestAccess::Read).unwrap();
        self.0.lock().unwrap()[offset..offset + length].to_vec()
    }
}

impl GuestMemory for Memory {
    fn probe(&self, address: u64, length: usize, access: GuestAccess) -> Result<usize, GuestFault> {
        let offset = Self::offset(address, access)?;
        let available = self.0.lock().unwrap().len().saturating_sub(offset);
        if length != 0 && available == 0 {
            return Err(GuestFault { address, access });
        }
        Ok(length.min(available))
    }

    fn read(&self, address: u64, output: &mut [u8]) -> Result<usize, GuestFault> {
        let count = self.probe(address, output.len(), GuestAccess::Read)?;
        let offset = Self::offset(address, GuestAccess::Read)?;
        output[..count].copy_from_slice(&self.0.lock().unwrap()[offset..offset + count]);
        Ok(count)
    }

    fn write(&self, address: u64, input: &[u8]) -> Result<usize, GuestFault> {
        let count = self.probe(address, input.len(), GuestAccess::Write)?;
        let offset = Self::offset(address, GuestAccess::Write)?;
        self.0.lock().unwrap()[offset..offset + count].copy_from_slice(&input[..count]);
        Ok(count)
    }
}

#[test]
fn mmap_mmap2_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let memory = Memory::new();
        let abi = MemoryAbi::new(&memory, architecture);
        let byte = abi.mmap(0x4000, 1, 3, 0x12, 7, 0x2000).unwrap();
        let pages = abi.mmap2(0x4000, 1, 3, 0x12, 7, 2).unwrap();
        assert_eq!(byte.offset, pages.offset);
        assert_eq!(byte.requested_length, 1);
        assert_eq!(pages.requested_length, 1);
        assert_eq!(byte.length, GuestPageSize::LINUX.bytes());
        assert_eq!(pages.length, GuestPageSize::LINUX.bytes());
        assert!(!byte.no_reserve);
        assert!(!pages.no_reserve);
        assert_eq!(byte.placement, Placement::Fixed(0x4000_u64.into()));
        assert_eq!(
            byte.source,
            MapSource::File {
                descriptor: 7,
                shared: false
            }
        );
        assert!(byte.protection.contains(Protection::READ.union(Protection::WRITE),));

        let mmap_reserved = abi.mmap(0, 1, 3, 0x22, -1, 0).unwrap();
        let mmap_no_reserve = abi.mmap(0, 1, 3, 0x4022, -1, 0).unwrap();
        let mmap2_reserved = abi.mmap2(0, 1, 3, 0x22, -1, 0).unwrap();
        let mmap2_no_reserve = abi.mmap2(0, 1, 3, 0x4022, -1, 0).unwrap();
        for plan in [mmap_reserved, mmap_no_reserve, mmap2_reserved, mmap2_no_reserve] {
            assert_eq!(plan.requested_length, 1);
            assert_eq!(plan.length, GuestPageSize::LINUX.bytes());
        }
        assert!(!mmap_reserved.no_reserve);
        assert!(mmap_no_reserve.no_reserve);
        assert!(!mmap2_reserved.no_reserve);
        assert!(mmap2_no_reserve.no_reserve);
    }
}

#[test]
fn mmap_ignores_denywrite() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let memory = Memory::new();
        let abi = MemoryAbi::new(&memory, architecture);
        let plain = abi.mmap(0, 4096, 0, 0x22, -1, 0).unwrap();
        let legacy = abi.mmap(0, 4096, 0, 0x822, -1, 0).unwrap();
        assert_eq!(legacy, plain);
    }
}

#[test]
fn mmap_rejects_overflow() {
    let memory = Memory::new();
    let abi = MemoryAbi::new(&memory, GuestArchitecture::X86_64);
    assert_eq!(abi.mmap(0, 1, 0, 0x20, -1, 0), Err(MemoryMarshalError::Invalid),);
    assert_eq!(abi.mmap(1, 4096, 0, 0x32, -1, 0), Err(MemoryMarshalError::Invalid),);
    assert_eq!(abi.mmap(0, 4096, 0, 0x22, -1, 1), Err(MemoryMarshalError::Invalid),);
    assert_eq!(abi.mmap(0, u64::MAX, 0, 0x22, -1, 0), Err(MemoryMarshalError::Overflow),);
}

#[test]
fn range_remap_rules() {
    assert!(MemoryAbi::<Memory>::munmap(0x1001, 4096).is_err());
    assert!(MemoryAbi::<Memory>::munmap(0x1000, 0).is_err());
    // Linux screens the start alignment ahead of the zero-length short circuit,
    // so only an aligned start reaches the `return 0`.
    assert_eq!(MemoryAbi::<Memory>::mprotect(1, 0, u32::MAX), Err(MemoryMarshalError::Invalid));
    // A bad protection loses to the zero-length short circuit, but both grow flags
    // together are screened ahead of it.
    assert_eq!(MemoryAbi::<Memory>::mprotect(0x1000, 0, 8), Ok(None));
    assert_eq!(
        MemoryAbi::<Memory>::mprotect(0x1000, 0, 0x0300_0000),
        Err(MemoryMarshalError::Invalid)
    );
    let writable_executable = MemoryAbi::<Memory>::mprotect(0x1000, 4096, 6).unwrap().unwrap();
    assert_eq!(
        writable_executable.protection,
        Some(Protection::WRITE.union(Protection::EXECUTE))
    );
    assert_eq!(
        MemoryAbi::<Memory>::mprotect(0x1000, 4096, 8),
        Err(MemoryMarshalError::Invalid)
    );
    let lock = MemoryAbi::<Memory>::mlock(0x1800, 0x1000).unwrap().unwrap();
    assert_eq!(lock.range.start().get(), 0x1000);
    assert_eq!(lock.range.length(), 0x2000);
    assert!(MemoryAbi::<Memory>::mremap(0x1000, 0x1000, 0x2000, 2, 0x4000,).is_err());
    assert!(MemoryAbi::<Memory>::mremap(0x1000, 0x1000, 0x2000, 3, 0x4001,).is_err());
    assert!(MemoryAbi::<Memory>::mremap(0x1000, 0x1000, 0x2000, 5, 0,).is_err());
}

#[test]
fn mincore_copyout_mutating() {
    let memory = Memory::new();
    let abi = MemoryAbi::new(&memory, GuestArchitecture::Aarch64);
    assert!(abi.mincore(0x2000, 0x3000, u64::MAX).is_err());
    assert_eq!(memory.get(BASE, 3), vec![0; 3]);
    let (_, output) = abi.mincore(0x2000, 0x3000, BASE).unwrap();
    let staged = abi.stage_mincore(output, &[true, false, true]).unwrap();
    assert_eq!(memory.get(BASE, 3), vec![0; 3]);
    staged
        .commit(&GuestMarshaller::new(&memory, GuestArchitecture::Aarch64))
        .unwrap();
    assert_eq!(memory.get(BASE, 3), vec![1, 0, 1]);
    assert_eq!(abi.mincore(1, 0, u64::MAX), Err(MemoryMarshalError::Invalid));
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let abi = MemoryAbi::new(&memory, architecture);
        assert_eq!(abi.mincore(0x2000, 0, u64::MAX), Ok((None, u64::MAX)));
        assert_eq!(abi.mincore(0x2001, 0, u64::MAX), Err(MemoryMarshalError::Invalid));
        assert!(matches!(
            abi.mincore(0x2000, 8192, BASE + 0x3fff),
            Err(MemoryMarshalError::Marshal(crate::MarshalError::Fault(_)))
        ));
    }
}

#[test]
fn lock_sync_access() {
    let memory = Memory::new();
    let abi = MemoryAbi::new(&memory, GuestArchitecture::X86_64);
    assert!(MemoryAbi::<Memory>::mlockall(4).is_err());
    for flags in [0, 1, 2, 3, 4, 6] {
        assert!(MemoryAbi::<Memory>::msync(0x1000, 1, flags).is_ok());
    }
    for flags in [5, 7, 8] {
        assert_eq!(
            MemoryAbi::<Memory>::msync(0x1000, 1, flags),
            Err(MemoryMarshalError::Invalid)
        );
    }
    assert!(MemoryAbi::<Memory>::madvise(0x1001, 4096, 0).is_err());
    assert_eq!(
        MemoryAbi::<Memory>::madvise(u64::MAX, 0, 0),
        Err(MemoryMarshalError::Invalid)
    );
    assert_eq!(MemoryAbi::<Memory>::madvise(0x2000, 0, 0), Ok(AdvicePlan::Noop));
    for (value, advice) in [
        (4, Advice::DontNeed),
        (10, Advice::DontFork),
        (11, Advice::DoFork),
        (18, Advice::WipeOnFork),
        (19, Advice::KeepOnFork),
        (22, Advice::Noop),
    ] {
        assert_eq!(
            MemoryAbi::<Memory>::madvise(0x2000, 4096, value),
            Ok(AdvicePlan::Apply {
                range: hl_isa::AddressRange::nonempty(hl_isa::GuestAddress::new(0x2000), 4096).unwrap(),
                advice,
            }),
        );
    }
    assert_eq!(
        MemoryAbi::<Memory>::madvise(u64::MAX, 0, -1),
        Err(MemoryMarshalError::Invalid),
    );
    assert_eq!(
        abi.memfd_create(u64::MAX, 0x8000_0000),
        Err(MemoryMarshalError::Invalid),
    );
    assert!(matches!(
        abi.memfd_create(u64::MAX, 0),
        Err(MemoryMarshalError::Marshal(_)),
    ));
    memory.put(BASE, b"shared-cache\0");
    let plan = abi.memfd_create(BASE, 3).unwrap();
    assert_eq!(plan.name, b"shared-cache");
    assert!(plan.close_on_exec);
    assert!(plan.allow_sealing);
    memory.put(BASE, &[b'n'; 250]);
    assert_eq!(abi.memfd_create(BASE, 0), Err(MemoryMarshalError::Invalid));
    assert_eq!(abi.memfd_create(BASE, 4), Err(MemoryMarshalError::Invalid));
    memory.put(BASE, b"huge\0");
    assert_eq!(abi.memfd_create(BASE, 4).unwrap().huge_page, Some(0));
}

#[test]
fn msync_alignment_isas() {
    for _architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        assert!(MemoryAbi::<Memory>::msync(0x1001, 0, 1).is_err());
        assert!(MemoryAbi::<Memory>::msync(0x1000, 0, 1).is_ok());
    }
}

#[test]
fn munmap_wrap_is_invalid() {
    assert_eq!(
        MemoryAbi::<Memory>::munmap(u64::MAX - 4095, 8192),
        Err(MemoryMarshalError::Invalid),
    );
}
