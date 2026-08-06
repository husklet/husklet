use std::fmt;
use std::sync::{Arc, Mutex};

use hl_descriptor::{
    DescriptionIdentity, DescriptorSnapshot, DescriptorTable, DirectoryBatch, DirectoryBatchToken, ObjectError,
    ObjectKind, OfdDirectoryEntry, OfdMetadata, OpenFileDescription, SnapshotBudget,
};
use hl_vfs::{ProcfsDescriptorView, ProcfsError};

use super::TaskProcfs;

/// Consumer port resolving regular descriptor identity to its guest-visible path.
pub trait DescriptorTarget: Send + Sync {
    fn path(&self, metadata: &OfdMetadata) -> Result<Vec<u8>, ProcfsError>;
}

const MAXIMUM_DESCRIPTORS: usize = 65_536;
const MAXIMUM_SNAPSHOT_BYTES: usize = 16 * 1024 * 1024;

enum DirectoryState {
    Pending,
    Ready {
        entries: Vec<OfdDirectoryEntry>,
        position: usize,
    },
}

pub(super) struct DescriptorDirectory {
    descriptors: Arc<DescriptorTable>,
    file_type: u8,
    metadata: OfdMetadata,
    budget: SnapshotBudget,
    state: Mutex<DirectoryState>,
}

impl DescriptorDirectory {
    pub(super) fn new(descriptors: Arc<DescriptorTable>, file_type: u8, metadata: OfdMetadata) -> Self {
        Self {
            descriptors,
            file_type,
            metadata,
            budget: SnapshotBudget {
                max_items: MAXIMUM_DESCRIPTORS,
                max_peak_bytes: MAXIMUM_SNAPSHOT_BYTES,
            },
            state: Mutex::new(DirectoryState::Pending),
        }
    }

    #[cfg(test)]
    pub(super) fn with_budget(
        descriptors: Arc<DescriptorTable>,
        file_type: u8,
        metadata: OfdMetadata,
        budget: SnapshotBudget,
    ) -> Self {
        Self {
            descriptors,
            file_type,
            metadata,
            budget,
            state: Mutex::new(DirectoryState::Pending),
        }
    }

    fn entries(&self) -> Result<Vec<OfdDirectoryEntry>, ObjectError> {
        let mut numbers = self
            .descriptors
            .bounded_active_snapshots(self.budget)
            .map_err(|_| ObjectError::ResourceLimit)?
            .into_iter()
            .map(|snapshot| snapshot.number)
            .collect::<Vec<_>>();
        numbers.sort_unstable();
        let mut entries = vec![
            OfdDirectoryEntry {
                inode: self.metadata.inode,
                cookie: 1,
                file_type: 4,
                name: b".".to_vec(),
            },
            OfdDirectoryEntry {
                inode: self.metadata.inode,
                cookie: 2,
                file_type: 4,
                name: b"..".to_vec(),
            },
        ];
        entries.extend(
            numbers
                .into_iter()
                .enumerate()
                .map(|(index, number)| OfdDirectoryEntry {
                    inode: u64::try_from(number).unwrap_or(0),
                    cookie: i64::try_from(index + 3).unwrap_or(i64::MAX),
                    file_type: self.file_type,
                    name: number.to_string().into_bytes(),
                }),
        );
        Ok(entries)
    }
}

impl fmt::Debug for DescriptorDirectory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProcfsDescriptorDirectory")
    }
}

impl OpenFileDescription for DescriptorDirectory {
    fn kind(&self) -> ObjectKind {
        ObjectKind::Directory
    }

    fn metadata(&self) -> Result<OfdMetadata, ObjectError> {
        Ok(self.metadata.clone())
    }

    fn read_directory(&self, maximum: usize) -> Result<DirectoryBatch, ObjectError> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if maximum == 0 {
            let position = match &*state {
                DirectoryState::Pending => 0,
                DirectoryState::Ready { position, .. } => *position,
            };
            return Ok(DirectoryBatch {
                token: DirectoryBatchToken {
                    generation: 1,
                    cookie: i64::try_from(position).map_err(|_| ObjectError::InvalidArgument)?,
                },
                entries: Vec::new(),
            });
        }
        if matches!(*state, DirectoryState::Pending) {
            let entries = self.entries()?;
            *state = DirectoryState::Ready { entries, position: 0 };
        }
        let DirectoryState::Ready { entries, position } = &*state else {
            unreachable!()
        };
        Ok(DirectoryBatch {
            token: DirectoryBatchToken {
                generation: 1,
                cookie: i64::try_from(*position).map_err(|_| ObjectError::InvalidArgument)?,
            },
            entries: entries.iter().skip(*position).take(maximum).cloned().collect(),
        })
    }

    fn commit_directory(&self, token: DirectoryBatchToken, count: usize) -> Result<(), ObjectError> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        match &mut *state {
            DirectoryState::Pending if token.generation == 1 && token.cookie == 0 && count == 0 => Ok(()),
            DirectoryState::Ready { entries, position }
                if token.generation == 1 && token.cookie == *position as i64 =>
            {
                *position = position
                    .checked_add(count)
                    .filter(|next| *next <= entries.len())
                    .ok_or(ObjectError::InvalidArgument)?;
                Ok(())
            }
            _ => Err(ObjectError::InvalidArgument),
        }
    }
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
            || lease.description_identity()
                != (DescriptionIdentity {
                    identity: snapshot.description_identity,
                    generation: snapshot.description_generation,
                })
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
