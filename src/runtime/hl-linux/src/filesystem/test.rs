use std::sync::Mutex;

use hl_isa::GuestArchitecture;
use hl_vfs::{DeviceId, FileIdentity, FileKind, FileMetadata, FileTimestamp, OpenIntent, Permissions};

use crate::{
    DirectoryRecord, FilesystemAbi, FilesystemMarshalError, FilesystemTarget, GuestAccess, GuestFault, GuestMarshaller,
    GuestMemory, LockType, StatOutputKind, XattrPlan,
};

const BASE: u64 = 0x1000;

struct Memory(Mutex<Vec<u8>>);

impl Memory {
    fn new() -> Self {
        Self(Mutex::new(vec![0; 0x5000]))
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

fn metadata() -> FileMetadata {
    let timestamp = FileTimestamp {
        seconds: 1,
        nanoseconds: 2,
    };
    FileMetadata {
        identity: FileIdentity {
            device: DeviceId::new(8, 1).linux_encoded(),
            inode: 9,
        },
        kind: FileKind::Regular,
        permissions: Permissions::from_bits(0o644),
        links: 1,
        user: 2,
        group: 3,
        special_device: 0,
        size: 4,
        blocks_512: 8,
        accessed: timestamp,
        modified: timestamp,
        changed: timestamp,
    }
}

#[test]
fn openat2_fixture_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let memory = Memory::new();
        let abi = FilesystemAbi::new(&memory, architecture);
        memory.put(BASE, b"/tmp/file\0");
        let mut how = Vec::new();
        how.extend_from_slice(&0x8_0242_u64.to_le_bytes());
        how.extend_from_slice(&0o640_u64.to_le_bytes());
        how.extend_from_slice(&0x0c_u64.to_le_bytes());
        memory.put(BASE + 0x100, &how);
        let plan = abi.openat2(-100, BASE, BASE + 0x100, 24).unwrap();
        assert_eq!(plan.operand.path.as_bytes(), b"/tmp/file");
        assert!(plan.close_on_exec);
        assert!(plan.resolve.no_symlinks);
        assert!(plan.resolve.beneath);
    }
}

#[test]
fn futimens_null_path() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let memory = Memory::new();
        let abi = FilesystemAbi::new(&memory, architecture);
        let plan = abi.utimensat(7, 0, 0, 0).expect("descriptor timestamps");
        let crate::FsMutationPlan::SetTimes { target, times } = plan else {
            panic!("unexpected mutation plan");
        };
        assert_eq!(target.directory.raw(), 7);
        assert!(target.allow_empty);
        assert!(target.path.as_bytes().is_empty());
        assert_eq!(times, [crate::TimestampChange::Now; 2]);
    }
}

#[test]
fn legacy_time_buffers_are_x86_layouts() {
    let memory = Memory::new();
    memory.put(BASE, b"/tmp/file\0");
    let mut times = Vec::new();
    for value in [11_i64, 22, 33, 44] {
        times.extend_from_slice(&value.to_le_bytes());
    }
    memory.put(BASE + 0x100, &times);
    let abi = FilesystemAbi::new(&memory, GuestArchitecture::X86_64);
    let crate::FsMutationPlan::SetTimes { target, times } = abi.utime(BASE, BASE + 0x100).unwrap() else {
        panic!("utime plan");
    };
    assert_eq!(target.path.as_bytes(), b"/tmp/file");
    assert_eq!(
        times,
        [
            crate::TimestampChange::Value {
                seconds: 11,
                nanoseconds: 0
            },
            crate::TimestampChange::Value {
                seconds: 22,
                nanoseconds: 0
            }
        ]
    );
    let crate::FsMutationPlan::SetTimes { times, .. } = abi.utimes(-100, BASE, BASE + 0x100).unwrap() else {
        panic!("utimes plan");
    };
    assert_eq!(
        times,
        [
            crate::TimestampChange::Value {
                seconds: 11,
                nanoseconds: 22_000
            },
            crate::TimestampChange::Value {
                seconds: 33,
                nanoseconds: 44_000
            }
        ]
    );
    assert!(matches!(
        abi.utimes(-100, BASE, u64::MAX),
        Err(crate::filesystem::plan::AbiError::Marshal(_))
    ));
    assert!(matches!(
        abi.utime(BASE, u64::MAX),
        Err(crate::filesystem::plan::AbiError::Marshal(_))
    ));
}

#[test]
fn open_stat_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let memory = Memory::new();
        memory.put(BASE, b"/tmp/\xff\0ignored");
        let abi = FilesystemAbi::new(&memory, architecture);
        let open = abi.openat(-100, BASE, 0, 0).unwrap();
        let stat = abi.stat_operand(-100, BASE, 0).unwrap();
        let access = abi.access(-100, BASE, 0, 0).unwrap();
        for path in [
            open.operand.path.as_bytes(),
            stat.path.as_bytes(),
            access.operand.path.as_bytes(),
        ] {
            assert_eq!(path, b"/tmp/\xff");
        }
    }
}

#[test]
fn open_flags_vary() {
    let memory = Memory::new();
    memory.put(BASE, b"link\0");
    for (architecture, directory, nofollow, temporary) in [
        (GuestArchitecture::Aarch64, 0x4_000, 0x8_000, 0x40_4000),
        (GuestArchitecture::X86_64, 0x10_000, 0x20_000, 0x41_0000),
    ] {
        let abi = FilesystemAbi::new(&memory, architecture);
        let plan = abi.openat(-100, BASE, nofollow, 0).unwrap();
        assert!(plan.operand.nofollow);
        assert_ne!(plan.intent.bits() & OpenIntent::NOFOLLOW, 0);
        assert_eq!(plan.intent.bits() & OpenIntent::DIRECTORY, 0);
        assert_eq!(plan.intent.bits() & OpenIntent::TEMPORARY, 0);
        let plan = abi.openat(-100, BASE, directory, 0).unwrap();
        assert_ne!(plan.intent.bits() & OpenIntent::DIRECTORY, 0);
        assert_eq!(plan.intent.bits() & OpenIntent::TEMPORARY, 0);
        let plan = abi.openat(-100, BASE, 0x100, 0).unwrap();
        assert!(plan.no_controlling_terminal);
        let plan = abi.openat(-100, BASE, temporary | 1, 0o600).unwrap();
        assert_ne!(plan.intent.bits() & OpenIntent::TEMPORARY, 0);
        assert_eq!(
            abi.openat(-100, BASE, temporary, 0o600),
            Err(crate::filesystem::AbiError::Invalid),
        );
        let plan = abi.openat(-100, BASE, temporary | 0x20_0000, 0o600).unwrap();
        assert_ne!(plan.intent.bits() & OpenIntent::PATH_ONLY, 0);
        assert_eq!(plan.intent.bits() & OpenIntent::TEMPORARY, 0);
    }
}

#[test]
fn chmodat2_validates_flags_before_path_and_preserves_empty_descriptor() {
    let memory = Memory::new();
    memory.put(BASE, b"\0");
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let abi = FilesystemAbi::new(&memory, architecture);
        assert_eq!(
            abi.chmodat(7, u64::MAX, 0o640, 0x2000),
            Err(crate::filesystem::AbiError::Invalid),
        );
        assert!(matches!(
            abi.chmodat(7, u64::MAX, 0o640, 0),
            Err(crate::filesystem::AbiError::Marshal(_))
        ));
        assert_eq!(
            abi.chmodat(7, BASE, 0o640, 0),
            Err(crate::filesystem::AbiError::NoEntry),
        );
        let crate::FsMutationPlan::Chmod { target, mode } = abi.chmodat(7, BASE, 0o10640, 0x1100).unwrap() else {
            panic!("unexpected chmod plan");
        };
        assert_eq!(target.directory.raw(), 7);
        assert!(target.allow_empty);
        assert!(target.nofollow);
        assert_eq!(mode, 0o640);
    }
}

#[test]
fn symlink_target_over_path_max_is_name_too_long() {
    let memory = Memory::new();
    memory.put(BASE, &vec![b'a'; 4097]);
    memory.put(BASE + 0x2000, b"/link\0");
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        assert_eq!(
            FilesystemAbi::new(&memory, architecture).symlinkat(BASE, -100, BASE + 0x2000),
            Err(crate::filesystem::AbiError::NameTooLong),
        );
    }
}

#[test]
fn path_termination_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let memory = Memory::new();
        memory.put(BASE, &vec![b'x'; 4096]);
        let abi = FilesystemAbi::new(&memory, architecture);
        assert_eq!(
            abi.path_operand(-100, BASE, false, false),
            Err(FilesystemMarshalError::NameTooLong),
        );
        memory.put(BASE, b"name\0suffix");
        assert_eq!(
            abi.path_operand(-100, BASE, false, false).unwrap().path.as_bytes(),
            b"name",
        );
    }
}

#[test]
fn openat2_size_access() {
    let memory = Memory::new();
    let abi = FilesystemAbi::new(&memory, GuestArchitecture::X86_64);
    assert_eq!(
        abi.openat2(-100, u64::MAX, u64::MAX, 8),
        Err(FilesystemMarshalError::Invalid),
    );
    assert_eq!(
        abi.openat2(-100, u64::MAX, u64::MAX, 4097),
        Err(FilesystemMarshalError::TooBig),
    );
    memory.put(BASE, b"x\0");
    let mut how = [0_u8; 24];
    how[..8].copy_from_slice(&u64::MAX.to_le_bytes());
    memory.put(BASE + 0x100, &how);
    assert_eq!(
        abi.openat2(-100, u64::MAX, BASE + 0x100, 24),
        Err(FilesystemMarshalError::Invalid),
    );

    let mut extended = [0_u8; 25];
    extended[24] = 1;
    memory.put(BASE + 0x100, &extended);
    assert_eq!(
        abi.openat2(-100, BASE, BASE + 0x100, extended.len()),
        Err(FilesystemMarshalError::TooBig),
    );
}

#[test]
fn open_empty_path_is_missing() {
    let memory = Memory::new();
    memory.put(BASE, b"\0");
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let abi = FilesystemAbi::new(&memory, architecture);
        assert_eq!(abi.openat(-100, BASE, 0, 0), Err(FilesystemMarshalError::NoEntry),);
        let how = [0_u8; 24];
        memory.put(BASE + 0x100, &how);
        assert_eq!(
            abi.openat2(-100, BASE, BASE + 0x100, how.len()),
            Err(FilesystemMarshalError::NoEntry),
        );
    }
}

#[test]
fn stat_getdents_writes() {
    let memory = Memory::new();
    let abi = FilesystemAbi::new(&memory, GuestArchitecture::Aarch64);
    let stat = abi.stage_stat(BASE, &metadata(), StatOutputKind::Stat).unwrap();
    assert_eq!(memory.get(BASE, 128), vec![0; 128]);
    stat.commit(&GuestMarshaller::new(&memory, GuestArchitecture::Aarch64))
        .unwrap();
    assert_eq!(u64::from_le_bytes(memory.get(BASE + 8, 8).try_into().unwrap()), 9);

    let dents = abi
        .stage_getdents(
            BASE + 0x200,
            24,
            &[DirectoryRecord {
                inode: 7,
                offset: 1,
                file_type: 4,
                name: b"x".to_vec(),
            }],
        )
        .unwrap();
    assert_eq!(dents.result_length, 24);
    dents
        .commit(&GuestMarshaller::new(&memory, GuestArchitecture::Aarch64))
        .unwrap();
    assert_eq!(memory.get(BASE + 0x200 + 16, 2), 24_u16.to_le_bytes());
    assert!(abi.stage_readlink(u64::MAX, 4, b"target").is_err());
}

#[test]
fn xattr_lock_execution() {
    let memory = Memory::new();
    let abi = FilesystemAbi::new(&memory, GuestArchitecture::X86_64);
    memory.put(BASE, b"/tmp/a\0");
    memory.put(BASE + 0x100, b"user.test\0");
    memory.put(BASE + 0x200, b"value");
    let target = abi.path_operand(-100, BASE, false, false).unwrap();
    assert!(
        abi.xattr_set(FilesystemTarget::Path(target.clone()), BASE + 0x100, BASE + 0x200, 5, 0,)
            .is_ok()
    );
    assert!(
        abi.xattr_set(FilesystemTarget::Path(target.clone()), BASE + 0x100, u64::MAX, 5, 3,)
            .is_err()
    );

    let mut lock = [0_u8; 32];
    lock[..2].copy_from_slice(&1_i16.to_le_bytes());
    lock[8..16].copy_from_slice(&4_i64.to_le_bytes());
    lock[16..24].copy_from_slice(&8_i64.to_le_bytes());
    memory.put(BASE + 0x300, &lock);
    let decoded = abi.file_lock(BASE + 0x300).unwrap();
    assert_eq!(decoded.lock_type, LockType::Write);
    assert_eq!(decoded.start, 4);
    assert!(abi.renameat2(-100, BASE, -100, BASE, 3,).is_err());
    assert!(abi.unlinkat(-100, BASE, 0x400).is_err());
}

#[test]
fn xattr_names_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let memory = Memory::new();
        let abi = FilesystemAbi::new(&memory, architecture);
        memory.put(BASE, b"/tmp/a\0");
        memory.put(BASE + 0x100, b"user.\xff\0");
        memory.put(BASE + 0x200, b"value");
        let target = abi.path_operand(-100, BASE, false, false).unwrap();
        let plan = abi
            .xattr_set(FilesystemTarget::Path(target.clone()), BASE + 0x100, BASE + 0x200, 5, 0)
            .unwrap();
        let XattrPlan::Set { name, .. } = plan else {
            panic!("set plan expected");
        };
        assert_eq!(name.as_bytes(), b"user.\xff");

        memory.put(BASE + 0x300, &[b'x'; 255]);
        memory.put(BASE + 0x300 + 255, b"\0");
        assert!(
            abi.xattr_set(FilesystemTarget::Path(target.clone()), BASE + 0x300, BASE + 0x200, 5, 0)
                .is_ok()
        );
        memory.put(BASE + 0x300, &[b'x'; 256]);
        assert_eq!(
            abi.xattr_set(FilesystemTarget::Path(target.clone()), BASE + 0x300, BASE + 0x200, 5, 0),
            Err(FilesystemMarshalError::NameTooLong),
        );
        memory.put(BASE + 0x100, b"user/a\0ignored");
        let plan = abi
            .xattr_set(FilesystemTarget::Path(target), BASE + 0x100, BASE + 0x200, 5, 0)
            .unwrap();
        let XattrPlan::Set { name, .. } = plan else {
            panic!("set plan expected");
        };
        assert_eq!(name.as_bytes(), b"user/a");
    }
}
