#![allow(clippy::new_ret_no_self)]

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use hl_descriptor::{DescriptorFlags, DescriptorTable, OpenFileDescription};
use hl_isa::GuestArchitecture;
use hl_linux::{CANONICAL_SYSCALLS, FilesystemSyscalls, GuestAccess, GuestFault, GuestMemory, LinuxResult};
use hl_vfs::{FileMetadata, GuestPath, XattrFlags, XattrName};

use crate::{
    DirectoryBaseLease, PreparedXattrMutation, ResolvedPathLease, RuntimeFilesystemSyscalls, RuntimePathError,
    RuntimePathHost, RuntimeXattrMutation,
};

use super::*;

#[derive(Clone)]
struct Memory {
    bytes: Arc<Mutex<Vec<u8>>>,
    reject_writes: Arc<AtomicBool>,
}

impl Memory {
    fn new() -> Self {
        Self {
            bytes: Arc::new(Mutex::new(vec![0; 1024])),
            reject_writes: Arc::new(AtomicBool::new(false)),
        }
    }

    fn put(&self, address: usize, bytes: &[u8]) {
        self.bytes.lock().unwrap()[address..address + bytes.len()].copy_from_slice(bytes);
    }

    fn get(&self, address: usize, length: usize) -> Vec<u8> {
        self.bytes.lock().unwrap()[address..address + length].to_vec()
    }
}

impl GuestMemory for Memory {
    fn probe(&self, address: u64, length: usize, access: GuestAccess) -> Result<usize, GuestFault> {
        if access == GuestAccess::Write && self.reject_writes.load(Ordering::Acquire) {
            return Err(GuestFault { address, access });
        }
        Ok(self
            .bytes
            .lock()
            .unwrap()
            .len()
            .saturating_sub(address as usize)
            .min(length))
    }

    fn read(&self, address: u64, output: &mut [u8]) -> Result<usize, GuestFault> {
        let bytes = self.bytes.lock().unwrap();
        let count = output.len().min(bytes.len().saturating_sub(address as usize));
        output[..count].copy_from_slice(&bytes[address as usize..address as usize + count]);
        Ok(count)
    }

    fn write(&self, address: u64, input: &[u8]) -> Result<usize, GuestFault> {
        if self.reject_writes.load(Ordering::Acquire) {
            return Err(GuestFault {
                address,
                access: GuestAccess::Write,
            });
        }
        let mut bytes = self.bytes.lock().unwrap();
        let count = input.len().min(bytes.len().saturating_sub(address as usize));
        bytes[address as usize..address as usize + count].copy_from_slice(&input[..count]);
        Ok(count)
    }
}

#[derive(Debug, Default)]
struct State {
    values: Mutex<BTreeMap<XattrName, Vec<u8>>>,
    last_path: Mutex<Vec<u8>>,
    fail_commit: AtomicBool,
    rollbacks: AtomicUsize,
    descriptor_resolutions: AtomicUsize,
}

struct Node(Arc<State>);

impl fmt::Debug for Node {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("XattrNode")
    }
}

impl ResolvedPathLease for Node {
    fn metadata(&self) -> Result<FileMetadata, RuntimePathError> {
        Err(RuntimePathError::Unsupported)
    }

    fn read_link(&self) -> Result<Vec<u8>, RuntimePathError> {
        Err(RuntimePathError::Unsupported)
    }

    fn access(&self, _plan: &hl_linux::AccessPlan) -> Result<(), RuntimePathError> {
        Err(RuntimePathError::Unsupported)
    }

    fn xattr_get(&self, name: &XattrName) -> Result<Vec<u8>, Errno> {
        self.0.values.lock().unwrap().get(name).cloned().ok_or(Errno::ENODATA)
    }

    fn xattr_list(&self) -> Result<Vec<u8>, Errno> {
        let values = self.0.values.lock().unwrap();
        let mut output = Vec::new();
        for name in values.keys() {
            output.extend_from_slice(name.as_bytes());
            output.push(0);
        }
        Ok(output)
    }

    fn prepare_xattr(&self, mutation: RuntimeXattrMutation) -> Result<Box<dyn PreparedXattrMutation>, Errno> {
        Ok(Box::new(Transaction {
            state: self.0.clone(),
            mutation: Some(mutation),
        }))
    }
}

#[derive(Debug)]
struct Transaction {
    state: Arc<State>,
    mutation: Option<RuntimeXattrMutation>,
}

impl PreparedXattrMutation for Transaction {
    fn commit(&mut self) -> Result<(), Errno> {
        if self.state.fail_commit.load(Ordering::Acquire) {
            return Err(Errno::EIO);
        }
        let mutation = self.mutation.take().ok_or(Errno::EIO)?;
        let mut values = self.state.values.lock().unwrap();
        match mutation {
            RuntimeXattrMutation::Set { name, value, flags } => {
                let exists = values.contains_key(&name);
                if flags == XattrFlags::Create && exists {
                    return Err(Errno::EEXIST);
                }
                if flags == XattrFlags::Replace && !exists {
                    return Err(Errno::ENODATA);
                }
                values.insert(name, value);
            }
            RuntimeXattrMutation::Remove { name } => {
                if values.remove(&name).is_none() {
                    return Err(Errno::ENODATA);
                }
            }
        }
        Ok(())
    }

    fn rollback(self: Box<Self>) {
        self.state.rollbacks.fetch_add(1, Ordering::AcqRel);
    }
}

#[derive(Debug)]
struct DescriptorObject;
impl OpenFileDescription for DescriptorObject {}

struct Host(Arc<State>);

impl RuntimePathHost for Host {
    fn root_base(&self) -> Result<DirectoryBaseLease, RuntimePathError> {
        Ok(DirectoryBaseLease::root(GuestPath::new("/").unwrap()))
    }

    fn descriptor_base(&self, lease: hl_descriptor::OperationLease) -> Result<DirectoryBaseLease, RuntimePathError> {
        Ok(DirectoryBaseLease::descriptor(lease, GuestPath::new("/base").unwrap()))
    }

    fn descriptor_node(
        &self,
        _lease: hl_descriptor::OperationLease,
    ) -> Result<Box<dyn ResolvedPathLease>, RuntimePathError> {
        self.0.descriptor_resolutions.fetch_add(1, Ordering::AcqRel);
        Ok(Box::new(Node(self.0.clone())))
    }

    fn resolve(
        &self,
        _base: &DirectoryBaseLease,
        operand: &hl_linux::PathOperand,
    ) -> Result<Box<dyn ResolvedPathLease>, RuntimePathError> {
        *self.0.last_path.lock().unwrap() = operand.path.as_bytes().to_vec();
        Ok(Box::new(Node(self.0.clone())))
    }

    fn prepare_open(
        &self,
        _base: &DirectoryBaseLease,
        _plan: &hl_linux::OpenAbiPlan,
        _identity: &hl_vfs::AccessIdentity,
    ) -> Result<Box<dyn crate::PreparedPathOpen>, RuntimePathError> {
        Err(RuntimePathError::Unsupported)
    }
}

struct Fixture;

impl Fixture {
    fn new(architecture: GuestArchitecture) -> (RuntimeFilesystemSyscalls<Memory>, Memory, Arc<State>, i32) {
        let memory = Memory::new();
        let state = Arc::new(State::default());
        let descriptors = Arc::new(DescriptorTable::new(8).unwrap());
        let descriptor = descriptors
            .install(0, Arc::new(DescriptorObject), DescriptorFlags::default())
            .unwrap();
        let runtime = RuntimeFilesystemSyscalls::new(descriptors, memory.clone(), architecture)
            .with_path_host(Arc::new(Host(state.clone())));
        (runtime, memory, state, descriptor)
    }

    fn arguments(name: &str, target: u64) -> [u64; 6] {
        if name.contains("setxattr") {
            [target, 48, 80, 1, 0, 0]
        } else if name.contains("getxattr") {
            [target, 48, 128, 1, 0, 0]
        } else if name.contains("listxattr") {
            [target, 128, 32, 0, 0, 0]
        } else {
            [target, 48, 0, 0, 0, 0]
        }
    }
}

#[test]
fn all_bytes_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let (runtime, memory, state, descriptor) = Fixture::new(architecture);
        memory.put(16, b"/tmp/\xfe\0");
        memory.put(48, b"user.\xff\0");
        memory.put(80, b"value");
        assert_eq!(
            runtime.path_xattr("setxattr", [16, 48, 80, 5, 1, 0]),
            LinuxResult::Value(0),
        );
        assert_eq!(&*state.last_path.lock().unwrap(), b"/tmp/\xfe");
        assert_eq!(
            runtime.path_xattr("getxattr", [16, 48, 128, 5, 0, 0]),
            LinuxResult::Value(5),
        );
        assert_eq!(memory.get(128, 5), b"value");
        assert_eq!(
            runtime.path_xattr("listxattr", [16, 160, 16, 0, 0, 0]),
            LinuxResult::Value(7),
        );
        assert_eq!(memory.get(160, 7), b"user.\xff\0");
        assert_eq!(
            runtime.path_xattr("fgetxattr", [descriptor as u64, 48, 192, 5, 0, 0]),
            LinuxResult::Value(5),
        );
        assert_eq!(state.descriptor_resolutions.load(Ordering::Acquire), 1);
        assert_eq!(
            runtime.path_xattr("lremovexattr", [16, 48, 0, 0, 0, 0]),
            LinuxResult::Value(0),
        );
    }
}

#[test]
fn probe_publish_atomically() {
    let (runtime, memory, state, _) = Fixture::new(GuestArchitecture::Aarch64);
    memory.put(16, b"/tmp/a\0");
    memory.put(48, b"user.k\0");
    state
        .values
        .lock()
        .unwrap()
        .insert(XattrName::new(b"user.k").unwrap(), b"value".to_vec());
    assert_eq!(
        runtime.path_xattr("getxattr", [16, 48, 999, 0, 0, 0]),
        LinuxResult::Value(5),
    );
    assert_eq!(
        runtime.path_xattr("getxattr", [16, 48, 999, 4, 0, 0]),
        LinuxResult::Error(Errno::ERANGE),
    );
    memory.reject_writes.store(true, Ordering::Release);
    assert_eq!(
        runtime.path_xattr("getxattr", [16, 48, 128, 5, 0, 0]),
        LinuxResult::Error(Errno::EFAULT),
    );
    assert_eq!(memory.get(128, 5), [0; 5]);
    memory.reject_writes.store(false, Ordering::Release);
    memory.put(80, b"new");
    state.fail_commit.store(true, Ordering::Release);
    assert_eq!(
        runtime.path_xattr("setxattr", [16, 48, 80, 3, 0, 0]),
        LinuxResult::Error(Errno::EIO),
    );
    assert_eq!(
        state.values.lock().unwrap()[&XattrName::new(b"user.k").unwrap()],
        b"value",
    );
    assert_eq!(state.rollbacks.load(Ordering::Acquire), 1);
}

#[test]
fn dispatcher_instead_enosys() {
    let (mut runtime, memory, _, descriptor) = Fixture::new(GuestArchitecture::Aarch64);
    memory.put(16, b"/tmp/a\0");
    memory.put(48, b"user.k\0");
    memory.put(80, b"x");
    for definition in CANONICAL_SYSCALLS
        .iter()
        .filter(|definition| definition.operation.name.contains("xattr"))
    {
        let name = definition.operation.name;
        let target = if name.starts_with('f') { descriptor as u64 } else { 16 };
        let arguments = Fixture::arguments(name, target);
        assert_ne!(
            FilesystemSyscalls::handle(&mut runtime, definition.operation, arguments,),
            LinuxResult::Error(Errno::ENOSYS),
            "{name}",
        );
    }
}
