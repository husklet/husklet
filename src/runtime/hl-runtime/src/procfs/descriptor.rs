use hl_descriptor::{DescriptorSnapshot, DescriptorTable, ObjectKind, OfdMetadata};
use hl_vfs::{ProcfsDescriptorView, ProcfsError};

use super::TaskProcfs;

/// Consumer port resolving regular descriptor identity to its guest-visible path.
pub trait DescriptorTarget: Send + Sync {
    fn path(&self, metadata: &OfdMetadata) -> Result<Vec<u8>, ProcfsError>;
}

impl TaskProcfs {
    pub(super) fn descriptor_table(
        &self,
        process: hl_task::ProcessId,
    ) -> Result<std::sync::Arc<DescriptorTable>, ProcfsError> {
        match &self.resources {
            Some(resources) => resources.descriptors(process),
            None if self.current == Some(process) => self.descriptors.clone().ok_or(ProcfsError::NotFound),
            None => Err(ProcfsError::NotFound),
        }
    }

    pub(super) fn descriptor_view(
        &self,
        descriptors: &DescriptorTable,
        snapshot: DescriptorSnapshot,
    ) -> Result<ProcfsDescriptorView, ProcfsError> {
        let lease = descriptors.pin(snapshot.number).map_err(|_| ProcfsError::NotFound)?;
        if lease.descriptor_generation() != snapshot.descriptor_generation
            || lease.description_identity().identity != snapshot.description_identity
        {
            return Err(ProcfsError::NotFound);
        }
        let metadata = lease.metadata().ok();
        let (mount, inode, target) = match snapshot.kind {
            ObjectKind::File | ObjectKind::Directory => {
                let metadata = metadata.ok_or(ProcfsError::NotFound)?;
                let target = self.targets.as_ref().and_then(|targets| targets.path(&metadata).ok());
                (Some(metadata.device), metadata.inode, target)
            }
            ObjectKind::Socket => (
                None,
                snapshot.description_identity,
                Some(format!("socket:[{}]", snapshot.description_identity).into_bytes()),
            ),
            ObjectKind::Pipe => (
                None,
                snapshot.description_identity,
                Some(format!("pipe:[{}]", snapshot.description_identity).into_bytes()),
            ),
            ObjectKind::Event | ObjectKind::EventCounter | ObjectKind::Poll | ObjectKind::Other => (
                None,
                snapshot.description_identity,
                Some(format!("anon_inode:[{}]", snapshot.description_identity).into_bytes()),
            ),
        };
        Ok(ProcfsDescriptorView {
            number: snapshot.number,
            offset: snapshot.offset,
            flags: snapshot.status.bits(),
            mount,
            inode,
            target,
        })
    }
}
