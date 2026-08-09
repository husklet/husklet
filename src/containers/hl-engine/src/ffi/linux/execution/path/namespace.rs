use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use hl_descriptor::{DescriptionIdentity, OpenFileDescription};
use hl_linux::{OpenAbiPlan, PathOperand};
use hl_runtime::{
    GuestPathBytes, NamespaceHandle, NamespaceHandleRegistry, OpenIntent, PreparedPathOpen, RuntimePathError,
};
use hl_task::NamespaceId;

use super::{NativePath, lease, projected};

pub(super) struct ProcfsTargets {
    paths: Arc<Mutex<BTreeMap<(u64, u64), lease::LeaseEntry>>>,
    projected: projected::Registry,
}

impl ProcfsTargets {
    pub(super) fn new(
        paths: Arc<Mutex<BTreeMap<(u64, u64), lease::LeaseEntry>>>,
        projected: projected::Registry,
    ) -> Arc<Self> {
        Arc::new(Self { paths, projected })
    }
}

impl hl_runtime::ProcfsDescriptorTarget for ProcfsTargets {
    fn path(&self, metadata: &hl_descriptor::OfdMetadata) -> Result<Vec<u8>, hl_runtime::ProcfsError> {
        if metadata.kind == 2
            && let Some(device) = hl_runtime::BUILTIN_DEVICES
                .iter()
                .find(|device| device.device.linux_encoded() == metadata.special_device)
        {
            return Ok(device.path.as_bytes().to_vec());
        }
        if metadata.kind == 2 {
            let device = hl_runtime::DeviceId::from_linux_encoded(metadata.special_device);
            if device.major == 136 {
                return Ok(format!("/dev/pts/{}", device.minor).into_bytes());
            }
        }
        let identity = (metadata.device, metadata.inode);
        if let Some(file) = self.projected.get(&identity) {
            return file
                .guest()
                .map(|path| path.as_str().as_bytes().to_vec())
                .map_err(|_| hl_runtime::ProcfsError::Invalid);
        }
        self.paths
            .lock()
            .map_err(|_| hl_runtime::ProcfsError::Invalid)?
            .get(&identity)
            .map(|entry| entry.guest.as_str().as_bytes().to_vec())
            .ok_or(hl_runtime::ProcfsError::NotFound)
    }
}

pub(super) struct ProcfsLink {
    target: Option<Vec<u8>>,
    raw: hl_descriptor::OfdMetadata,
    metadata: hl_runtime::FileMetadata,
}

impl ProcfsLink {
    pub(super) fn target(&self) -> Option<&[u8]> {
        self.target.as_deref()
    }

    pub(super) fn follow_namespace(mut self) -> Self {
        self.target = None;
        self.raw.kind = 8;
        self.raw.permissions = 0o444;
        self.metadata.kind = hl_runtime::FileKind::Regular;
        self.metadata.permissions = hl_runtime::Permissions::from_bits(0o444);
        self
    }
}

impl std::fmt::Debug for ProcfsLink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ProcfsDescriptorLink")
    }
}

impl hl_runtime::ResolvedPathLease for ProcfsLink {
    fn metadata(&self) -> Result<hl_runtime::FileMetadata, RuntimePathError> {
        Ok(self.metadata.clone())
    }

    fn read_link(&self) -> Result<Vec<u8>, RuntimePathError> {
        self.target.clone().ok_or(RuntimePathError::Invalid)
    }

    fn access(&self, _plan: &hl_linux::AccessPlan) -> Result<(), RuntimePathError> {
        Ok(())
    }
}

impl OpenFileDescription for ProcfsLink {
    fn metadata(&self) -> Result<hl_descriptor::OfdMetadata, hl_descriptor::ObjectError> {
        Ok(self.raw.clone())
    }
}

impl NativePath {
    pub(super) fn procfs_node(&self, path: &[u8]) -> Result<Option<ProcfsLink>, RuntimePathError> {
        let (Some(procfs), Some(process)) = (&self.procfs, self.process) else {
            return Ok(None);
        };
        let thread = self.thread.map_or(process.number(), hl_task::ThreadId::number);
        let Some(metadata) = procfs
            .metadata_for(path, process.number(), thread)
            .map_err(Self::procfs_error)?
        else {
            return Ok(None);
        };
        let target = procfs
            .read_link_for(
                path,
                process.number(),
                self.thread.map_or(process.number(), hl_task::ThreadId::number),
            )
            .map_err(Self::procfs_error)?;
        let kind = match metadata.kind {
            4 => hl_runtime::FileKind::Directory,
            8 => hl_runtime::FileKind::Regular,
            10 => hl_runtime::FileKind::Symlink,
            _ => return Err(RuntimePathError::Invalid),
        };
        Ok(Some(ProcfsLink {
            target,
            raw: metadata.clone(),
            metadata: hl_runtime::FileMetadata {
                identity: hl_runtime::FileIdentity {
                    device: metadata.device,
                    inode: metadata.inode,
                },
                kind,
                permissions: hl_runtime::Permissions::from_bits(metadata.permissions),
                links: metadata.links,
                user: metadata.user,
                group: metadata.group,
                special_device: metadata.special_device,
                size: metadata.size,
                blocks_512: metadata.blocks_512,
                block_size: metadata.block_size,
                accessed: hl_runtime::FileTimestamp {
                    seconds: metadata.accessed.seconds,
                    nanoseconds: metadata.accessed.nanoseconds,
                },
                modified: hl_runtime::FileTimestamp {
                    seconds: metadata.modified.seconds,
                    nanoseconds: metadata.modified.nanoseconds,
                },
                changed: hl_runtime::FileTimestamp {
                    seconds: metadata.changed.seconds,
                    nanoseconds: metadata.changed.nanoseconds,
                },
            },
        }))
    }

    /// Anchors a procfs magic-link target that names its sibling relatively, as
    /// `/proc/self` names a PID and `/proc/mounts` names `self/mounts`.
    pub(super) fn procfs_target(path: &[u8], target: Vec<u8>) -> Vec<u8> {
        const RELATIVE: &[&[u8]] = &[b"/proc/self", b"/proc/thread-self", b"/proc/mounts"];
        if !RELATIVE.contains(&path) {
            return target;
        }
        let slash = path.iter().rposition(|byte| *byte == b'/').unwrap_or(0);
        let mut absolute = path[..slash].to_vec();
        absolute.push(b'/');
        absolute.extend_from_slice(&target);
        absolute
    }

    pub(super) fn procfs_link(&self, path: &[u8]) -> Result<Option<Vec<u8>>, RuntimePathError> {
        let (Some(procfs), Some(process)) = (&self.procfs, self.process) else {
            return Ok(None);
        };
        procfs
            .read_link_for(
                path,
                process.number(),
                self.thread.map_or(process.number(), hl_task::ThreadId::number),
            )
            .map_err(Self::procfs_error)
    }

    pub(super) fn procfs_namespace(&self, path: &[u8]) -> Result<bool, RuntimePathError> {
        let (Some(procfs), Some(process)) = (&self.procfs, self.process) else {
            return Ok(false);
        };
        procfs
            .namespace_inode(path, process.number())
            .map(|inode| inode.is_some())
            .map_err(Self::procfs_error)
    }

    pub(super) fn procfs_plan(&self, plan: &OpenAbiPlan) -> Result<Option<OpenAbiPlan>, RuntimePathError> {
        // A namespace magic link never resolves through its "pid:[...]" target;
        // an open of it binds the namespace object itself.
        if let (Some(procfs), Some(process)) = (&self.procfs, self.process)
            && procfs
                .namespace_inode(plan.operand.path.as_bytes(), process.number())
                .map_err(Self::procfs_error)?
                .is_some()
        {
            return Ok(None);
        }
        let Some(target) = self.procfs_link(plan.operand.path.as_bytes())? else {
            return Ok(None);
        };
        let target = Self::procfs_target(plan.operand.path.as_bytes(), target);
        if plan.operand.nofollow && plan.intent.bits() & OpenIntent::PATH_ONLY == 0 {
            return Err(RuntimePathError::Loop);
        }
        if plan.operand.nofollow {
            return Ok(None);
        }
        if plan.resolve.no_magic_links {
            return Err(RuntimePathError::Loop);
        }
        Ok(Some(OpenAbiPlan {
            operand: PathOperand {
                directory: plan.operand.directory,
                path: GuestPathBytes::new(&target).map_err(|_| RuntimePathError::Invalid)?,
                allow_empty: false,
                nofollow: plan.operand.nofollow,
            },
            intent: plan.intent,
            mode: plan.mode,
            close_on_exec: plan.close_on_exec,
            nonblocking: plan.nonblocking,
            no_controlling_terminal: plan.no_controlling_terminal,
            resolve: plan.resolve,
        }))
    }

    fn procfs_error(error: hl_runtime::ProcfsError) -> RuntimePathError {
        match error {
            hl_runtime::ProcfsError::NotFound => RuntimePathError::NotFound,
            hl_runtime::ProcfsError::Access => RuntimePathError::Access,
            hl_runtime::ProcfsError::ReadOnly => RuntimePathError::ReadOnly,
            hl_runtime::ProcfsError::Invalid => RuntimePathError::Invalid,
            hl_runtime::ProcfsError::ResourceLimit => RuntimePathError::TooLarge,
        }
    }

    pub(super) fn synthetic_open(
        &self,
        plan: &OpenAbiPlan,
    ) -> Result<Option<Box<dyn PreparedPathOpen>>, RuntimePathError> {
        if plan.operand.nofollow
            && plan.intent.bits() & OpenIntent::PATH_ONLY != 0
            && let Some(node) = self.procfs_node(plan.operand.path.as_bytes())?
            && node.target().is_some()
        {
            if plan.intent.bits() & OpenIntent::DIRECTORY != 0 {
                return Err(RuntimePathError::NotDirectory);
            }
            return Ok(Some(Box::new(ProcfsOpen(Arc::new(node)))));
        }
        if let (Some(procfs), Some(process)) = (&self.procfs, self.process) {
            match procfs.open_for(
                plan.operand.path.as_bytes(),
                process.number(),
                self.thread.map_or(process.number(), hl_task::ThreadId::number),
                plan.intent,
            ) {
                Ok(Some(object)) => return Ok(Some(Box::new(ProcfsOpen(object)))),
                Ok(None) => {}
                Err(hl_runtime::ProcfsError::NotFound) => return Err(RuntimePathError::NotFound),
                Err(hl_runtime::ProcfsError::Access) => return Err(RuntimePathError::Access),
                Err(hl_runtime::ProcfsError::ReadOnly) => return Err(RuntimePathError::ReadOnly),
                Err(hl_runtime::ProcfsError::Invalid) => return Err(RuntimePathError::Invalid),
                Err(hl_runtime::ProcfsError::ResourceLimit) => return Err(RuntimePathError::TooLarge),
            }
            let namespace = procfs
                .uts_namespace(plan.operand.path.as_bytes(), process.number())
                .map_err(|error| match error {
                    hl_runtime::ProcfsError::NotFound => RuntimePathError::NotFound,
                    hl_runtime::ProcfsError::Access => RuntimePathError::Access,
                    hl_runtime::ProcfsError::ReadOnly => RuntimePathError::ReadOnly,
                    hl_runtime::ProcfsError::Invalid => RuntimePathError::Invalid,
                    hl_runtime::ProcfsError::ResourceLimit => RuntimePathError::TooLarge,
                })?;
            if let Some(serial) = namespace {
                let handles = self.namespace_handles.as_ref().ok_or(RuntimePathError::NotFound)?;
                return NamespaceOpen::prepare(
                    handles,
                    NamespaceId {
                        kind: hl_task::NamespaceKind::Uts,
                        serial,
                    },
                    plan.intent,
                )
                .map(Some);
            }
        }
        if let (Some(procfs), Some(process)) = (&self.procfs, self.process)
            && procfs
                .auxv(
                    plan.operand.path.as_bytes(),
                    process.number(),
                    self.thread.map_or(process.number(), hl_task::ThreadId::number),
                )
                .map_err(Self::procfs_error)?
        {
            let bytes = self
                .auxiliary
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            return super::auxiliary::AuxiliaryFile::prepare(bytes, plan.intent).map(Some);
        }
        Ok(None)
    }
}

struct ProcfsOpen(Arc<dyn OpenFileDescription>);

impl fmt::Debug for ProcfsOpen {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProcfsOpen")
    }
}

impl PreparedPathOpen for ProcfsOpen {
    fn object(&self) -> Arc<dyn OpenFileDescription> {
        Arc::clone(&self.0)
    }
    fn commit(&mut self) -> Result<(), RuntimePathError> {
        Ok(())
    }
    fn rollback(self: Box<Self>) {}
}

pub(super) struct NamespaceOpen {
    object: Arc<NamespaceHandle>,
    handles: Arc<NamespaceHandleRegistry>,
}

impl NamespaceOpen {
    pub(super) fn prepare(
        handles: &Arc<NamespaceHandleRegistry>,
        identifier: NamespaceId,
        intent: OpenIntent,
    ) -> Result<Box<dyn PreparedPathOpen>, RuntimePathError> {
        if intent.bits() & (OpenIntent::WRITE | OpenIntent::CREATE | OpenIntent::TRUNCATE) != 0 {
            return Err(RuntimePathError::Access);
        }
        Ok(Box::new(Self {
            object: handles.object(identifier),
            handles: Arc::clone(handles),
        }))
    }
}

impl fmt::Debug for NamespaceOpen {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("NamespaceOpen")
    }
}

impl PreparedPathOpen for NamespaceOpen {
    fn object(&self) -> Arc<dyn OpenFileDescription> {
        self.object.clone()
    }

    fn bind(&mut self, identity: DescriptionIdentity) -> Result<(), RuntimePathError> {
        self.handles
            .bind(identity, &self.object)
            .map_err(|_| RuntimePathError::Io)
    }

    fn commit(&mut self) -> Result<(), RuntimePathError> {
        Ok(())
    }

    fn rollback(self: Box<Self>) {}
}

#[cfg(test)]
mod test {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use hl_descriptor::OpenFileDescription;
    use hl_linux::{OpenAbiPlan, PathOperand, ResolveFlags};
    use hl_runtime::{GuestPathBytes, OpenDirectory, OpenIntent, RuntimePathHost};

    use super::super::{NativePath, projected, watch};
    use super::ProcfsTargets;

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn canonicalizes_devpts_device() {
        let targets = ProcfsTargets::new(
            Arc::new(std::sync::Mutex::new(BTreeMap::new())),
            projected::Registry::default(),
        );
        let metadata_for = |kind, special_device| {
            let timestamp = hl_descriptor::OfdTimestamp {
                seconds: 0,
                nanoseconds: 0,
            };
            hl_descriptor::OfdMetadata {
                device: 0,
                inode: 0,
                kind,
                permissions: 0o666,
                links: 1,
                user: 0,
                group: 0,
                special_device,
                size: 0,
                blocks_512: 0,
                block_size: 4096,
                accessed: timestamp,
                modified: timestamp,
                changed: timestamp,
            }
        };
        let encoded = hl_runtime::DeviceId::new(136, 513).linux_encoded();
        let metadata = metadata_for(2, encoded);
        assert_eq!(
            hl_runtime::ProcfsDescriptorTarget::path(targets.as_ref(), &metadata),
            Ok(b"/dev/pts/513".to_vec())
        );
        let metadata = metadata_for(2, hl_runtime::DeviceId::new(2, 3).linux_encoded());
        assert_eq!(
            hl_runtime::ProcfsDescriptorTarget::path(targets.as_ref(), &metadata),
            Err(hl_runtime::ProcfsError::NotFound)
        );
    }

    #[test]
    fn builtin_descriptor_target_is_canonical_device_path() {
        let targets = ProcfsTargets::new(
            Arc::new(std::sync::Mutex::new(BTreeMap::new())),
            projected::Registry::default(),
        );
        let device =
            hl_runtime::BuiltinDescription::open(hl_runtime::DeviceKind::Null, hl_runtime::DeviceOpenCapability::Read)
                .unwrap();
        let metadata = device.metadata().unwrap();
        assert_eq!(
            hl_runtime::ProcfsDescriptorTarget::path(targets.as_ref(), &metadata).unwrap(),
            b"/dev/null"
        );
    }

    #[test]
    fn procfs_integration() {
        let root = std::env::temp_dir().join(format!(
            "hl-procfs-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed),
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir(&root).unwrap();
        let bytes = root.as_os_str().as_encoded_bytes();
        let tasks = Arc::new(hl_task::TaskRegistry::new(hl_task::RegistryConfig::default()).unwrap());
        let (process, _) = tasks
            .create_init(
                hl_task::ProcessCredentials::new(7, 8, &[], 8).unwrap(),
                hl_task::ProcessLimits::default(),
            )
            .unwrap();
        let host = NativePath::new(bytes, watch::Hub::new(bytes).unwrap())
            .unwrap()
            .with_process(
                Arc::clone(&tasks),
                process,
                Arc::new(hl_runtime::NamespaceHandleRegistry::new()),
                Arc::new(hl_descriptor::DescriptorTable::new(8).unwrap()),
            );
        let plan = OpenAbiPlan {
            operand: PathOperand {
                directory: OpenDirectory::default(),
                path: GuestPathBytes::new(b"/proc/self/limits").unwrap(),
                allow_empty: false,
                nofollow: false,
            },
            intent: OpenIntent::from_bits(OpenIntent::READ),
            mode: 0,
            close_on_exec: false,
            nonblocking: false,
            no_controlling_terminal: false,
            resolve: ResolveFlags::default(),
        };
        let self_plan = OpenAbiPlan {
            operand: PathOperand {
                directory: OpenDirectory::default(),
                path: GuestPathBytes::new(b"/proc/self").unwrap(),
                allow_empty: false,
                nofollow: false,
            },
            ..plan.clone()
        };
        let redirected = host.procfs_plan(&self_plan).unwrap().unwrap();
        assert_eq!(
            redirected.operand.path.as_bytes(),
            format!("/proc/{}", process.number()).as_bytes()
        );
        let mut prepared = host
            .prepare_open(&host.root_base().unwrap(), &plan, &super::super::test_identity())
            .unwrap();
        let object = prepared.object();
        prepared.commit().unwrap();
        let mut output = [0_u8; 4096];
        let count = object.read(&mut output).unwrap();
        let text = std::str::from_utf8(&output[..count]).unwrap();
        assert!(text.contains("Max core file size"));
        assert!(text.contains("Max open files"));
        let namespace = tasks.namespaces(process).unwrap().uts;
        let namespace_path = format!("/proc/{}/ns/uts", process.number());
        let namespace_plan = OpenAbiPlan {
            operand: PathOperand {
                directory: OpenDirectory::default(),
                path: GuestPathBytes::new(namespace_path.as_bytes()).unwrap(),
                allow_empty: false,
                nofollow: false,
            },
            intent: OpenIntent::from_bits(OpenIntent::READ),
            mode: 0,
            close_on_exec: false,
            nonblocking: false,
            no_controlling_terminal: false,
            resolve: ResolveFlags::default(),
        };
        let mut namespace_open = host
            .prepare_open(
                &host.root_base().unwrap(),
                &namespace_plan,
                &super::super::test_identity(),
            )
            .unwrap();
        let namespace_object = namespace_open.object();
        assert_eq!(namespace_object.metadata().unwrap().inode, namespace.serial);
        namespace_open.commit().unwrap();

        let mount_path = format!("/proc/{}/ns/mnt", process.number());
        let mount_operand = PathOperand {
            directory: OpenDirectory::default(),
            path: GuestPathBytes::new(mount_path.as_bytes()).unwrap(),
            allow_empty: false,
            nofollow: false,
        };
        let mount = host.resolve(&host.root_base().unwrap(), &mount_operand).unwrap();
        let metadata = mount.metadata().unwrap();
        assert_eq!(metadata.identity.inode, 4_026_531_841);
        assert_eq!(metadata.kind, hl_runtime::FileKind::Regular);

        // Measured on Linux 7.0.11: `lstat` of every `/proc/<pid>/ns` entry is
        // `120777` with the namespace identity as the inode, and an `open` of one
        // binds the nsfs object rather than resolving the "mnt:[...]" target.
        let link_operand = PathOperand {
            nofollow: true,
            ..mount_operand.clone()
        };
        let link = host.resolve(&host.root_base().unwrap(), &link_operand).unwrap();
        let link_metadata = link.metadata().unwrap();
        assert_eq!(link_metadata.kind, hl_runtime::FileKind::Symlink);
        assert_eq!(link_metadata.permissions.bits(), 0o777);
        assert_eq!(link_metadata.identity.inode, 4_026_531_841);
        assert_eq!(link.read_link().unwrap(), b"mnt:[4026531841]");

        let mount_plan = OpenAbiPlan {
            operand: mount_operand.clone(),
            ..plan.clone()
        };
        assert!(host.procfs_plan(&mount_plan).unwrap().is_none());
        let mut mount_open = host
            .prepare_open(&host.root_base().unwrap(), &mount_plan, &super::super::test_identity())
            .unwrap();
        let mount_object = mount_open.object();
        let mount_raw = mount_object.metadata().unwrap();
        assert_eq!(mount_raw.inode, 4_026_531_841);
        assert_eq!((mount_raw.kind, mount_raw.permissions), (8, 0o444));
        mount_open.commit().unwrap();

        // `auxv` and `pagemap` stat and open on every pid spelling, not only the
        // two `self` literals the boundary used to match.
        for leaf in ["auxv", "pagemap"] {
            let path = format!("/proc/{}/{leaf}", process.number());
            let operand = PathOperand {
                directory: OpenDirectory::default(),
                path: GuestPathBytes::new(path.as_bytes()).unwrap(),
                allow_empty: false,
                nofollow: false,
            };
            let node = host.resolve(&host.root_base().unwrap(), &operand).unwrap();
            let node_metadata = node.metadata().unwrap();
            assert_eq!((leaf, node_metadata.kind), (leaf, hl_runtime::FileKind::Regular));
            assert_eq!((leaf, node_metadata.permissions.bits()), (leaf, 0o400));
            assert_eq!((leaf, node_metadata.size), (leaf, 0));
            assert_eq!((leaf, node_metadata.block_size), (leaf, 1024));
            let leaf_plan = OpenAbiPlan {
                operand,
                ..plan.clone()
            };
            let mut opened = host
                .prepare_open(&host.root_base().unwrap(), &leaf_plan, &super::super::test_identity())
                .unwrap();
            let _ = opened.object();
            opened.commit().unwrap();
        }

        drop(object);
        drop(prepared);
        drop(host);
        std::fs::remove_dir_all(root).unwrap();
    }
}
