use hl_vfs::{XattrFlags, XattrName};

use super::plan::{AbiError, Target};
use crate::{GuestMarshaller, GuestMemory, MarshalError, PathOperand};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedFilesystemCopyout {
    pub(crate) writes: Vec<(u64, Vec<u8>)>,
    pub result_length: usize,
}

impl StagedFilesystemCopyout {
    pub fn commit<M: GuestMemory>(self, marshaller: &GuestMarshaller<'_, M>) -> Result<usize, AbiError> {
        for (address, bytes) in self.writes {
            let progress = marshaller.copy_to(address, &bytes);
            if let Some(fault) = progress.fault {
                return Err(MarshalError::Fault(fault).into());
            }
        }
        Ok(self.result_length)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimestampChange {
    Omit,
    Now,
    Value { seconds: i64, nanoseconds: u32 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FsMutationPlan {
    CreateDirectory {
        target: PathOperand,
        mode: u32,
    },
    CreateNode {
        target: PathOperand,
        mode: u32,
        device: u64,
    },
    Unlink {
        target: PathOperand,
        directory: bool,
    },
    Rename {
        from: PathOperand,
        to: PathOperand,
        exchange: bool,
        no_replace: bool,
    },
    Link {
        from: PathOperand,
        to: PathOperand,
        follow: bool,
    },
    Symlink {
        target: Vec<u8>,
        link: PathOperand,
    },
    Chmod {
        target: PathOperand,
        mode: u32,
    },
    Chown {
        target: PathOperand,
        user: Option<u32>,
        group: Option<u32>,
    },
    SetTimes {
        target: PathOperand,
        times: [TimestampChange; 2],
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum XattrPlan {
    Set {
        target: Target,
        name: XattrName,
        value: Vec<u8>,
        flags: XattrFlags,
    },
    Get {
        target: Target,
        name: XattrName,
        output: u64,
        size: usize,
    },
    List {
        target: Target,
        output: u64,
        size: usize,
    },
    Remove {
        target: Target,
        name: XattrName,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LockType {
    Read,
    Write,
    Unlock,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileLock {
    pub lock_type: LockType,
    pub whence: i16,
    pub start: i64,
    pub length: i64,
    pub process: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryRecord {
    pub inode: u64,
    pub offset: i64,
    pub file_type: u8,
    pub name: Vec<u8>,
}
