use hl_isa::GuestArchitecture;
use hl_vfs::{
    Access, FileMetadata, FilesystemStats, GuestPathBytes, OpenDirectory, OpenIntent, PathError, XATTR_LIST_MAXIMUM,
    XATTR_NAME_MAXIMUM, XATTR_VALUE_MAXIMUM, XattrFlags, XattrName,
};

use super::plan::{AbiError, Target};
use crate::{
    AccessPlan, DirectoryRecord, FileLock, GuestAccess, GuestFault, GuestMarshaller, GuestMemory, LockType,
    MarshalError, OpenAbiPlan, PathOperand, ResolveFlags, STATFS_SIZE, STATX_SIZE, StagedFilesystemCopyout,
    StatEncoder, StatOutputKind, StatfsEncoder, XattrPlan,
};

const PATH_MAXIMUM: usize = 4096;
const AT_FDCWD: i32 = -100;
const AT_SYMLINK_NOFOLLOW: u32 = 0x100;
const AT_EMPTY_PATH: u32 = 0x1000;
const OPEN_CLOEXEC: u64 = 0x8_0000;
const OPEN_NONBLOCK: u64 = 0x800;
const OPEN_ALLOWED: u64 = 0x7f_ffc3;

pub struct Abi<'a, M: GuestMemory> {
    pub(crate) marshaller: GuestMarshaller<'a, M>,
    architecture: GuestArchitecture,
}

impl<'a, M: GuestMemory> Abi<'a, M> {
    #[must_use]
    pub const fn new(memory: &'a M, architecture: GuestArchitecture) -> Self {
        Self {
            marshaller: GuestMarshaller::new(memory, architecture),
            architecture,
        }
    }

    pub fn openat(&self, directory: i32, path: u64, flags: u64, mode: u32) -> Result<OpenAbiPlan, AbiError> {
        self.open_plan(directory, path, flags, mode, 0, false)
    }

    pub fn open(&self, path: u64, flags: u64, mode: u32) -> Result<OpenAbiPlan, AbiError> {
        self.openat(AT_FDCWD, path, flags, mode)
    }

    pub fn openat2(&self, directory: i32, path: u64, how_pointer: u64, size: usize) -> Result<OpenAbiPlan, AbiError> {
        if size < 24 {
            return Err(AbiError::Invalid);
        }
        if size > 4096 {
            return Err(AbiError::TooBig);
        }
        let mut how = vec![0; size];
        let progress = self.marshaller.copy_from(how_pointer, &mut how);
        if let Some(fault) = progress.fault {
            return Err(MarshalError::Fault(fault).into());
        }
        if how[24..].iter().any(|byte| *byte != 0) {
            return Err(AbiError::TooBig);
        }
        let flags = Self::u64(&how, 0);
        let mode = Self::u64(&how, 8);
        let resolve = Self::u64(&how, 16);
        if mode > 0o7777 {
            return Err(AbiError::Invalid);
        }
        self.open_plan(directory, path, flags, mode as u32, resolve, true)
    }

    fn open_plan(
        &self,
        directory: i32,
        path: u64,
        flags: u64,
        mode: u32,
        resolve: u64,
        strict: bool,
    ) -> Result<OpenAbiPlan, AbiError> {
        if strict && flags & !OPEN_ALLOWED != 0 {
            return Err(AbiError::Invalid);
        }
        let access = flags & 3;
        if access == 3 || resolve & !0x3f != 0 {
            return Err(AbiError::Invalid);
        }
        let (directory_flag, nofollow) = match self.architecture {
            GuestArchitecture::Aarch64 => (0x4_000, 0x8_000),
            GuestArchitecture::X86_64 => (0x10_000, 0x20_000),
        };
        let temporary = 0x40_0000 | directory_flag;
        let path_only = flags & 0x20_0000 != 0;
        let temporary_bit = flags & 0x40_0000 != 0;
        if !path_only
            && (temporary_bit && flags & temporary != temporary || flags & temporary == temporary && access == 0)
        {
            return Err(AbiError::Invalid);
        }
        if mode != 0 && flags & 0x40 == 0 && flags & temporary != temporary {
            return Err(AbiError::Invalid);
        }
        let mut intent = match access {
            0 => OpenIntent::READ,
            1 => OpenIntent::WRITE,
            _ => OpenIntent::READ | OpenIntent::WRITE,
        };
        for (linux, neutral) in [
            (0x40, OpenIntent::CREATE),
            (0x80, OpenIntent::EXCLUSIVE),
            (0x200, OpenIntent::TRUNCATE),
            (0x400, OpenIntent::APPEND),
            (0x20_0000, OpenIntent::PATH_ONLY),
            (nofollow, OpenIntent::NOFOLLOW),
            (directory_flag, OpenIntent::DIRECTORY),
        ] {
            if flags & linux != 0 {
                intent |= neutral;
            }
        }
        if !path_only && flags & temporary == temporary {
            intent |= OpenIntent::TEMPORARY;
        }
        if resolve & 0x4 != 0 {
            intent |= OpenIntent::NO_SYMLINKS;
        }
        Ok(OpenAbiPlan {
            operand: self.path_operand(directory, path, false, flags & nofollow != 0)?,
            intent: OpenIntent::from_bits(intent),
            mode: mode & 0o7777,
            close_on_exec: flags & OPEN_CLOEXEC != 0,
            nonblocking: flags & OPEN_NONBLOCK != 0,
            no_controlling_terminal: flags & 0x100 != 0,
            resolve: Self::resolve_flags(resolve),
        })
    }

    fn resolve_flags(bits: u64) -> ResolveFlags {
        ResolveFlags {
            no_cross_device: bits & 1 != 0,
            no_magic_links: bits & 2 != 0,
            no_symlinks: bits & 4 != 0,
            beneath: bits & 8 != 0,
            in_root: bits & 0x10 != 0,
            cached: bits & 0x20 != 0,
        }
    }

    pub fn path_operand(
        &self,
        directory: i32,
        pointer: u64,
        allow_empty: bool,
        nofollow: bool,
    ) -> Result<PathOperand, AbiError> {
        let bytes = self
            .marshaller
            .c_string(pointer, PATH_MAXIMUM)
            .map_err(|error| match error {
                MarshalError::TooBig => AbiError::NameTooLong,
                other => AbiError::Marshal(other),
            })?;
        if bytes.is_empty() && !allow_empty {
            return Err(AbiError::NoEntry);
        }
        let path = GuestPathBytes::new(&bytes).map_err(|error| {
            if error == PathError::TooLong {
                AbiError::NameTooLong
            } else {
                AbiError::Invalid
            }
        })?;
        Ok(PathOperand {
            directory: OpenDirectory::from_raw(directory as i64 as u64),
            path,
            allow_empty,
            nofollow,
        })
    }

    pub fn access(&self, directory: i32, path: u64, mode: u32, flags: u32) -> Result<AccessPlan, AbiError> {
        if mode & !7 != 0 || flags & !(AT_SYMLINK_NOFOLLOW | AT_EMPTY_PATH | 0x200) != 0 {
            return Err(AbiError::Invalid);
        }
        Ok(AccessPlan {
            operand: self.path_operand(
                directory,
                path,
                flags & AT_EMPTY_PATH != 0,
                flags & AT_SYMLINK_NOFOLLOW != 0,
            )?,
            access: Access::from_bits(mode as u8).map_err(|_| AbiError::Invalid)?,
            effective_ids: flags & 0x200 != 0,
        })
    }

    pub fn stat_operand(&self, directory: i32, path: u64, flags: u32) -> Result<PathOperand, AbiError> {
        if flags & !(AT_SYMLINK_NOFOLLOW | AT_EMPTY_PATH) != 0 {
            return Err(AbiError::Invalid);
        }
        self.path_operand(
            directory,
            path,
            flags & AT_EMPTY_PATH != 0,
            flags & AT_SYMLINK_NOFOLLOW != 0,
        )
    }

    pub fn statx_operand(
        &self,
        directory: i32,
        path: u64,
        flags: u32,
        mask: u32,
    ) -> Result<(PathOperand, u32), AbiError> {
        if flags & !0x7900 != 0 || flags & 0x6000 == 0x6000 || mask & 0x8000_0000 != 0 {
            return Err(AbiError::Invalid);
        }
        self.path_operand(
            directory,
            path,
            flags & AT_EMPTY_PATH != 0,
            flags & AT_SYMLINK_NOFOLLOW != 0,
        )
        .map(|operand| (operand, mask))
    }

    pub fn stat(&self, path: u64) -> Result<Target, AbiError> {
        self.stat_operand(AT_FDCWD, path, 0).map(Target::Path)
    }

    #[must_use]
    pub const fn fstat(descriptor: i32) -> Target {
        Target::Descriptor(descriptor)
    }

    pub fn stage_stat(
        &self,
        output: u64,
        metadata: &FileMetadata,
        kind: StatOutputKind,
    ) -> Result<StagedFilesystemCopyout, AbiError> {
        let mut bytes = vec![
            0;
            match kind {
                StatOutputKind::Stat => self.architecture.linux_stat_size(),
                StatOutputKind::Statx { .. } => STATX_SIZE,
            }
        ];
        match kind {
            StatOutputKind::Stat => {
                StatEncoder::encode_stat(self.architecture, metadata, &mut bytes)?;
            }
            StatOutputKind::Statx { extensions } => {
                StatEncoder::encode_statx(metadata, extensions, &mut bytes)?;
            }
        }
        self.stage_bytes(output, bytes)
    }

    pub fn stage_statfs(&self, output: u64, stats: FilesystemStats) -> Result<StagedFilesystemCopyout, AbiError> {
        let mut bytes = vec![0; STATFS_SIZE];
        StatfsEncoder::encode(self.architecture, stats, &mut bytes).map_err(|()| AbiError::Invalid)?;
        self.stage_bytes(output, bytes)
    }

    pub fn stage_readlink(
        &self,
        output: u64,
        capacity: usize,
        target: &[u8],
    ) -> Result<StagedFilesystemCopyout, AbiError> {
        self.stage_bytes(output, target[..target.len().min(capacity)].to_vec())
    }

    pub fn probe_readlink_output(&self, output: u64, capacity: usize) -> Result<(), AbiError> {
        let length = capacity.min(PATH_MAXIMUM);
        let available = self.marshaller.probe(output, length, GuestAccess::Write)?;
        if available != length {
            return Err(AbiError::Marshal(MarshalError::Fault(GuestFault {
                address: output + available as u64,
                access: GuestAccess::Write,
            })));
        }
        Ok(())
    }

    pub fn stage_getdents(
        &self,
        output: u64,
        capacity: usize,
        records: &[DirectoryRecord],
    ) -> Result<StagedFilesystemCopyout, AbiError> {
        let mut bytes = Vec::new();
        for record in records {
            if record.name.contains(&0) {
                return Err(AbiError::Invalid);
            }
            let length = (19 + record.name.len() + 1 + 7) & !7;
            if bytes.len() + length > capacity {
                break;
            }
            bytes.extend_from_slice(&record.inode.to_le_bytes());
            bytes.extend_from_slice(&record.offset.to_le_bytes());
            bytes.extend_from_slice(&(length as u16).to_le_bytes());
            bytes.push(record.file_type);
            bytes.extend_from_slice(&record.name);
            bytes.resize(bytes.len() + 1, 0);
            bytes.resize((bytes.len() + 7) & !7, 0);
        }
        self.stage_bytes(output, bytes)
    }

    fn stage_bytes(&self, output: u64, bytes: Vec<u8>) -> Result<StagedFilesystemCopyout, AbiError> {
        let available = self.marshaller.probe(output, bytes.len(), GuestAccess::Write)?;
        if available != bytes.len() {
            return Err(AbiError::Invalid);
        }
        let result_length = bytes.len();
        Ok(StagedFilesystemCopyout {
            writes: vec![(output, bytes)],
            result_length,
        })
    }

    pub fn xattr_set(
        &self,
        target: Target,
        name_pointer: u64,
        value_pointer: u64,
        size: usize,
        flags: u32,
    ) -> Result<XattrPlan, AbiError> {
        let name = self.xattr_name(name_pointer)?;
        if size > XATTR_VALUE_MAXIMUM {
            return Err(AbiError::Invalid);
        }
        let flags = XattrFlags::from_bits(flags).map_err(|_| AbiError::Invalid)?;
        let mut value = vec![0; size];
        let progress = self.marshaller.copy_from(value_pointer, &mut value);
        if let Some(fault) = progress.fault {
            return Err(MarshalError::Fault(fault).into());
        }
        Ok(XattrPlan::Set {
            target,
            name,
            value,
            flags,
        })
    }

    pub fn xattr_output(
        target: Target,
        name: Option<XattrName>,
        output: u64,
        size: usize,
    ) -> Result<XattrPlan, AbiError> {
        if size > XATTR_LIST_MAXIMUM {
            return Err(AbiError::Invalid);
        }
        Ok(match name {
            Some(name) => XattrPlan::Get {
                target,
                name,
                output,
                size,
            },
            None => XattrPlan::List { target, output, size },
        })
    }

    pub fn xattr_get(
        &self,
        target: Target,
        name_pointer: u64,
        output: u64,
        size: usize,
    ) -> Result<XattrPlan, AbiError> {
        let name = self.xattr_name(name_pointer)?;
        Self::xattr_output(target, Some(name), output, size)
    }

    pub fn xattr_list(target: Target, output: u64, size: usize) -> Result<XattrPlan, AbiError> {
        Self::xattr_output(target, None, output, size)
    }

    pub fn xattr_remove(&self, target: Target, name_pointer: u64) -> Result<XattrPlan, AbiError> {
        Ok(XattrPlan::Remove {
            target,
            name: self.xattr_name(name_pointer)?,
        })
    }

    pub fn stage_xattr_output(
        &self,
        output: u64,
        capacity: usize,
        bytes: Vec<u8>,
    ) -> Result<StagedFilesystemCopyout, AbiError> {
        if capacity == 0 || bytes.is_empty() {
            return Ok(StagedFilesystemCopyout {
                writes: Vec::new(),
                result_length: bytes.len(),
            });
        }
        if capacity < bytes.len() {
            return Err(AbiError::Range);
        }
        self.stage_bytes(output, bytes)
    }

    fn xattr_name(&self, pointer: u64) -> Result<XattrName, AbiError> {
        let bytes = self
            .marshaller
            .c_string(pointer, XATTR_NAME_MAXIMUM + 1)
            .map_err(|error| match error {
                MarshalError::TooBig => AbiError::NameTooLong,
                other => AbiError::Marshal(other),
            })?;
        XattrName::new(&bytes).map_err(|_| AbiError::Invalid)
    }

    pub fn file_lock(&self, pointer: u64) -> Result<FileLock, AbiError> {
        let bytes = self.marshaller.copy_struct_from::<32>(pointer)?;
        let lock_type = match i16::from_le_bytes(bytes[..2].try_into().unwrap()) {
            0 => LockType::Read,
            1 => LockType::Write,
            2 => LockType::Unlock,
            _ => return Err(AbiError::Invalid),
        };
        Ok(FileLock {
            lock_type,
            whence: i16::from_le_bytes(bytes[2..4].try_into().unwrap()),
            start: i64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            length: i64::from_le_bytes(bytes[16..24].try_into().unwrap()),
            process: i32::from_le_bytes(bytes[24..28].try_into().unwrap()),
        })
    }

    pub fn flock_operation(operation: u32) -> Result<u32, AbiError> {
        if operation & !0xf != 0
            || [operation & 0xb == 1, operation & 0xb == 2, operation & 0xb == 8]
                .into_iter()
                .filter(|selected| *selected)
                .count()
                != 1
        {
            return Err(AbiError::Invalid);
        }
        Ok(operation)
    }

    pub fn stage_file_lock(&self, pointer: u64, lock: FileLock) -> Result<StagedFilesystemCopyout, AbiError> {
        let mut bytes = vec![0; 32];
        let kind = match lock.lock_type {
            LockType::Read => 0_i16,
            LockType::Write => 1,
            LockType::Unlock => 2,
        };
        bytes[..2].copy_from_slice(&kind.to_le_bytes());
        bytes[2..4].copy_from_slice(&lock.whence.to_le_bytes());
        bytes[8..16].copy_from_slice(&lock.start.to_le_bytes());
        bytes[16..24].copy_from_slice(&lock.length.to_le_bytes());
        bytes[24..28].copy_from_slice(&lock.process.to_le_bytes());
        self.stage_bytes(pointer, bytes)
    }

    fn u64(bytes: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
    }
}
