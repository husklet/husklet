use std::sync::Mutex;

use hl_isa::GuestArchitecture;
use hl_task::{Limit, Resource};

use crate::{
    ExecPlan, GuestAccess, GuestFault, GuestMarshaller, GuestMemory, PrctlPlan, ProcessAbi, ProcessMarshalError,
    ResourceUsage, WaitKind,
};

const BASE: u64 = 0x1000;

#[test]
fn resource_indices() {
    let memory = Memory::new();
    let abi = ProcessAbi::new(&memory, GuestArchitecture::Aarch64);
    let expected = [
        Resource::CpuTime,
        Resource::FileSize,
        Resource::Data,
        Resource::Stack,
        Resource::Core,
        Resource::ResidentSet,
        Resource::Processes,
        Resource::OpenFiles,
        Resource::LockedMemory,
        Resource::AddressSpace,
        Resource::Locks,
        Resource::PendingSignals,
        Resource::MessageQueue,
        Resource::Nice,
        Resource::RealtimePriority,
        Resource::RealtimeTime,
    ];
    for (index, resource) in expected.into_iter().enumerate() {
        assert_eq!(abi.resource(index as u32), Ok(resource));
    }
    assert_eq!(abi.resource(16), Err(ProcessMarshalError::Invalid));
}

struct Memory(Mutex<Vec<u8>>);

impl Memory {
    fn new() -> Self {
        Self(Mutex::new(vec![0; 0x8000]))
    }
    fn sized(size: usize) -> Self {
        Self(Mutex::new(vec![0; size]))
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
fn clone3_decodes_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let memory = Memory::new();
        let mut bytes = [0_u8; 88];
        for (index, value) in [0x100_u64, 1, 2, 3, 17, 4, 5, 6, 7, 8, 9].into_iter().enumerate() {
            bytes[index * 8..index * 8 + 8].copy_from_slice(&value.to_le_bytes());
        }
        memory.put(BASE, &bytes);
        let plan = ProcessAbi::new(&memory, architecture).clone3(BASE, 88).unwrap();
        assert_eq!(plan.flags, 0x100);
        assert_eq!(plan.parent_tid, 3);
        assert_eq!(plan.tls, 6);
        assert_eq!(plan.cgroup, 9);
    }
}

#[test]
fn clone3_rejects_extensions() {
    let memory = Memory::new();
    let abi = ProcessAbi::new(&memory, GuestArchitecture::X86_64);
    assert_eq!(abi.clone3(u64::MAX, 8), Err(ProcessMarshalError::Invalid));
    memory.put(BASE, &[0; 89]);
    memory.put(BASE + 88, &[1]);
    assert_eq!(abi.clone3(BASE, 89), Err(ProcessMarshalError::TooBig));
}

#[test]
fn legacy_clone_order() {
    let memory = Memory::new();
    let x86 = ProcessAbi::new(&memory, GuestArchitecture::X86_64)
        .clone_legacy(17, 1, 2, 3, 4)
        .unwrap();
    let arm = ProcessAbi::new(&memory, GuestArchitecture::Aarch64)
        .clone_legacy(17, 1, 2, 3, 4)
        .unwrap();
    assert_eq!((x86.child_tid, x86.tls), (3, 4));
    assert_eq!((arm.child_tid, arm.tls), (4, 3));
}

#[test]
fn exec_copies_plan() {
    let memory = Memory::new();
    memory.put(BASE, b"/bin/x\0");
    memory.put(BASE + 0x100, b"one\0");
    memory.put(BASE + 0x200, b"K=V\0");
    let mut argv = Vec::new();
    argv.extend_from_slice(&(BASE + 0x100).to_le_bytes());
    argv.extend_from_slice(&0_u64.to_le_bytes());
    memory.put(BASE + 0x300, &argv);
    let mut envp = Vec::new();
    envp.extend_from_slice(&(BASE + 0x200).to_le_bytes());
    envp.extend_from_slice(&0_u64.to_le_bytes());
    memory.put(BASE + 0x400, &envp);
    let plan = ProcessAbi::new(&memory, GuestArchitecture::Aarch64)
        .execve(BASE, BASE + 0x300, BASE + 0x400)
        .unwrap();
    memory.put(BASE + 0x100, b"bad\0");
    assert_eq!(plan.path, b"/bin/x");
    assert_eq!(plan.arguments, [b"one".to_vec()]);
    assert_eq!(plan.environment, [b"K=V".to_vec()]);
}

#[test]
fn exec_comm() {
    let plan = |path: &[u8]| ExecPlan {
        directory: None,
        path: path.to_vec(),
        arguments: Vec::new(),
        environment: Vec::new(),
        flags: 0,
    };

    assert_eq!(plan(b"./selfexe").comm(), *b"selfexe\0\0\0\0\0\0\0\0\0");
    assert_eq!(plan(b"/proc/self/exe").comm(), *b"exe\0\0\0\0\0\0\0\0\0\0\0\0\0");
    assert_eq!(plan(b"/long-executable-name").comm(), *b"long-executable\0");
}

#[test]
fn exec_accepts_null_vectors_as_empty() {
    let memory = Memory::new();
    memory.put(BASE, b"/bin/x\0");
    let plan = ProcessAbi::new(&memory, GuestArchitecture::Aarch64)
        .execve(BASE, 0, 0)
        .unwrap();
    assert!(plan.arguments.is_empty());
    assert!(plan.environment.is_empty());
}

#[test]
fn exec_string_limit() {
    let memory = Memory::sized(0x2_1000);
    memory.put(BASE, b"/bin/x\0");
    let string = BASE + 0x100;
    memory.put(string, &vec![b'x'; 32 * 4096]);
    memory.put(string + 32 * 4096, &[0]);
    memory.put(BASE + 0x20, &string.to_le_bytes());
    memory.put(BASE + 0x28, &0_u64.to_le_bytes());
    memory.put(BASE + 0x30, &0_u64.to_le_bytes());
    let result = ProcessAbi::new(&memory, GuestArchitecture::Aarch64).execve(BASE, BASE + 0x20, BASE + 0x30);
    assert_eq!(result, Err(ProcessMarshalError::TooBig));
}

#[test]
fn exec_path_first() {
    let memory = Memory::new();
    memory.put(BASE, b"/missing\0");
    let abi = ProcessAbi::new(&memory, GuestArchitecture::Aarch64);
    let plan = abi.exec_path(None, BASE, 0).unwrap();
    assert_eq!(plan.path, b"/missing");
    assert!(plan.arguments.is_empty());
    assert_eq!(abi.exec_vectors(plan, u64::MAX, 0), Err(ProcessMarshalError::Fault),);
}

#[test]
fn wait_identity_values() {
    let memory = Memory::new();
    let abi = ProcessAbi::new(&memory, GuestArchitecture::X86_64);
    assert_eq!(abi.wait4(-8, 0, 1, 0).unwrap().kind, WaitKind::ProcessGroup(8));
    let change = abi.identity_user(u32::MAX, 12, u32::MAX);
    assert!(matches!(
        change,
        crate::IdentityChange::User {
            real: None,
            effective: Some(12),
            saved: None
        }
    ));
    assert_eq!(abi.waitid(9, 0, BASE, 2, 0), Err(ProcessMarshalError::Invalid),);
}

#[test]
fn limit_usage_commit() {
    let memory = Memory::new();
    let abi = ProcessAbi::new(&memory, GuestArchitecture::X86_64);
    let limit = Limit::new(3, 4).unwrap();
    let staged = abi.stage_limit(BASE, limit).unwrap();
    assert_eq!(memory.get(BASE, 16), vec![0; 16]);
    staged
        .commit(&GuestMarshaller::new(&memory, GuestArchitecture::X86_64))
        .unwrap();
    assert_eq!(memory.get(BASE, 8), 3_u64.to_le_bytes());

    let usage = abi
        .stage_usage(
            BASE + 0x100,
            ResourceUsage {
                user_seconds: 7,
                involuntary_switches: 11,
                ..ResourceUsage::default()
            },
        )
        .unwrap();
    usage
        .commit(&GuestMarshaller::new(&memory, GuestArchitecture::X86_64))
        .unwrap();
    assert_eq!(memory.get(BASE + 0x100, 8), 7_i64.to_le_bytes());
    assert_eq!(memory.get(BASE + 0x100 + 120, 8), 11_i64.to_le_bytes());
}

#[test]
fn prctl_validates_names() {
    let memory = Memory::new();
    memory.put(BASE, b"worker\0");
    let abi = ProcessAbi::new(&memory, GuestArchitecture::Aarch64);
    assert_eq!(abi.prctl([4, u64::MAX, 0, 0, 0, 0]), Err(ProcessMarshalError::Invalid),);
    assert_eq!(abi.prctl([15, u64::MAX, 0, 0, 0, 0]), Err(ProcessMarshalError::Fault),);
    assert_eq!(
        abi.prctl([15, BASE, 11, 12, 13, 14]),
        Ok(PrctlPlan::SetName(*b"worker\0\0\0\0\0\0\0\0\0\0")),
    );
    assert_eq!(
        abi.prctl([16, BASE + 32, 11, 12, 13, 14]),
        Ok(PrctlPlan::GetName { destination: BASE + 32 }),
    );
    assert_eq!(abi.prctl([3, 11, 12, 13, 14, 15]), Ok(PrctlPlan::GetDumpable));
}
