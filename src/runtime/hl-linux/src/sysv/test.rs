use std::sync::Mutex;

use hl_isa::GuestArchitecture;

use crate::{
    GuestAccess, GuestFault, GuestMemory, IpcCommand, IpcPermissions, MessageInfo, MessageQueueStatus,
    SemaphoreControlPlan, SemaphoreInfo, SemaphoreOperation, SemaphoreStatus, SharedMemoryInfo, SharedMemoryStatus,
    ShmInfo, SysvAbi, SysvIdentifier, SysvMarshalError,
};

const BASE: u64 = 0x1000;

struct Memory {
    bytes: Mutex<Vec<u8>>,
    readable: bool,
    writable: bool,
}

impl Memory {
    fn new(length: usize) -> Self {
        Self {
            bytes: Mutex::new(vec![0; length]),
            readable: true,
            writable: true,
        }
    }

    fn inaccessible(length: usize) -> Self {
        Self {
            bytes: Mutex::new(vec![0; length]),
            readable: false,
            writable: false,
        }
    }

    fn offset(address: u64) -> Option<usize> {
        usize::try_from(address.checked_sub(BASE)?).ok()
    }

    fn span(&self, address: u64, length: usize, access: GuestAccess) -> Result<(usize, usize), GuestFault> {
        let allowed = match access {
            GuestAccess::Read => self.readable,
            GuestAccess::Write => self.writable,
        };
        let offset = Self::offset(address)
            .filter(|offset| allowed && *offset < self.bytes.lock().unwrap().len())
            .ok_or(GuestFault { address, access })?;
        let available = self.bytes.lock().unwrap().len() - offset;
        Ok((offset, available.min(length)))
    }

    fn put(&self, address: u64, bytes: &[u8]) {
        let offset = Self::offset(address).unwrap();
        self.bytes.lock().unwrap()[offset..offset + bytes.len()].copy_from_slice(bytes);
    }

    fn get(&self, address: u64, length: usize) -> Vec<u8> {
        let offset = Self::offset(address).unwrap();
        self.bytes.lock().unwrap()[offset..offset + length].to_vec()
    }

    fn golden_permissions(&self, length: usize) -> Vec<u8> {
        let mut bytes = vec![0; length];
        put_u32(&mut bytes, 0, (-2_i32) as u32);
        put_u32(&mut bytes, 4, 3);
        put_u32(&mut bytes, 8, 4);
        put_u32(&mut bytes, 12, 5);
        put_u32(&mut bytes, 16, 6);
        put_u32(&mut bytes, 20, 0x1234_5678);
        put_u16(&mut bytes, 24, 0x9abc);
        bytes
    }
}

impl GuestMemory for Memory {
    fn probe(&self, address: u64, length: usize, access: GuestAccess) -> Result<usize, GuestFault> {
        self.span(address, length, access).map(|(_, span)| span)
    }

    fn read(&self, address: u64, destination: &mut [u8]) -> Result<usize, GuestFault> {
        let (offset, span) = self.span(address, destination.len(), GuestAccess::Read)?;
        destination[..span].copy_from_slice(&self.bytes.lock().unwrap()[offset..offset + span]);
        Ok(span)
    }

    fn write(&self, address: u64, source: &[u8]) -> Result<usize, GuestFault> {
        let (offset, span) = self.span(address, source.len(), GuestAccess::Write)?;
        self.bytes.lock().unwrap()[offset..offset + span].copy_from_slice(&source[..span]);
        Ok(span)
    }
}

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn semaphore_operations_layout() {
    let memory = Memory::new(4096);
    let mut raw = [0_u8; 12];
    put_u16(&mut raw, 0, 0x1234);
    put_u16(&mut raw, 2, (-2_i16) as u16);
    put_u16(&mut raw, 4, 0x1000);
    put_u16(&mut raw, 6, 0x5678);
    put_u16(&mut raw, 8, 3);
    put_u16(&mut raw, 10, 0x800);
    memory.put(BASE, &raw);

    let plan = SysvAbi::new(&memory, GuestArchitecture::Aarch64)
        .semop(7, BASE, 2, None)
        .unwrap();
    assert_eq!(
        plan.operations,
        vec![
            SemaphoreOperation {
                index: 0x1234,
                delta: -2,
                flags: 0x1000
            },
            SemaphoreOperation {
                index: 0x5678,
                delta: 3,
                flags: 0x800
            },
        ]
    );
}

#[test]
fn messages_use_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let memory = Memory::new(4096);
        let mut raw = Vec::from(0x0102_0304_0506_0708_i64.to_le_bytes());
        raw.extend_from_slice(b"payload");
        memory.put(BASE, &raw);
        let plan = SysvAbi::new(&memory, architecture).msgsnd(4, BASE, 7, 0).unwrap();
        assert_eq!(plan.message_type, 0x0102_0304_0506_0708);
        assert_eq!(plan.bytes, b"payload");
    }
}

#[test]
fn scalar_semctl_pointer() {
    let memory = Memory::inaccessible(4096);
    let abi = SysvAbi::new(&memory, GuestArchitecture::X86_64);

    assert_eq!(
        abi.semctl(12, 3, 12, 0xffff_ffff_ffff_f000),
        Ok(SemaphoreControlPlan::Scalar {
            identifier: SysvIdentifier(12),
            index: 3,
            command: IpcCommand::GetValue,
            value: 0,
        })
    );
    assert_eq!(
        abi.semctl(12, 3, 16, 0x1_0000_8001),
        Ok(SemaphoreControlPlan::Scalar {
            identifier: SysvIdentifier(12),
            index: 3,
            command: IpcCommand::SetValue,
            value: 0x8001,
        })
    );
}

#[test]
fn pointer_commands_selection() {
    let memory = Memory::inaccessible(4096);
    let abi = SysvAbi::new(&memory, GuestArchitecture::Aarch64);

    assert_eq!(abi.shmctl(1, 0x7fff, 0), Err(SysvMarshalError::Invalid));
    assert_eq!(abi.msgctl(1, 0x7fff, 0), Err(SysvMarshalError::Invalid));
    assert_eq!(abi.semctl(1, 0, 0x7fff, 0), Err(SysvMarshalError::Invalid));
    assert!(abi.shmctl(1, 1, BASE).is_ok());
    assert!(abi.msgctl(1, 1, BASE).is_ok());
    assert!(abi.semctl(1, 0, 1, BASE).is_ok());
    assert_eq!(abi.import_shared_status(BASE), Err(SysvMarshalError::Fault));
    assert_eq!(abi.import_message_status(BASE), Err(SysvMarshalError::Fault));
    assert_eq!(abi.import_semaphore_status(BASE), Err(SysvMarshalError::Fault));
}

#[test]
fn shared_status_layout() {
    let memory = Memory::new(4096);
    let mut raw = [0_u8; 112];
    put_u32(&mut raw, 0, (-7_i32) as u32);
    put_u32(&mut raw, 4, 11);
    put_u32(&mut raw, 8, 12);
    put_u32(&mut raw, 12, 13);
    put_u32(&mut raw, 16, 14);
    put_u32(&mut raw, 20, 0o765);
    put_u16(&mut raw, 24, 0x4321);
    put_u64(&mut raw, 48, 0x1122_3344_5566_7788);
    put_u64(&mut raw, 56, 101);
    put_u64(&mut raw, 64, 102);
    put_u64(&mut raw, 72, 103);
    put_u32(&mut raw, 80, 104);
    put_u32(&mut raw, 84, 105);
    put_u64(&mut raw, 88, 106);
    memory.put(BASE, &raw);

    let value = SysvAbi::new(&memory, GuestArchitecture::Aarch64)
        .import_shared_status(BASE)
        .unwrap();
    assert_eq!(value.permissions.key, -7);
    assert_eq!(value.permissions.sequence, 0x4321);
    assert_eq!(value.size, 0x1122_3344_5566_7788);
    assert_eq!(value.attached_at, 101);
    assert_eq!(value.detached_at, 102);
    assert_eq!(value.changed_at, 103);
    assert_eq!(value.creator_pid, 104);
    assert_eq!(value.last_pid, 105);
    assert_eq!(value.attaches, 106);
}

#[test]
fn staged_copyout_commit() {
    let memory = Memory::new(4096);
    let abi = SysvAbi::new(&memory, GuestArchitecture::Aarch64);
    let status = crate::SharedMemoryStatus::default();
    let staged = abi.stage_shared_status(BASE, status).unwrap();

    assert_eq!(memory.get(BASE, 112), vec![0; 112]);
    staged
        .commit(&crate::GuestMarshaller::new(&memory, GuestArchitecture::Aarch64))
        .unwrap();
    assert_eq!(memory.get(BASE, 112), vec![0; 112]);
}

fn permissions() -> IpcPermissions {
    IpcPermissions {
        key: -2,
        uid: 3,
        gid: 4,
        creator_uid: 5,
        creator_gid: 6,
        mode: 0x1234_5678,
        sequence: 0x9abc,
    }
}

fn commit<M: GuestMemory>(memory: &M, architecture: GuestArchitecture, staged: crate::StagedSysvCopyout) {
    staged
        .commit(&crate::GuestMarshaller::new(memory, architecture))
        .unwrap();
}

#[test]
fn shared_message_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let memory = Memory::new(4096);
        let abi = SysvAbi::new(&memory, architecture);
        let shared = SharedMemoryStatus {
            permissions: permissions(),
            size: 7,
            attached_at: 8,
            detached_at: 9,
            changed_at: 10,
            creator_pid: 11,
            last_pid: 12,
            attaches: 13,
        };
        commit(&memory, architecture, abi.stage_shared_status(BASE, shared).unwrap());
        let mut expected = memory.golden_permissions(112);
        put_u64(&mut expected, 48, 7);
        put_u64(&mut expected, 56, 8);
        put_u64(&mut expected, 64, 9);
        put_u64(&mut expected, 72, 10);
        put_u32(&mut expected, 80, 11);
        put_u32(&mut expected, 84, 12);
        put_u64(&mut expected, 88, 13);
        assert_eq!(memory.get(BASE, 112), expected);
        assert_eq!(abi.import_shared_status(BASE), Ok(shared));

        let message = MessageQueueStatus {
            permissions: permissions(),
            sent_at: 14,
            received_at: 15,
            changed_at: 16,
            bytes: 17,
            messages: 18,
            maximum_bytes: 19,
            last_sender: 20,
            last_receiver: 21,
        };
        commit(&memory, architecture, abi.stage_message_status(BASE, message).unwrap());
        let mut expected = memory.golden_permissions(120);
        for (offset, value) in [(48, 14), (56, 15), (64, 16), (72, 17), (80, 18), (88, 19)] {
            put_u64(&mut expected, offset, value);
        }
        put_u32(&mut expected, 96, 20);
        put_u32(&mut expected, 100, 21);
        assert_eq!(memory.get(BASE, 120), expected);
        assert_eq!(abi.import_message_status(BASE), Ok(message));
    }
}

#[test]
fn semaphore_status_geometry() {
    for (architecture, length, changed, count) in [
        (GuestArchitecture::Aarch64, 88, 56, 64),
        (GuestArchitecture::X86_64, 104, 64, 80),
    ] {
        let memory = Memory::new(4096);
        let abi = SysvAbi::new(&memory, architecture);
        let value = SemaphoreStatus {
            permissions: permissions(),
            operated_at: 22,
            changed_at: 23,
            semaphores: 24,
        };
        commit(&memory, architecture, abi.stage_semaphore_status(BASE, value).unwrap());
        let mut expected = memory.golden_permissions(length);
        put_u64(&mut expected, 48, 22);
        put_u64(&mut expected, changed, 23);
        put_u64(&mut expected, count, 24);
        assert_eq!(memory.get(BASE, length), expected);
        assert_eq!(abi.import_semaphore_status(BASE), Ok(value));
    }
}

#[test]
fn information_structure_layout() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let memory = Memory::new(4096);
        let abi = SysvAbi::new(&memory, architecture);
        let shared = SharedMemoryInfo {
            maximum_size: 1,
            minimum_size: 2,
            maximum_segments: 3,
            maximum_process_segments: 4,
            maximum_pages: 5,
        };
        commit(&memory, architecture, abi.stage_shared_info(BASE, shared).unwrap());
        let mut expected = vec![0; 72];
        for (index, value) in (1_u64..=5).enumerate() {
            put_u64(&mut expected, index * 8, value);
        }
        assert_eq!(memory.get(BASE, 72), expected);

        let usage = ShmInfo {
            used_identifiers: -1,
            total_pages: 2,
            resident_pages: 3,
            swapped_pages: 4,
            swap_attempts: 5,
            swap_successes: 6,
        };
        commit(&memory, architecture, abi.stage_shm_info(BASE, usage).unwrap());
        let mut expected = vec![0; 48];
        put_u32(&mut expected, 0, u32::MAX);
        for (index, value) in (2_u64..=6).enumerate() {
            put_u64(&mut expected, 8 + index * 8, value);
        }
        assert_eq!(memory.get(BASE, 48), expected);

        let semaphore = SemaphoreInfo {
            values: [1, -2, 3, -4, 5, -6, 7, -8, 9, -10],
        };
        commit(
            &memory,
            architecture,
            abi.stage_semaphore_info(BASE, semaphore).unwrap(),
        );
        let mut expected = vec![0; 40];
        for (index, value) in semaphore.values.into_iter().enumerate() {
            put_u32(&mut expected, index * 4, value as u32);
        }
        assert_eq!(memory.get(BASE, 40), expected);

        let message = MessageInfo {
            values: [11, 12, 13, 14, 15, 16, 17],
            segments: 0x2345,
        };
        commit(&memory, architecture, abi.stage_message_info(BASE, message).unwrap());
        let mut expected = vec![0; 32];
        for (index, value) in message.values.into_iter().enumerate() {
            put_u32(&mut expected, index * 4, value as u32);
        }
        put_u16(&mut expected, 28, 0x2345);
        assert_eq!(memory.get(BASE, 32), expected);
    }
}

#[test]
fn message_receive_type() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let memory = Memory::new(4096);
        let abi = SysvAbi::new(&memory, architecture);
        let staged = abi.stage_message_receive(BASE, -9, b"abc").unwrap();
        commit(&memory, architecture, staged);
        let mut expected = Vec::from((-9_i64).to_le_bytes());
        expected.extend_from_slice(b"abc");
        assert_eq!(memory.get(BASE, 11), expected);
    }
}

#[test]
fn validation_precedence_access() {
    let memory = Memory::inaccessible(4096);
    let abi = SysvAbi::new(&memory, GuestArchitecture::Aarch64);
    assert_eq!(abi.semop(1, 0, 0, Some(0)), Err(SysvMarshalError::Invalid));
    assert_eq!(abi.semop(1, 0, 501, Some(0)), Err(SysvMarshalError::TooBig));
    assert_eq!(abi.semop(1, 0, 1, Some(BASE)), Err(SysvMarshalError::Fault));
    assert_eq!(abi.msgsnd(1, 0, 8_193, 0), Err(SysvMarshalError::Invalid));
    assert_eq!(abi.msgsnd(1, 0, 1, 0), Err(SysvMarshalError::Fault));
    assert_eq!(abi.msgrcv(1, 0, 1, 0, 0), Err(SysvMarshalError::Fault));

    let memory = Memory::new(4096);
    memory.put(BASE, &[0; 8]);
    assert_eq!(
        SysvAbi::new(&memory, GuestArchitecture::X86_64).msgsnd(1, BASE, 0, 0),
        Err(SysvMarshalError::Invalid)
    );
    let mut operation = [0_u8; 6];
    put_u16(&mut operation, 4, 0);
    memory.put(BASE, &operation);
    let mut timeout = [0_u8; 16];
    put_u64(&mut timeout, 8, 1_000_000_000);
    memory.put(BASE + 32, &timeout);
    assert_eq!(
        SysvAbi::new(&memory, GuestArchitecture::Aarch64).semop(1, BASE, 1, Some(BASE + 32)),
        Err(SysvMarshalError::Invalid)
    );
}

#[test]
fn control_plans_arrays() {
    let memory = Memory::inaccessible(4096);
    let abi = SysvAbi::new(&memory, GuestArchitecture::Aarch64);
    assert!(matches!(
        abi.shmctl(7, 15, BASE).unwrap(),
        crate::SharedMemoryControlPlan::IndexStat {
            index: crate::SysvRawIndex(7),
            any: true,
            output: BASE
        }
    ));
    assert!(matches!(
        abi.semctl(8, 0, 13, BASE).unwrap(),
        SemaphoreControlPlan::Array {
            identifier: SysvIdentifier(8),
            command: IpcCommand::GetAll,
            address: BASE
        }
    ));
}

#[test]
fn staging_rejects_it() {
    let memory = Memory::new(111);
    let abi = SysvAbi::new(&memory, GuestArchitecture::Aarch64);
    assert_eq!(
        abi.stage_shared_status(BASE, SharedMemoryStatus::default()),
        Err(SysvMarshalError::Fault)
    );
    assert_eq!(memory.get(BASE, 111), vec![0; 111]);
}

#[test]
fn staging_pointer_wrap_is_fault() {
    let memory = Memory::new(128);
    let abi = SysvAbi::new(&memory, GuestArchitecture::X86_64);
    assert_eq!(
        abi.stage_shared_status(u64::MAX, SharedMemoryStatus::default()),
        Err(SysvMarshalError::Fault),
    );
}
