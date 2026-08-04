use super::abi::Abi;
use super::plan::AbiError;
use crate::{FsMutationPlan, GuestMemory, PathOperand, TimestampChange};
use hl_vfs::{GuestPathBytes, OpenDirectory};

const AT_SYMLINK_NOFOLLOW: u32 = 0x100;
const AT_REMOVEDIR: u32 = 0x200;
const AT_SYMLINK_FOLLOW: u32 = 0x400;
const AT_EMPTY_PATH: u32 = 0x1000;
const UTIME_NOW: i64 = 0x3fff_ffff;
const UTIME_OMIT: i64 = 0x3fff_fffe;

impl<M: GuestMemory> Abi<'_, M> {
    pub fn mkdirat(&self, directory: i32, path: u64, mode: u32) -> Result<FsMutationPlan, AbiError> {
        Ok(FsMutationPlan::CreateDirectory {
            target: self.path_operand(directory, path, false, false)?,
            mode: mode & 0o7777,
        })
    }

    pub fn mknodat(&self, directory: i32, path: u64, mode: u32, device: u64) -> Result<FsMutationPlan, AbiError> {
        Ok(FsMutationPlan::CreateNode {
            target: self.path_operand(directory, path, false, false)?,
            mode,
            device,
        })
    }

    pub fn unlinkat(&self, directory: i32, path: u64, flags: u32) -> Result<FsMutationPlan, AbiError> {
        if flags & !AT_REMOVEDIR != 0 {
            return Err(AbiError::Invalid);
        }
        Ok(FsMutationPlan::Unlink {
            target: self.path_operand(directory, path, false, false)?,
            directory: flags & AT_REMOVEDIR != 0,
        })
    }

    pub fn renameat2(
        &self,
        old_directory: i32,
        old_path: u64,
        new_directory: i32,
        new_path: u64,
        flags: u32,
    ) -> Result<FsMutationPlan, AbiError> {
        if flags & !3 != 0 || flags == 3 {
            return Err(AbiError::Invalid);
        }
        let from = self.path_operand(old_directory, old_path, false, false)?;
        let to = self.path_operand(new_directory, new_path, false, false)?;
        Ok(FsMutationPlan::Rename {
            from,
            to,
            exchange: flags & 2 != 0,
            no_replace: flags & 1 != 0,
        })
    }

    pub fn linkat(
        &self,
        old_directory: i32,
        old_path: u64,
        new_directory: i32,
        new_path: u64,
        flags: u32,
    ) -> Result<FsMutationPlan, AbiError> {
        if flags & !(AT_SYMLINK_FOLLOW | AT_EMPTY_PATH) != 0 {
            return Err(AbiError::Invalid);
        }
        let from = self.path_operand(
            old_directory,
            old_path,
            flags & AT_EMPTY_PATH != 0,
            flags & AT_SYMLINK_FOLLOW == 0,
        )?;
        let to = self.path_operand(new_directory, new_path, false, false)?;
        Ok(FsMutationPlan::Link {
            from,
            to,
            follow: flags & AT_SYMLINK_FOLLOW != 0,
        })
    }

    pub fn symlinkat(&self, target: u64, directory: i32, link: u64) -> Result<FsMutationPlan, AbiError> {
        let target = self.marshaller.c_string(target, 4097).map_err(|error| match error {
            crate::MarshalError::TooBig => AbiError::NameTooLong,
            other => AbiError::Marshal(other),
        })?;
        if target.is_empty() {
            return Err(AbiError::Invalid);
        }
        Ok(FsMutationPlan::Symlink {
            target,
            link: self.path_operand(directory, link, false, false)?,
        })
    }

    pub fn chmodat(&self, directory: i32, path: u64, mode: u32, flags: u32) -> Result<FsMutationPlan, AbiError> {
        if flags & !(AT_SYMLINK_NOFOLLOW | AT_EMPTY_PATH) != 0 {
            return Err(AbiError::Invalid);
        }
        Ok(FsMutationPlan::Chmod {
            target: self.path_operand(
                directory,
                path,
                flags & AT_EMPTY_PATH != 0,
                flags & AT_SYMLINK_NOFOLLOW != 0,
            )?,
            mode: mode & 0o7777,
        })
    }

    pub fn chownat(
        &self,
        directory: i32,
        path: u64,
        user: u32,
        group: u32,
        flags: u32,
    ) -> Result<FsMutationPlan, AbiError> {
        if flags & !(AT_SYMLINK_NOFOLLOW | AT_EMPTY_PATH) != 0 {
            return Err(AbiError::Invalid);
        }
        Ok(FsMutationPlan::Chown {
            target: self.path_operand(
                directory,
                path,
                flags & AT_EMPTY_PATH != 0,
                flags & AT_SYMLINK_NOFOLLOW != 0,
            )?,
            user: (user != u32::MAX).then_some(user),
            group: (group != u32::MAX).then_some(group),
        })
    }

    pub fn utimensat(&self, directory: i32, path: u64, times: u64, flags: u32) -> Result<FsMutationPlan, AbiError> {
        if flags & !AT_SYMLINK_NOFOLLOW != 0 {
            return Err(AbiError::Invalid);
        }
        let target = if path == 0 {
            PathOperand {
                directory: OpenDirectory::from_raw(directory as i64 as u64),
                path: GuestPathBytes::new(b"").map_err(|_| AbiError::Invalid)?,
                allow_empty: true,
                nofollow: false,
            }
        } else {
            self.path_operand(directory, path, false, flags != 0)?
        };
        let times = if times == 0 {
            [TimestampChange::Now; 2]
        } else {
            let bytes = self.marshaller.copy_struct_from::<32>(times)?;
            [Self::timestamp(&bytes[..16])?, Self::timestamp(&bytes[16..])?]
        };
        Ok(FsMutationPlan::SetTimes { target, times })
    }

    pub fn utime(&self, path: u64, times: u64) -> Result<FsMutationPlan, AbiError> {
        let times = if times == 0 {
            [TimestampChange::Now; 2]
        } else {
            let bytes = self.marshaller.copy_struct_from::<16>(times)?;
            [
                TimestampChange::Value {
                    seconds: i64::from_le_bytes(bytes[..8].try_into().unwrap()),
                    nanoseconds: 0,
                },
                TimestampChange::Value {
                    seconds: i64::from_le_bytes(bytes[8..].try_into().unwrap()),
                    nanoseconds: 0,
                },
            ]
        };
        Ok(FsMutationPlan::SetTimes {
            target: self.path_operand(-100, path, false, false)?,
            times,
        })
    }

    pub fn utimes(&self, directory: i32, path: u64, times: u64) -> Result<FsMutationPlan, AbiError> {
        self.legacy_timeval_plan(directory, path, times, false)
    }

    pub fn futimesat(&self, directory: i32, path: u64, times: u64) -> Result<FsMutationPlan, AbiError> {
        self.legacy_timeval_plan(directory, path, times, true)
    }

    fn legacy_timeval_plan(
        &self,
        directory: i32,
        path: u64,
        times: u64,
        descriptor_when_null: bool,
    ) -> Result<FsMutationPlan, AbiError> {
        let times = if times == 0 {
            [TimestampChange::Now; 2]
        } else {
            let bytes = self.marshaller.copy_struct_from::<32>(times)?;
            [Self::timeval(&bytes[..16])?, Self::timeval(&bytes[16..])?]
        };
        let target = if descriptor_when_null && path == 0 {
            PathOperand {
                directory: OpenDirectory::from_raw(directory as i64 as u64),
                path: GuestPathBytes::new(b"").map_err(|_| AbiError::Invalid)?,
                allow_empty: true,
                nofollow: false,
            }
        } else {
            self.path_operand(directory, path, false, false)?
        };
        Ok(FsMutationPlan::SetTimes { target, times })
    }

    fn timeval(bytes: &[u8]) -> Result<TimestampChange, AbiError> {
        let seconds = i64::from_le_bytes(bytes[..8].try_into().unwrap());
        let microseconds = i64::from_le_bytes(bytes[8..16].try_into().unwrap());
        let nanoseconds = microseconds.checked_mul(1_000).ok_or(AbiError::Invalid)?;
        if !(0..=999_999_999).contains(&nanoseconds) {
            return Err(AbiError::Invalid);
        }
        Ok(TimestampChange::Value {
            seconds,
            nanoseconds: nanoseconds as u32,
        })
    }

    fn timestamp(bytes: &[u8]) -> Result<TimestampChange, AbiError> {
        let seconds = i64::from_le_bytes(bytes[..8].try_into().unwrap());
        let nanoseconds = i64::from_le_bytes(bytes[8..16].try_into().unwrap());
        match nanoseconds {
            UTIME_NOW => Ok(TimestampChange::Now),
            UTIME_OMIT => Ok(TimestampChange::Omit),
            0..=999_999_999 => Ok(TimestampChange::Value {
                seconds,
                nanoseconds: nanoseconds as u32,
            }),
            _ => Err(AbiError::Invalid),
        }
    }
}
