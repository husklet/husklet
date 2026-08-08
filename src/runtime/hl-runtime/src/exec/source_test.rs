use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use hl_descriptor::{DescriptorFlags, DescriptorTable, ObjectError, OpenFileDescription};
use hl_isa::GuestArchitecture;
use hl_linux::{ExecPlan, GuestAccess, GuestFault, GuestMemory, ProcessAbi};
use hl_loader::{ImageRole, ImageSource, ImageSourceError};
use hl_task::{ProcessCredentials, ProcessLimits, RegistryConfig, TaskRegistry};
use hl_vfs::{FileIdentity, FileKind, FileMetadata, FileTimestamp, GuestPath, Permissions};

use crate::{
    DescriptorImageSlot, DirectoryBaseLease, ExecutablePath, PreparedExecParticipant, PreparedPathOpen,
    ResolvedPathLease, RuntimeExecError, RuntimePathError, RuntimePathHost, SourceFactory, VfsSourceFactory,
};

#[path = "path_test.rs"]
mod path_test;

#[derive(Debug)]
struct FileObject;

impl OpenFileDescription for FileObject {
    fn read_at(&self, _: u64, _: &mut [u8]) -> Result<usize, ObjectError> {
        Ok(0)
    }
}

#[derive(Debug)]
pub(super) struct Node {
    metadata: FileMetadata,
    bytes: Vec<u8>,
    executable: bool,
    reads: AtomicUsize,
}

impl Node {
    pub(super) fn regular(bytes: &[u8], executable: bool) -> Arc<Self> {
        Arc::new(Self {
            metadata: Self::metadata(
                FileKind::Regular,
                bytes.len() as u64,
                if executable { 0o755 } else { 0o644 },
            ),
            bytes: bytes.to_vec(),
            executable,
            reads: AtomicUsize::new(0),
        })
    }

    fn kind(kind: FileKind) -> Arc<Self> {
        Arc::new(Self {
            metadata: Self::metadata(kind, 0, 0o755),
            bytes: Vec::new(),
            executable: true,
            reads: AtomicUsize::new(0),
        })
    }

    fn metadata(kind: FileKind, size: u64, permissions: u16) -> FileMetadata {
        let timestamp = FileTimestamp::new(0, 0).unwrap();
        FileMetadata {
            identity: FileIdentity { device: 1, inode: 2 },
            kind,
            permissions: Permissions::from_bits(permissions),
            links: 1,
            user: 0,
            group: 0,
            special_device: 0,
            size,
            blocks_512: 0,
            accessed: timestamp,
            modified: timestamp,
            changed: timestamp,
        }
    }
}

impl ResolvedPathLease for Arc<Node> {
    fn metadata(&self) -> Result<FileMetadata, RuntimePathError> {
        Ok(self.metadata.clone())
    }

    fn read_link(&self) -> Result<Vec<u8>, RuntimePathError> {
        Err(RuntimePathError::Invalid)
    }

    fn access(&self, _: &hl_linux::AccessPlan) -> Result<(), RuntimePathError> {
        if self.executable {
            Ok(())
        } else {
            Err(RuntimePathError::Access)
        }
    }

    fn read_image(&self, maximum: usize) -> Result<Vec<u8>, RuntimePathError> {
        if self.metadata.size > maximum as u64 {
            return Err(RuntimePathError::TooLarge);
        }
        self.reads.fetch_add(1, Ordering::AcqRel);
        Ok(self.bytes.clone())
    }

    fn executable_access(&self, _: &ExecutablePath) -> Result<(), RuntimePathError> {
        if self.executable {
            Ok(())
        } else {
            Err(RuntimePathError::Access)
        }
    }
}

pub(super) struct Host {
    nodes: Mutex<BTreeMap<Vec<u8>, Arc<Node>>>,
    descriptor: Mutex<Option<Arc<Node>>>,
}

impl Host {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            nodes: Mutex::new(BTreeMap::new()),
            descriptor: Mutex::new(None),
        })
    }

    pub(super) fn add(&self, path: impl AsRef<[u8]>, node: Arc<Node>) {
        self.nodes.lock().unwrap().insert(path.as_ref().to_vec(), node);
    }

    fn remove(&self, path: impl AsRef<[u8]>) {
        self.nodes.lock().unwrap().remove(path.as_ref());
    }

    fn descriptor(&self, node: Arc<Node>) {
        *self.descriptor.lock().unwrap() = Some(node);
    }
}

impl RuntimePathHost for Host {
    fn root_base(&self) -> Result<DirectoryBaseLease, RuntimePathError> {
        Ok(DirectoryBaseLease::root(GuestPath::new("/").unwrap()))
    }

    fn descriptor_base(&self, lease: hl_descriptor::OperationLease) -> Result<DirectoryBaseLease, RuntimePathError> {
        Ok(DirectoryBaseLease::descriptor(lease, GuestPath::new("/").unwrap()))
    }

    fn descriptor_node(
        &self,
        _: hl_descriptor::OperationLease,
    ) -> Result<Box<dyn ResolvedPathLease>, RuntimePathError> {
        self.descriptor
            .lock()
            .unwrap()
            .clone()
            .map(|node| Box::new(node) as Box<dyn ResolvedPathLease>)
            .ok_or(RuntimePathError::Access)
    }

    fn resolve(
        &self,
        _: &DirectoryBaseLease,
        operand: &hl_linux::PathOperand,
    ) -> Result<Box<dyn ResolvedPathLease>, RuntimePathError> {
        self.nodes
            .lock()
            .unwrap()
            .get(operand.path.as_bytes())
            .cloned()
            .map(|node| Box::new(node) as Box<dyn ResolvedPathLease>)
            .ok_or(RuntimePathError::NotFound)
    }

    fn resolve_executable(
        &self,
        _: &DirectoryBaseLease,
        operand: &ExecutablePath,
    ) -> Result<Box<dyn ResolvedPathLease>, RuntimePathError> {
        self.nodes
            .lock()
            .unwrap()
            .get(operand.path.as_bytes())
            .cloned()
            .map(|node| Box::new(node) as Box<dyn ResolvedPathLease>)
            .ok_or(RuntimePathError::NotFound)
    }

    fn prepare_open(
        &self,
        _: &DirectoryBaseLease,
        _: &hl_linux::OpenAbiPlan,
        _: &hl_vfs::AccessIdentity,
    ) -> Result<Box<dyn PreparedPathOpen>, RuntimePathError> {
        Err(RuntimePathError::Unsupported)
    }
}

#[derive(Clone)]
pub(super) struct Memory {
    pub(super) bytes: Arc<Vec<u8>>,
}

impl GuestMemory for Memory {
    fn probe(&self, address: u64, length: usize, access: GuestAccess) -> Result<usize, GuestFault> {
        if (address as usize)
            .checked_add(length)
            .is_some_and(|end| end <= self.bytes.len())
        {
            Ok(length)
        } else {
            Err(GuestFault { address, access })
        }
    }

    fn read(&self, address: u64, output: &mut [u8]) -> Result<usize, GuestFault> {
        let start = address as usize;
        let Some(source) = self.bytes.get(start..start + output.len()) else {
            return Err(GuestFault {
                address,
                access: GuestAccess::Read,
            });
        };
        output.copy_from_slice(source);
        Ok(output.len())
    }

    fn write(&self, _: u64, _: &[u8]) -> Result<usize, GuestFault> {
        unreachable!()
    }
}

pub(super) struct Fixture {
    pub(super) host: Arc<Host>,
    descriptors: Arc<DescriptorImageSlot>,
    descriptor: i32,
    pub(super) process: hl_task::ProcessId,
}

impl Fixture {
    pub(super) fn new() -> Self {
        let table = DescriptorTable::new(32).unwrap();
        let descriptor = table
            .install(0, Arc::new(FileObject), DescriptorFlags::default())
            .unwrap();
        let descriptors = Arc::new(DescriptorImageSlot::new(table));
        let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
        let credentials = ProcessCredentials::new(0, 0, &[], 8).unwrap();
        let (process, _) = tasks.create_init(credentials, ProcessLimits::empty()).unwrap();
        Self {
            host: Host::new(),
            descriptors,
            descriptor,
            process,
        }
    }

    pub(super) fn factory(&self) -> VfsSourceFactory {
        VfsSourceFactory::new(self.host.clone(), self.descriptors.clone())
    }

    pub(super) fn plan(path: &[u8], directory: Option<i32>, flags: u32) -> ExecPlan {
        ExecPlan {
            directory,
            path: path.to_vec(),
            arguments: vec![b"program".to_vec()],
            environment: Vec::new(),
            flags,
        }
    }

    fn abi_plan(architecture: GuestArchitecture, directory: i32, path: &[u8], flags: u32) -> ExecPlan {
        let mut bytes = vec![0; 128];
        bytes[8..8 + path.len()].copy_from_slice(path);
        bytes[8 + path.len()] = 0;
        let memory = Memory { bytes: Arc::new(bytes) };
        ProcessAbi::new(&memory, architecture)
            .execveat(directory, 8, 0, 0, flags)
            .unwrap()
    }

    pub(super) fn execve_plan(architecture: GuestArchitecture, path: &[u8]) -> ExecPlan {
        let mut bytes = vec![0; 128];
        bytes[8..8 + path.len()].copy_from_slice(path);
        bytes[8 + path.len()] = 0;
        let memory = Memory { bytes: Arc::new(bytes) };
        ProcessAbi::new(&memory, architecture).execve(8, 0, 0).unwrap()
    }
}

#[test]
fn isa_path_precedence() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        fixture.host.add("/bin/app", Node::regular(b"elf", true));
        let execve = Fixture::execve_plan(architecture, b"/bin/app");
        assert!(fixture.factory().open(fixture.process, &execve).is_ok());
        let absolute = Fixture::abi_plan(architecture, 31, b"/bin/app", 0);
        assert!(fixture.factory().open(fixture.process, &absolute).is_ok());

        let empty = Fixture::abi_plan(architecture, fixture.descriptor, b"", 0x1000);
        fixture.host.descriptor(Node::regular(b"fd", true));
        assert!(fixture.factory().open(fixture.process, &empty).is_ok());
        let absent_flag = Fixture::abi_plan(architecture, fixture.descriptor, b"", 0);
        assert!(matches!(
            fixture.factory().open(fixture.process, &absent_flag),
            Err(RuntimeExecError::NotFound),
        ));
    }
}

#[test]
fn relative_errors_exact() {
    let fixture = Fixture::new();
    assert!(matches!(
        fixture
            .factory()
            .open(fixture.process, &Fixture::plan(b"app", Some(31), 0),),
        Err(RuntimeExecError::BadDescriptor),
    ));
    fixture.host.add("/link", Node::kind(FileKind::Symlink));
    assert!(matches!(
        fixture
            .factory()
            .open(fixture.process, &Fixture::plan(b"/link", Some(31), 0x100),),
        Err(RuntimeExecError::Loop),
    ));
    fixture.host.add("/plain", Node::regular(b"plain", false));
    assert!(matches!(
        fixture
            .factory()
            .open(fixture.process, &Fixture::plan(b"/plain", None, 0),),
        Err(RuntimeExecError::Access),
    ));
    fixture.host.add("/dir", Node::kind(FileKind::Directory));
    assert!(matches!(
        fixture
            .factory()
            .open(fixture.process, &Fixture::plan(b"/dir", None, 0),),
        Err(RuntimeExecError::Access),
    ));
}

#[test]
fn pinned_before_allocation() {
    let fixture = Fixture::new();
    let original = Node::regular(b"original", true);
    fixture.host.add("/bin/app", original.clone());
    let mut source = fixture
        .factory()
        .open(fixture.process, &Fixture::plan(b"/bin/app", None, 0))
        .unwrap();
    fixture.host.remove("/bin/app");
    fixture.host.add("/bin/app", Node::regular(b"replacement", true));
    assert_eq!(
        source.read_image(ImageRole::Main, b"/bin/app", 64),
        Ok(b"original".to_vec()),
    );
    assert_eq!(
        source.read_image(ImageRole::Main, b"/bin/app", 2),
        Err(ImageSourceError::TooLarge),
    );
    assert_eq!(original.reads.load(Ordering::Acquire), 1);
}

#[test]
fn source_published_exec() {
    let table = DescriptorTable::new(8).unwrap();
    let descriptor = table
        .install(
            0,
            Arc::new(FileObject),
            DescriptorFlags::from_bits(DescriptorFlags::CLOSE_ON_EXEC),
        )
        .unwrap();
    let slot = Arc::new(DescriptorImageSlot::new(table));
    let host = Host::new();
    host.descriptor(Node::regular(b"fd", true));
    let factory = VfsSourceFactory::new(host, slot.clone());
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let credentials = ProcessCredentials::new(0, 0, &[], 8).unwrap();
    let (process, _) = tasks.create_init(credentials, ProcessLimits::empty()).unwrap();
    let plan = Fixture::plan(b"", Some(descriptor), 0x1000);

    assert!(factory.open(process, &plan).is_ok());
    let (generation, old) = slot.current();
    let mut prepared = slot.prepare(generation);
    prepared.publish().unwrap();

    assert!(old.pin(descriptor).is_ok());
    assert!(matches!(
        factory.open(process, &plan),
        Err(RuntimeExecError::BadDescriptor),
    ));
    prepared.finish();
}
