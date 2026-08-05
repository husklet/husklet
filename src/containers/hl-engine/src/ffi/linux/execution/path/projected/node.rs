use std::fmt;
use std::sync::{Arc, Mutex};

use hl_linux::{AccessPlan, PathOperand};
use hl_runtime::{
    DirectoryBaseLease, FileIdentity, FileKind, FileMetadata, FileTimestamp, Permissions, ResolvedPathLease,
    RuntimePathError,
};

use super::super::source::ProjectedContext;
use super::{Error, Path, Registry};

pub(in crate::ffi::linux::execution::path) struct Node {
    tree: Arc<Mutex<crate::native::AuthorityWorker>>,
    handle: u64,
    stat: hl_provider::TreeStat,
}

impl Node {
    pub(in crate::ffi::linux::execution::path) fn resolve(
        context: &ProjectedContext,
        base: &DirectoryBaseLease,
        operand: &PathOperand,
        files: &Registry,
    ) -> Result<Box<dyn ResolvedPathLease>, RuntimePathError> {
        let path = Path::join(context.root(), base, operand.path.as_bytes())?;
        let tree = Arc::clone(context.tree()?);
        let relative = operand.path.as_bytes().first() != Some(&b'/');
        let base_handle = base
            .descriptor_lease()
            .and_then(|lease| lease.metadata().ok())
            .and_then(|metadata| files.get(&(metadata.device, metadata.inode)))
            .map(|file| file.handle);
        let handle = {
            let mut worker = tree.lock().map_err(|_| RuntimePathError::Io)?;
            match (relative, base_handle, operand.nofollow) {
                (true, Some(base), true) => worker.tree_link_at(base, operand.path.as_bytes()),
                (true, Some(base), false) => worker.tree_open_at(base, operand.path.as_bytes(), false),
                (_, _, true) => worker.tree_open_link(&path),
                (_, _, false) => worker.tree_open(&path, false),
            }
        }
        .map_err(Error::path)?;
        let stat = tree
            .lock()
            .map_err(|_| RuntimePathError::Io)?
            .tree_stat(handle)
            .map_err(Error::path)?;
        Ok(Box::new(Self { tree, handle, stat }))
    }
}

impl fmt::Debug for Node {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProjectedNode")
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        let _ = self.tree.lock().map(|mut worker| worker.tree_close(self.handle));
    }
}

impl ResolvedPathLease for Node {
    fn metadata(&self) -> Result<FileMetadata, RuntimePathError> {
        Metadata::project(&self.stat)
    }

    fn read_link(&self) -> Result<Vec<u8>, RuntimePathError> {
        self.tree
            .lock()
            .map_err(|_| RuntimePathError::Io)?
            .tree_read_link(self.handle, hl_provider::TreeWire::MAX_DATA)
            .map_err(Error::path)
    }

    fn access(&self, plan: &AccessPlan) -> Result<(), RuntimePathError> {
        let requested = plan.access.bits();
        let mode = self.stat.mode;
        if requested & 1 != 0 && mode & 0o111 == 0 {
            return Err(RuntimePathError::Access);
        }
        if requested & 2 != 0 && mode & 0o222 == 0 {
            return Err(RuntimePathError::Access);
        }
        if requested & 4 != 0 && mode & 0o444 == 0 {
            return Err(RuntimePathError::Access);
        }
        Ok(())
    }
}

struct Metadata;

impl Metadata {
    fn project(stat: &hl_provider::TreeStat) -> Result<FileMetadata, RuntimePathError> {
        let kind = match stat.mode & 0o170_000 {
            0o010_000 => FileKind::Fifo,
            0o020_000 => FileKind::Character,
            0o040_000 => FileKind::Directory,
            0o060_000 => FileKind::Block,
            0o100_000 => FileKind::Regular,
            0o120_000 => FileKind::Symlink,
            0o140_000 => FileKind::Socket,
            _ => return Err(RuntimePathError::Invalid),
        };
        let zero = FileTimestamp {
            seconds: 0,
            nanoseconds: 0,
        };
        Ok(FileMetadata {
            identity: FileIdentity {
                device: stat.device,
                inode: stat.inode,
            },
            kind,
            permissions: Permissions::from_bits((stat.mode & 0o7777) as u16),
            links: 1,
            user: 0,
            group: 0,
            special_device: 0,
            size: stat.size,
            blocks_512: stat.size.div_ceil(512),
            accessed: zero,
            modified: zero,
            changed: zero,
        })
    }
}
