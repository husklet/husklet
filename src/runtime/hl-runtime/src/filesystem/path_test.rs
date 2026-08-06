use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};

use hl_descriptor::OpenFileDescription;
use hl_linux::{GuestAccess, GuestFault, GuestMemory, LinuxResult};
use hl_vfs::{FileMetadata, GuestPath};

use super::*;
use crate::{
    DirectoryBaseLease, PreparedPathMutation, PreparedPathOpen, ResolvedPathLease, RuntimePathError, RuntimePathHost,
};

struct Memory(Mutex<Vec<u8>>);
struct FaultMemory(Mutex<Vec<u8>>);

impl GuestMemory for Memory {
    fn probe(&self, address: u64, length: usize, _access: GuestAccess) -> Result<usize, GuestFault> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .len()
            .saturating_sub(address as usize)
            .min(length))
    }
    fn read(&self, address: u64, output: &mut [u8]) -> Result<usize, GuestFault> {
        let bytes = self.0.lock().unwrap();
        let count = output.len().min(bytes.len().saturating_sub(address as usize));
        output[..count].copy_from_slice(&bytes[address as usize..address as usize + count]);
        Ok(count)
    }
    fn write(&self, address: u64, input: &[u8]) -> Result<usize, GuestFault> {
        let mut bytes = self.0.lock().unwrap();
        let count = input.len().min(bytes.len().saturating_sub(address as usize));
        bytes[address as usize..address as usize + count].copy_from_slice(&input[..count]);
        Ok(count)
    }
}
impl GuestMemory for FaultMemory {
    fn probe(&self, address: u64, length: usize, _access: GuestAccess) -> Result<usize, GuestFault> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .len()
            .saturating_sub(address as usize)
            .min(length))
    }
    fn read(&self, address: u64, output: &mut [u8]) -> Result<usize, GuestFault> {
        let bytes = self.0.lock().unwrap();
        let count = output.len().min(bytes.len().saturating_sub(address as usize));
        output[..count].copy_from_slice(&bytes[address as usize..address as usize + count]);
        Ok(count)
    }
    fn write(&self, address: u64, _input: &[u8]) -> Result<usize, GuestFault> {
        Err(GuestFault {
            address,
            access: GuestAccess::Write,
        })
    }
}

#[derive(Debug)]
struct Object;
impl OpenFileDescription for Object {}

#[derive(Debug)]
struct OpenTransaction {
    commits: Arc<AtomicUsize>,
    rollbacks: Arc<AtomicUsize>,
    fail: bool,
}
impl PreparedPathOpen for OpenTransaction {
    fn object(&self) -> Arc<dyn OpenFileDescription> {
        Arc::new(Object)
    }
    fn commit(&mut self) -> Result<(), RuntimePathError> {
        self.commits.fetch_add(1, Ordering::AcqRel);
        if self.fail { Err(RuntimePathError::Io) } else { Ok(()) }
    }
    fn rollback(self: Box<Self>) {
        self.rollbacks.fetch_add(1, Ordering::AcqRel);
    }
}
impl PreparedPathMutation for OpenTransaction {
    fn commit(&mut self) -> Result<(), RuntimePathError> {
        self.commits.fetch_add(1, Ordering::AcqRel);
        if self.fail { Err(RuntimePathError::Io) } else { Ok(()) }
    }
    fn rollback(self: Box<Self>) {
        self.rollbacks.fetch_add(1, Ordering::AcqRel);
    }
}

struct PathResolution;
impl std::fmt::Debug for PathResolution {
    fn fmt(&self, value: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        value.write_str("PathResolution")
    }
}
impl ResolvedPathLease for PathResolution {
    fn metadata(&self) -> Result<FileMetadata, RuntimePathError> {
        let time = hl_vfs::FileTimestamp {
            seconds: 4,
            nanoseconds: 5,
        };
        Ok(FileMetadata {
            identity: hl_vfs::FileIdentity { device: 11, inode: 22 },
            kind: hl_vfs::FileKind::Regular,
            permissions: hl_vfs::Permissions::from_bits(0o640),
            links: 2,
            user: 33,
            group: 44,
            special_device: 0,
            size: 55,
            blocks_512: 8,
            accessed: time,
            modified: time,
            changed: time,
        })
    }
    fn read_link(&self) -> Result<Vec<u8>, RuntimePathError> {
        Ok(vec![0xff, b'x'])
    }
    fn access(&self, _plan: &hl_linux::AccessPlan) -> Result<(), RuntimePathError> {
        Ok(())
    }
    fn read_image(&self, maximum: usize) -> Result<Vec<u8>, RuntimePathError> {
        if maximum < 55 {
            Err(RuntimePathError::TooLarge)
        } else {
            Ok(vec![0; 55])
        }
    }
}

struct Host {
    prepares: AtomicUsize,
    roots: AtomicUsize,
    descriptors: AtomicUsize,
    commits: Arc<AtomicUsize>,
    rollbacks: Arc<AtomicUsize>,
    fail: bool,
    entered: Option<Arc<Barrier>>,
    release: Option<Arc<Barrier>>,
    base_identity: AtomicU64,
    base_identities: Mutex<Vec<u64>>,
    mutation_bases: AtomicUsize,
    mutation_kinds: Mutex<Vec<&'static str>>,
}
impl RuntimePathHost for Host {
    fn root_base(&self) -> Result<DirectoryBaseLease, RuntimePathError> {
        self.roots.fetch_add(1, Ordering::AcqRel);
        Ok(DirectoryBaseLease::root(GuestPath::new("/").unwrap()))
    }
    fn descriptor_base(&self, lease: hl_descriptor::OperationLease) -> Result<DirectoryBaseLease, RuntimePathError> {
        self.descriptors.fetch_add(1, Ordering::AcqRel);
        Ok(DirectoryBaseLease::descriptor(lease, GuestPath::new("/base").unwrap()))
    }
    fn descriptor_node(
        &self,
        _lease: hl_descriptor::OperationLease,
    ) -> Result<Box<dyn ResolvedPathLease>, RuntimePathError> {
        self.descriptors.fetch_add(1, Ordering::AcqRel);
        Ok(Box::new(PathResolution))
    }
    fn resolve(
        &self,
        _base: &DirectoryBaseLease,
        _operand: &hl_linux::PathOperand,
    ) -> Result<Box<dyn ResolvedPathLease>, RuntimePathError> {
        Ok(Box::new(PathResolution))
    }
    fn prepare_open(
        &self,
        base: &DirectoryBaseLease,
        _plan: &hl_linux::OpenAbiPlan,
    ) -> Result<Box<dyn PreparedPathOpen>, RuntimePathError> {
        self.prepares.fetch_add(1, Ordering::AcqRel);
        if let Some(lease) = base.descriptor_lease() {
            self.base_identity
                .store(lease.description_identity().identity, Ordering::Release);
        }
        if let Some(barrier) = &self.entered {
            barrier.wait();
        }
        if let Some(barrier) = &self.release {
            barrier.wait();
        }
        Ok(Box::new(OpenTransaction {
            commits: self.commits.clone(),
            rollbacks: self.rollbacks.clone(),
            fail: self.fail,
        }))
    }
    fn prepare_mutation(
        &self,
        bases: &[DirectoryBaseLease],
        plan: &hl_linux::FsMutationPlan,
        _identity: &hl_vfs::AccessIdentity,
    ) -> Result<Box<dyn PreparedPathMutation>, RuntimePathError> {
        self.mutation_bases.store(bases.len(), Ordering::Release);
        *self.base_identities.lock().unwrap() = bases
            .iter()
            .filter_map(|base| {
                base.descriptor_lease()
                    .map(|lease| lease.description_identity().identity)
            })
            .collect();
        if let Some(barrier) = &self.entered {
            barrier.wait();
        }
        if let Some(barrier) = &self.release {
            barrier.wait();
        }
        let kind = match plan {
            hl_linux::FsMutationPlan::CreateDirectory { .. } => "mkdir",
            hl_linux::FsMutationPlan::CreateNode { .. } => "mknod",
            hl_linux::FsMutationPlan::Unlink { directory: false, .. } => "unlink",
            hl_linux::FsMutationPlan::Unlink { directory: true, .. } => "rmdir",
            hl_linux::FsMutationPlan::Rename { .. } => "rename",
            hl_linux::FsMutationPlan::Link { .. } => "link",
            hl_linux::FsMutationPlan::Symlink { .. } => "symlink",
            hl_linux::FsMutationPlan::Chmod { .. } => "chmod",
            hl_linux::FsMutationPlan::Chown { .. } => "chown",
            hl_linux::FsMutationPlan::SetTimes { .. } => "utimens",
        };
        self.mutation_kinds.lock().unwrap().push(kind);
        Ok(Box::new(OpenTransaction {
            commits: self.commits.clone(),
            rollbacks: self.rollbacks.clone(),
            fail: self.fail,
        }))
    }
    fn prepare_inode_link(
        &self,
        _source: hl_descriptor::OperationLease,
        _target_base: &DirectoryBaseLease,
        _target: &hl_linux::PathOperand,
        _identity: &hl_vfs::AccessIdentity,
    ) -> Result<Box<dyn PreparedPathMutation>, RuntimePathError> {
        self.mutation_kinds.lock().unwrap().push("inode-link");
        Ok(Box::new(OpenTransaction {
            commits: self.commits.clone(),
            rollbacks: self.rollbacks.clone(),
            fail: self.fail,
        }))
    }
    fn access_identity(&self) -> Result<hl_vfs::AccessIdentity, RuntimePathError> {
        Ok(hl_vfs::AccessIdentity {
            user: 0,
            group: 0,
            supplementary_groups: Vec::new(),
            capabilities: hl_vfs::Capabilities {
                dac_read_search: true,
                ..hl_vfs::Capabilities::default()
            },
        })
    }
}

impl Host {
    fn fixture(architecture: GuestArchitecture, fail: bool) -> (RuntimeFilesystemSyscalls<Memory>, Arc<Host>) {
        Self::fixture_limit(architecture, fail, 16)
    }

    fn fixture_limit(
        architecture: GuestArchitecture,
        fail: bool,
        limit: i32,
    ) -> (RuntimeFilesystemSyscalls<Memory>, Arc<Host>) {
        let mut bytes = vec![0; 256];
        bytes[32..37].copy_from_slice(b"/tmp\0");
        let host = Arc::new(Host {
            prepares: AtomicUsize::new(0),
            roots: AtomicUsize::new(0),
            descriptors: AtomicUsize::new(0),
            commits: Arc::new(AtomicUsize::new(0)),
            rollbacks: Arc::new(AtomicUsize::new(0)),
            fail,
            entered: None,
            release: None,
            base_identity: AtomicU64::new(0),
            base_identities: Mutex::new(Vec::new()),
            mutation_bases: AtomicUsize::new(0),
            mutation_kinds: Mutex::new(Vec::new()),
        });
        let adapter = RuntimeFilesystemSyscalls::new(
            Arc::new(DescriptorTable::new(limit).unwrap()),
            Memory(Mutex::new(bytes)),
            architecture,
        )
        .with_path_host(host.clone());
        (adapter, host)
    }
}

#[test]
fn descriptor_limit_order() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        for (flags, mode) in [(0x40_u64 | 2, 0o600), (0x200_u64 | 2, 0)] {
            let (adapter, host) = Host::fixture_limit(architecture, false, 0);
            assert_eq!(
                adapter.openat([u64::MAX, 32, flags, mode, 0, 0], false),
                LinuxResult::Error(Errno::EMFILE),
            );
            assert_eq!(host.prepares.load(Ordering::Acquire), 0);
            assert_eq!(host.commits.load(Ordering::Acquire), 0);
            assert_eq!(host.rollbacks.load(Ordering::Acquire), 0);
        }
    }
}

#[test]
fn absolute_flags_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let (adapter, host) = Host::fixture(architecture, false);
        let result = adapter.openat([u64::MAX, 32, 2 | 0x400 | 0x800 | 0x80000, 0, 0, 0], false);
        let LinuxResult::Value(fd) = result else {
            panic!("{result:?}")
        };
        assert_eq!(host.roots.load(Ordering::Acquire), 1);
        assert_eq!(host.descriptors.load(Ordering::Acquire), 0);
        assert_eq!(host.commits.load(Ordering::Acquire), 1);
        assert!(adapter.descriptors.flags(fd as i32).unwrap().closes_on_exec());
        let status = adapter.descriptors.pin(fd as i32).unwrap().status().bits();
        assert_eq!(status & 3, 2);
        assert_ne!(status & hl_descriptor::StatusFlags::APPEND, 0);
        assert_ne!(status & hl_descriptor::StatusFlags::NONBLOCKING, 0);
    }
}

#[test]
fn openat2_beneath_rejects_absolute_path_before_host() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let (adapter, host) = Host::fixture(architecture, false);
        {
            let mut bytes = adapter.memory.0.lock().unwrap();
            bytes[64..72].copy_from_slice(&0_u64.to_le_bytes());
            bytes[72..80].copy_from_slice(&0_u64.to_le_bytes());
            bytes[80..88].copy_from_slice(&0x8_u64.to_le_bytes());
        }
        assert_eq!(
            adapter.openat([u64::MAX, 32, 64, 24, 0, 0], true),
            LinuxResult::Error(hl_linux::Errno::EXDEV)
        );
        assert_eq!(host.roots.load(Ordering::Acquire), 0);
        assert_eq!(host.descriptors.load(Ordering::Acquire), 0);
        assert_eq!(host.prepares.load(Ordering::Acquire), 0);
    }
}

#[test]
fn host_publishes_descriptor() {
    let (adapter, host) = Host::fixture(GuestArchitecture::Aarch64, true);
    assert_eq!(
        adapter.openat([u64::MAX, 32, 0, 0, 0, 0], false),
        LinuxResult::Error(Errno::EIO)
    );
    assert_eq!(host.commits.load(Ordering::Acquire), 1);
    assert_eq!(host.rollbacks.load(Ordering::Acquire), 1);
    assert_eq!(adapter.descriptors.pin(0).unwrap_err(), DescriptorError::BadDescriptor);
}

#[test]
fn openat2_how_validation_precedes_containment() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let (adapter, host) = Host::fixture(architecture, false);
        {
            let mut bytes = adapter.memory.0.lock().unwrap();
            bytes[64..72].copy_from_slice(&(0x80000_u64 | 0x40 | 0x80 | 0x200).to_le_bytes());
            bytes[72..80].copy_from_slice(&0o640_u64.to_le_bytes());
            bytes[80..88].copy_from_slice(&8_u64.to_le_bytes());
        }
        let result = adapter.openat([u64::MAX, 32, 64, 24, 0, 0], true);
        assert_eq!(result, LinuxResult::Error(Errno::EXDEV));
        assert_eq!(host.commits.load(Ordering::Acquire), 0);
        let invalid = adapter.openat([u64::MAX, 32, u64::MAX, 8, 0, 0], true);
        assert_eq!(invalid, LinuxResult::Error(Errno::EINVAL));
    }
}

#[test]
fn invalid_prepare_publish() {
    let (adapter, host) = Host::fixture(GuestArchitecture::Aarch64, false);
    let result = adapter.openat([u64::MAX, 32, u64::MAX, 0, 0, 0], false);
    assert_eq!(result, LinuxResult::Error(Errno::EINVAL));
    assert_eq!(host.prepares.load(Ordering::Acquire), 0);
    assert_eq!(host.commits.load(Ordering::Acquire), 0);
    assert_eq!(adapter.descriptors.pin(0).unwrap_err(), DescriptorError::BadDescriptor);
}

#[test]
fn relative_close_reuse() {
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let host = Arc::new(Host {
        prepares: AtomicUsize::new(0),
        roots: AtomicUsize::new(0),
        descriptors: AtomicUsize::new(0),
        commits: Arc::new(AtomicUsize::new(0)),
        rollbacks: Arc::new(AtomicUsize::new(0)),
        fail: false,
        entered: Some(entered.clone()),
        release: Some(release.clone()),
        base_identity: AtomicU64::new(0),
        base_identities: Mutex::new(Vec::new()),
        mutation_bases: AtomicUsize::new(0),
        mutation_kinds: Mutex::new(Vec::new()),
    });
    let table = Arc::new(DescriptorTable::new(16).unwrap());
    let dirfd = table.install(0, Arc::new(Object), DescriptorFlags::default()).unwrap();
    let original = table.pin(dirfd).unwrap().description_identity().identity;
    let mut bytes = vec![0; 256];
    bytes[32..36].copy_from_slice(b"tmp\0");
    let adapter = Arc::new(
        RuntimeFilesystemSyscalls::new(table.clone(), Memory(Mutex::new(bytes)), GuestArchitecture::Aarch64)
            .with_path_host(host.clone()),
    );
    std::thread::scope(|scope| {
        let runtime = adapter.clone();
        let worker = scope.spawn(move || runtime.openat([dirfd as u64, 32, 0, 0, 0, 0], false));
        entered.wait();
        table.close(dirfd).unwrap();
        assert_eq!(
            table.install(0, Arc::new(Object), DescriptorFlags::default()).unwrap(),
            dirfd
        );
        release.wait();
        assert!(matches!(worker.join().unwrap(), LinuxResult::Value(_)));
    });
    assert_eq!(host.base_identity.load(Ordering::Acquire), original);
}

#[test]
fn checkpoint_host_commit() {
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let host = Arc::new(Host {
        prepares: AtomicUsize::new(0),
        roots: AtomicUsize::new(0),
        descriptors: AtomicUsize::new(0),
        commits: Arc::new(AtomicUsize::new(0)),
        rollbacks: Arc::new(AtomicUsize::new(0)),
        fail: false,
        entered: Some(entered.clone()),
        release: Some(release.clone()),
        base_identity: AtomicU64::new(0),
        base_identities: Mutex::new(Vec::new()),
        mutation_bases: AtomicUsize::new(0),
        mutation_kinds: Mutex::new(Vec::new()),
    });
    let table = Arc::new(DescriptorTable::new(8).unwrap());
    let mut bytes = vec![0; 256];
    bytes[32..37].copy_from_slice(b"/tmp\0");
    let adapter = Arc::new(
        RuntimeFilesystemSyscalls::new(table.clone(), Memory(Mutex::new(bytes)), GuestArchitecture::Aarch64)
            .with_path_host(host.clone()),
    );
    std::thread::scope(|scope| {
        let runtime = adapter.clone();
        let worker = scope.spawn(move || runtime.openat([u64::MAX, 32, 0, 0, 0, 0], false));
        entered.wait();
        let frozen = Arc::new(AtomicUsize::new(0));
        let marker = Arc::clone(&frozen);
        let table_ref = &table;
        let freezer = scope.spawn(move || {
            table_ref.freeze_checkpoint();
            marker.store(1, Ordering::Release);
        });
        assert_eq!(frozen.load(Ordering::Acquire), 0);
        release.wait();
        assert!(matches!(worker.join().unwrap(), LinuxResult::Value(0)));
        freezer.join().unwrap();
        assert_eq!(frozen.load(Ordering::Acquire), 1);
        table.thaw_checkpoint();
    });
    assert_eq!(host.commits.load(Ordering::Acquire), 1);
    assert_eq!(host.rollbacks.load(Ordering::Acquire), 0);
    assert_eq!(table.pin(0).unwrap().descriptor_number(), 0);
}

#[test]
fn mutation_transaction_outcomes() {
    let (adapter, host) = Host::fixture(GuestArchitecture::Aarch64, false);
    assert_eq!(
        adapter.path_mutation("mkdirat", [(-100_i64) as u64, 32, 0o755, 0, 0, 0]),
        LinuxResult::Value(0),
    );
    assert_eq!(host.mutation_bases.load(Ordering::Acquire), 1);
    assert_eq!(host.commits.load(Ordering::Acquire), 1);

    let (failed, failed_host) = Host::fixture(GuestArchitecture::Aarch64, true);
    {
        let mut bytes = failed.memory.0.lock().unwrap();
        bytes[40..45].copy_from_slice(b"/new\0");
    }
    assert_eq!(
        failed.path_mutation("renameat2", [(-100_i64) as u64, 32, (-100_i64) as u64, 40, 1, 0],),
        LinuxResult::Error(Errno::EIO),
    );
    assert_eq!(failed_host.mutation_bases.load(Ordering::Acquire), 2);
    assert_eq!(failed_host.commits.load(Ordering::Acquire), 1);
    assert_eq!(failed_host.rollbacks.load(Ordering::Acquire), 1);
}

#[test]
fn mutation_prepared_transaction() {
    let (adapter, host) = Host::fixture(GuestArchitecture::Aarch64, false);
    adapter.memory.0.lock().unwrap()[40..45].copy_from_slice(b"/new\0");
    let root = (-100_i64) as u64;
    let cases = [
        ("mkdirat", [root, 32, 0o755, 0, 0, 0]),
        ("mknodat", [root, 32, 0o100600, 0, 0, 0]),
        ("unlinkat", [root, 32, 0, 0, 0, 0]),
        ("unlinkat", [root, 32, 0x200, 0, 0, 0]),
        ("symlinkat", [32, root, 40, 0, 0, 0]),
        ("linkat", [root, 32, root, 40, 0, 0]),
        ("renameat2", [root, 32, root, 40, 1, 0]),
        ("fchmodat", [root, 32, 0o640, 0, 0, 0]),
        ("fchownat", [root, 32, 7, 8, 0, 0]),
        ("utimensat", [root, 32, 0, 0, 0, 0]),
    ];
    for (name, arguments) in cases {
        assert_eq!(adapter.path_mutation(name, arguments), LinuxResult::Value(0));
    }
    assert_eq!(
        *host.mutation_kinds.lock().unwrap(),
        [
            "mkdir", "mknod", "unlink", "rmdir", "symlink", "link", "rename", "chmod", "chown", "utimens"
        ],
    );
}

#[test]
fn inode_link_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let (adapter, host) = Host::fixture(architecture, false);
        adapter.memory.0.lock().unwrap()[40..45].copy_from_slice(b"/new\0");
        let descriptor = adapter
            .descriptors
            .install(0, Arc::new(Object), DescriptorFlags::default())
            .unwrap();
        assert_eq!(
            adapter.path_mutation("linkat", [descriptor as u64, 48, (-100_i64) as u64, 40, 0x1000, 0],),
            LinuxResult::Value(0),
        );
        assert_eq!(*host.mutation_kinds.lock().unwrap(), ["inode-link"]);
        assert_eq!(host.commits.load(Ordering::Acquire), 1);
    }
}

#[test]
fn proc_descriptor_link_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let (adapter, host) = Host::fixture(architecture, false);
        let mut bytes = adapter.memory.0.lock().unwrap();
        bytes[40..45].copy_from_slice(b"/new\0");
        bytes[48..64].copy_from_slice(b"/proc/self/fd/0\0");
        drop(bytes);
        adapter
            .descriptors
            .install(0, Arc::new(Object), DescriptorFlags::default())
            .unwrap();
        assert_eq!(
            adapter.path_mutation("linkat", [(-100_i64) as u64, 48, (-100_i64) as u64, 40, 0x400, 0],),
            LinuxResult::Value(0),
        );
        assert_eq!(*host.mutation_kinds.lock().unwrap(), ["inode-link"]);
    }
}

#[test]
fn read_bytes_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let (stat, _) = Host::fixture(architecture, false);
        assert_eq!(stat.path_stat([u64::MAX, 32, 0, 0, 0, 0], false), LinuxResult::Value(0));
        let bytes = stat.memory.0.lock().unwrap();
        assert_eq!(u64::from_le_bytes(bytes[0..8].try_into().unwrap()), 11);
        assert_eq!(u64::from_le_bytes(bytes[8..16].try_into().unwrap()), 22);
        let mode_offset = if architecture == GuestArchitecture::Aarch64 {
            16
        } else {
            24
        };
        assert_eq!(
            u32::from_le_bytes(bytes[mode_offset..mode_offset + 4].try_into().unwrap()),
            0o100640
        );
        drop(bytes);

        let (statx, _) = Host::fixture(architecture, false);
        assert_eq!(
            statx.path_stat([u64::MAX, 32, 0, 0x7ff, 0, 0], true),
            LinuxResult::Value(0),
        );
        let bytes = statx.memory.0.lock().unwrap();
        assert_eq!(u64::from_le_bytes(bytes[32..40].try_into().unwrap()), 22);
        assert_eq!(u16::from_le_bytes(bytes[28..30].try_into().unwrap()), 0o100640);
        drop(bytes);

        let (readlink, _) = Host::fixture(architecture, false);
        assert_eq!(
            readlink.path_readlink([u64::MAX, 32, 0, 8, 0, 0]),
            LinuxResult::Value(2),
        );
        assert_eq!(&readlink.memory.0.lock().unwrap()[0..2], &[0xff, b'x']);
        let (access, _) = Host::fixture(architecture, false);
        assert_eq!(
            access.path_access([u64::MAX, 32, 4, 0, 0, 0], false),
            LinuxResult::Value(0)
        );
    }
}

#[test]
fn followed_procfd_requires_a_live_descriptor() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let (adapter, _) = Host::fixture(architecture, false);
        let path = b"/proc/self/fd/3\0";
        adapter.memory.0.lock().unwrap()[32..32 + path.len()].copy_from_slice(path);
        assert_eq!(
            adapter.path_access([u64::MAX, 32, 0, 0, 0, 0], false),
            LinuxResult::Error(Errno::ENOENT),
        );
        adapter
            .descriptors
            .install(3, Arc::new(Object), hl_descriptor::DescriptorFlags::default())
            .unwrap();
        assert_eq!(
            adapter.path_access([u64::MAX, 32, 0, 0, 0, 0], false),
            LinuxResult::Value(0),
        );
    }
}

#[test]
fn mutation_linux_meaning() {
    assert_eq!(RuntimePathError::CrossDevice.errno(), Errno::EXDEV);
    assert_eq!(RuntimePathError::Access.errno(), Errno::EACCES);
    assert_eq!(RuntimePathError::ReadOnly.errno(), Errno::EROFS);
    assert_eq!(RuntimePathError::Exists.errno(), Errno::EEXIST);
    assert_eq!(RuntimePathError::NotDirectory.errno(), Errno::ENOTDIR);
}

#[test]
fn descriptor_readlink_is_empty_path_copyout() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let (adapter, _) = Host::fixture(architecture, false);
        adapter.memory.0.lock().unwrap()[48] = 0;
        let descriptor = adapter
            .descriptors
            .install(0, Arc::new(Object), hl_descriptor::DescriptorFlags::default())
            .unwrap();
        assert_eq!(
            adapter.path_readlink([descriptor as u64, 48, 0, 1, 0, 0]),
            LinuxResult::Value(1),
        );
        assert_eq!(adapter.memory.0.lock().unwrap()[0], 0xff);
        assert_eq!(
            adapter.path_readlink([99, 48, 0, 1, 0, 0]),
            LinuxResult::Error(Errno::EBADF),
        );
        assert_eq!(
            adapter.path_readlink([99, 48, 256, 1, 0, 0]),
            LinuxResult::Error(Errno::EFAULT),
        );
        assert_eq!(
            adapter.path_readlink([descriptor as u64, 48, 0, 0, 0, 0]),
            LinuxResult::Error(Errno::EINVAL),
        );
    }
}

#[test]
fn staged_host_state() {
    let mut bytes = vec![0; 256];
    bytes[32..37].copy_from_slice(b"/tmp\0");
    let host = Arc::new(Host {
        prepares: AtomicUsize::new(0),
        roots: AtomicUsize::new(0),
        descriptors: AtomicUsize::new(0),
        commits: Arc::new(AtomicUsize::new(0)),
        rollbacks: Arc::new(AtomicUsize::new(0)),
        fail: false,
        entered: None,
        release: None,
        base_identity: AtomicU64::new(0),
        base_identities: Mutex::new(Vec::new()),
        mutation_bases: AtomicUsize::new(0),
        mutation_kinds: Mutex::new(Vec::new()),
    });
    let adapter = RuntimeFilesystemSyscalls::new(
        Arc::new(DescriptorTable::new(8).unwrap()),
        FaultMemory(Mutex::new(bytes)),
        GuestArchitecture::Aarch64,
    )
    .with_path_host(host.clone());
    assert_eq!(
        adapter.path_stat([u64::MAX, 32, 0, 0, 0, 0], false),
        LinuxResult::Error(Errno::EFAULT)
    );
    assert_eq!(
        adapter.path_readlink([u64::MAX, 32, 0, 8, 0, 0]),
        LinuxResult::Error(Errno::EFAULT)
    );
    assert_eq!(host.commits.load(Ordering::Acquire), 0);
    assert_eq!(host.rollbacks.load(Ordering::Acquire), 0);
}

#[test]
fn two_across_reuse() {
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let host = Arc::new(Host {
        prepares: AtomicUsize::new(0),
        roots: AtomicUsize::new(0),
        descriptors: AtomicUsize::new(0),
        commits: Arc::new(AtomicUsize::new(0)),
        rollbacks: Arc::new(AtomicUsize::new(0)),
        fail: false,
        entered: Some(entered.clone()),
        release: Some(release.clone()),
        base_identity: AtomicU64::new(0),
        base_identities: Mutex::new(Vec::new()),
        mutation_bases: AtomicUsize::new(0),
        mutation_kinds: Mutex::new(Vec::new()),
    });
    let table = Arc::new(DescriptorTable::new(8).unwrap());
    let first = table.install(0, Arc::new(Object), DescriptorFlags::default()).unwrap();
    let second = table.install(0, Arc::new(Object), DescriptorFlags::default()).unwrap();
    let expected = vec![
        table.pin(first).unwrap().description_identity().identity,
        table.pin(second).unwrap().description_identity().identity,
    ];
    let mut bytes = vec![0; 256];
    bytes[32..36].copy_from_slice(b"old\0");
    bytes[40..44].copy_from_slice(b"new\0");
    let adapter = Arc::new(
        RuntimeFilesystemSyscalls::new(table.clone(), Memory(Mutex::new(bytes)), GuestArchitecture::Aarch64)
            .with_path_host(host.clone()),
    );
    std::thread::scope(|scope| {
        let runtime = adapter.clone();
        let worker =
            scope.spawn(move || runtime.path_mutation("renameat2", [first as u64, 32, second as u64, 40, 0, 0]));
        entered.wait();
        table.close(first).unwrap();
        table.close(second).unwrap();
        table.install(0, Arc::new(Object), DescriptorFlags::default()).unwrap();
        table.install(0, Arc::new(Object), DescriptorFlags::default()).unwrap();
        release.wait();
        assert_eq!(worker.join().unwrap(), LinuxResult::Value(0));
    });
    assert_eq!(*host.base_identities.lock().unwrap(), expected);
}
