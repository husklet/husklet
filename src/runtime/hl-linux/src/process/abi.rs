use hl_isa::GuestArchitecture;
use hl_task::{Limit, Resource};

use super::copyout::{ResourceUsage, StagedProcessCopyout};

use crate::{Errno, GuestAccess, GuestMarshaller, GuestMemory, MarshalError};

const CLONE_ARGS_MINIMUM: usize = 64;
const CLONE_ARGS_CURRENT: usize = 88;
const CLONE_ARGS_MAXIMUM: usize = 4096;
const EXEC_VECTOR_MAXIMUM: usize = 4096;
const EXEC_BYTES_MAXIMUM: usize = 2 * 1024 * 1024;
const EXEC_STRING_MAXIMUM: usize = 32 * 4096;
const GROUP_MAXIMUM: usize = 65536;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Fault,
    Invalid,
    NameTooLong,
    TooBig,
    Overflow,
}

impl Error {
    #[must_use]
    pub const fn errno(self) -> Errno {
        match self {
            Self::Fault => Errno::EFAULT,
            Self::Invalid => Errno::EINVAL,
            Self::NameTooLong => Errno::ENAMETOOLONG,
            Self::TooBig => Errno::E2BIG,
            Self::Overflow => Errno::EOVERFLOW,
        }
    }
}

impl From<MarshalError> for Error {
    fn from(value: MarshalError) -> Self {
        match value {
            MarshalError::Fault(_) => Self::Fault,
            MarshalError::Invalid => Self::Invalid,
            MarshalError::TooBig => Self::TooBig,
            MarshalError::Overflow => Self::Overflow,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClonePlan {
    pub flags: u64,
    pub stack: u64,
    pub stack_size: u64,
    pub parent_tid: u64,
    pub child_tid: u64,
    pub tls: u64,
    pub exit_signal: u32,
    pub pidfd: u64,
    pub set_tid: u64,
    pub set_tid_count: u64,
    pub cgroup: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecPlan {
    pub directory: Option<i32>,
    pub path: Vec<u8>,
    pub arguments: Vec<Vec<u8>>,
    pub environment: Vec<Vec<u8>>,
    pub flags: u32,
}

impl ExecPlan {
    /// Returns the Linux task name derived from the path passed to `execve`.
    ///
    /// Linux retains the final path component before interpreter rewriting and
    /// limits the visible name to 15 bytes plus its terminating NUL.
    #[must_use]
    pub fn comm(&self) -> [u8; 16] {
        Self::comm_from_path(&self.path)
    }

    /// Derives a Linux task name when launch composition has not yet built an
    /// `ExecPlan` but applies the same exec-path identity rule.
    #[must_use]
    pub fn comm_from_path(path: &[u8]) -> [u8; 16] {
        let leaf = path.rsplit(|byte| *byte == b'/').next().unwrap_or(path);
        let mut name = [0; 16];
        let count = leaf.len().min(15);
        name[..count].copy_from_slice(&leaf[..count]);
        name
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaitKind {
    Any,
    Process(u32),
    ProcessGroup(u32),
    SameProcessGroup,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WaitPlan {
    pub kind: WaitKind,
    pub options: u32,
    pub status: u64,
    pub information: u64,
    pub usage: u64,
    pub keep_waitable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IdentityChange {
    User {
        real: Option<u32>,
        effective: Option<u32>,
        saved: Option<u32>,
    },
    Group {
        real: Option<u32>,
        effective: Option<u32>,
        saved: Option<u32>,
    },
    SupplementaryGroups(Vec<u32>),
}

pub struct Abi<'a, M: GuestMemory + ?Sized> {
    pub(crate) marshaller: GuestMarshaller<'a, M>,
}

impl<'a, M: GuestMemory + ?Sized> Abi<'a, M> {
    #[must_use]
    pub const fn new(memory: &'a M, architecture: GuestArchitecture) -> Self {
        Self {
            marshaller: GuestMarshaller::new(memory, architecture),
        }
    }

    #[must_use]
    pub const fn fork(&self) -> ClonePlan {
        Self::plain_clone(0)
    }

    #[must_use]
    pub const fn vfork(&self) -> ClonePlan {
        Self::plain_clone(0x0000_4100)
    }

    #[must_use]
    pub const fn exit(&self, status: u64) -> u8 {
        status as u8
    }

    pub fn clone_legacy(
        &self,
        flags: u64,
        stack: u64,
        parent_tid: u64,
        architecture_fourth: u64,
        architecture_fifth: u64,
    ) -> Result<ClonePlan, Error> {
        let (child_tid, tls) = match self.marshaller.architecture() {
            GuestArchitecture::X86_64 => (architecture_fourth, architecture_fifth),
            GuestArchitecture::Aarch64 => (architecture_fifth, architecture_fourth),
        };
        let exit_signal = (flags & 0xff) as u32;
        Self::validate_clone(flags, exit_signal)?;
        Ok(ClonePlan {
            flags: flags & !0xff,
            stack,
            stack_size: 0,
            parent_tid,
            child_tid,
            tls,
            exit_signal,
            pidfd: parent_tid,
            set_tid: 0,
            set_tid_count: 0,
            cgroup: 0,
        })
    }

    pub fn clone3(&self, address: u64, size: usize) -> Result<ClonePlan, Error> {
        if !(CLONE_ARGS_MINIMUM..=CLONE_ARGS_MAXIMUM).contains(&size) {
            return Err(Error::Invalid);
        }
        let copied = size.min(CLONE_ARGS_CURRENT);
        let mut bytes = vec![0; copied];
        let progress = self.marshaller.copy_from(address, &mut bytes);
        if progress.fault.is_some() {
            return Err(Error::Fault);
        }
        if size > CLONE_ARGS_CURRENT {
            let extension = size - CLONE_ARGS_CURRENT;
            let mut bytes = vec![0; extension];
            let progress = self
                .marshaller
                .copy_from(address + CLONE_ARGS_CURRENT as u64, &mut bytes);
            if progress.fault.is_some() {
                return Err(Error::Fault);
            }
            if Self::nonzero_extension(&bytes) {
                return Err(Error::TooBig);
            }
        }
        let flags = Self::optional_word(&bytes, 0);
        let exit_signal = Self::optional_word(&bytes, 32);
        if exit_signal > 64 {
            return Err(Error::Invalid);
        }
        Self::validate_clone(flags, exit_signal as u32)?;
        Ok(ClonePlan {
            flags,
            pidfd: Self::optional_word(&bytes, 8),
            child_tid: Self::optional_word(&bytes, 16),
            parent_tid: Self::optional_word(&bytes, 24),
            exit_signal: exit_signal as u32,
            stack: Self::optional_word(&bytes, 40),
            stack_size: Self::optional_word(&bytes, 48),
            tls: Self::optional_word(&bytes, 56),
            set_tid: Self::optional_word(&bytes, 64),
            set_tid_count: Self::optional_word(&bytes, 72),
            cgroup: Self::optional_word(&bytes, 80),
        })
    }

    pub fn execve(&self, path: u64, argv: u64, envp: u64) -> Result<ExecPlan, Error> {
        let plan = self.exec_path(None, path, 0)?;
        self.exec_vectors(plan, argv, envp)
    }

    pub fn execveat(&self, directory: i32, path: u64, argv: u64, envp: u64, flags: u32) -> Result<ExecPlan, Error> {
        if flags & !0x1100 != 0 {
            return Err(Error::Invalid);
        }
        let plan = self.exec_path(Some(directory), path, flags)?;
        self.exec_vectors(plan, argv, envp)
    }

    pub fn wait4(&self, pid: i32, status: u64, options: u32, usage: u64) -> Result<WaitPlan, Error> {
        if options & !0xc000_000b != 0 {
            return Err(Error::Invalid);
        }
        Ok(WaitPlan {
            kind: Self::wait_pid(pid),
            options,
            status,
            information: 0,
            usage,
            keep_waitable: false,
        })
    }

    pub fn waitid(&self, id_type: u32, id: u32, information: u64, options: u32, usage: u64) -> Result<WaitPlan, Error> {
        if information == 0 || options & !0x1100_000f != 0 || options & 0x0e == 0 {
            return Err(Error::Invalid);
        }
        let kind = match id_type {
            0 => WaitKind::Any,
            1 => WaitKind::Process(id),
            2 if id == 0 => WaitKind::SameProcessGroup,
            2 => WaitKind::ProcessGroup(id),
            3 => WaitKind::Process(id),
            _ => return Err(Error::Invalid),
        };
        Ok(WaitPlan {
            kind,
            options,
            status: 0,
            information,
            usage,
            keep_waitable: options & 0x0100_0000 != 0,
        })
    }

    pub fn groups(&self, address: u64, count: usize) -> Result<Vec<u32>, Error> {
        if count > GROUP_MAXIMUM {
            return Err(Error::Invalid);
        }
        let length = count.checked_mul(4).ok_or(Error::Overflow)?;
        let mut bytes = vec![0; length];
        if self.marshaller.copy_from(address, &mut bytes).fault.is_some() {
            return Err(Error::Fault);
        }
        Ok(bytes
            .chunks_exact(4)
            .map(|chunk| u32::from_le_bytes(chunk.try_into().expect("group")))
            .collect())
    }

    #[must_use]
    pub fn identity_user(&self, real: u32, effective: u32, saved: u32) -> IdentityChange {
        IdentityChange::User {
            real: (real != u32::MAX).then_some(real),
            effective: (effective != u32::MAX).then_some(effective),
            saved: (saved != u32::MAX).then_some(saved),
        }
    }

    #[must_use]
    pub fn identity_group(&self, real: u32, effective: u32, saved: u32) -> IdentityChange {
        IdentityChange::Group {
            real: (real != u32::MAX).then_some(real),
            effective: (effective != u32::MAX).then_some(effective),
            saved: (saved != u32::MAX).then_some(saved),
        }
    }

    pub fn resource(&self, raw: u32) -> Result<Resource, Error> {
        match raw {
            0 => Ok(Resource::CpuTime),
            1 => Ok(Resource::FileSize),
            2 => Ok(Resource::Data),
            3 => Ok(Resource::Stack),
            4 => Ok(Resource::Core),
            5 => Ok(Resource::ResidentSet),
            6 => Ok(Resource::Processes),
            7 => Ok(Resource::OpenFiles),
            8 => Ok(Resource::LockedMemory),
            9 => Ok(Resource::AddressSpace),
            10 => Ok(Resource::Locks),
            11 => Ok(Resource::PendingSignals),
            12 => Ok(Resource::MessageQueue),
            13 => Ok(Resource::Nice),
            14 => Ok(Resource::RealtimePriority),
            15 => Ok(Resource::RealtimeTime),
            _ => Err(Error::Invalid),
        }
    }

    pub fn limit(&self, address: u64) -> Result<Limit, Error> {
        let bytes = self.marshaller.copy_struct_from::<16>(address)?;
        Limit::new(Self::word(&bytes, 0), Self::word(&bytes, 8)).map_err(|_| Error::Invalid)
    }

    pub fn stage_limit(&self, address: u64, limit: Limit) -> Result<StagedProcessCopyout, Error> {
        let mut bytes = Vec::with_capacity(16);
        bytes.extend_from_slice(&limit.soft.to_le_bytes());
        bytes.extend_from_slice(&limit.hard.to_le_bytes());
        self.stage(address, bytes)
    }

    #[must_use]
    pub fn defer_limit(&self, address: u64, limit: Limit) -> StagedProcessCopyout {
        let mut bytes = Vec::with_capacity(16);
        bytes.extend_from_slice(&limit.soft.to_le_bytes());
        bytes.extend_from_slice(&limit.hard.to_le_bytes());
        StagedProcessCopyout {
            destination: address,
            bytes,
        }
    }

    pub fn stage_id(&self, address: u64, identifier: u32) -> Result<StagedProcessCopyout, Error> {
        self.stage(address, identifier.to_le_bytes().to_vec())
    }

    pub fn stage_groups(&self, address: u64, groups: &[u32]) -> Result<StagedProcessCopyout, Error> {
        if groups.len() > GROUP_MAXIMUM {
            return Err(Error::Invalid);
        }
        let bytes = groups.iter().flat_map(|group| group.to_le_bytes()).collect();
        self.stage(address, bytes)
    }

    pub fn stage_wait_status(&self, address: u64, status: u32) -> Result<StagedProcessCopyout, Error> {
        self.stage(address, status.to_le_bytes().to_vec())
    }

    pub fn stage_usage(&self, address: u64, usage: ResourceUsage) -> Result<StagedProcessCopyout, Error> {
        let values = [
            usage.user_seconds,
            usage.user_microseconds,
            usage.system_seconds,
            usage.system_microseconds,
            usage.maximum_resident_set,
            0,
            usage.minor_faults,
            usage.major_faults,
            0,
            0,
            0,
            0,
            0,
            0,
            usage.voluntary_switches,
            usage.involuntary_switches,
        ];
        let bytes = values.iter().flat_map(|value| value.to_le_bytes()).collect();
        self.stage(address, bytes)
    }

    pub fn exec_path(&self, directory: Option<i32>, path: u64, flags: u32) -> Result<ExecPlan, Error> {
        if flags & !0x1100 != 0 {
            return Err(Error::Invalid);
        }
        let path = self.marshaller.c_string(path, 4096).map_err(|error| match error {
            MarshalError::TooBig => Error::NameTooLong,
            other => Error::from(other),
        })?;
        Ok(ExecPlan {
            directory,
            path,
            arguments: Vec::new(),
            environment: Vec::new(),
            flags,
        })
    }

    pub fn exec_vectors(&self, mut plan: ExecPlan, argv: u64, envp: u64) -> Result<ExecPlan, Error> {
        let arguments = self.strings(argv)?;
        let environment = self.strings(envp)?;
        let total = plan.path.len()
            + arguments.iter().map(Vec::len).sum::<usize>()
            + environment.iter().map(Vec::len).sum::<usize>();
        if total > EXEC_BYTES_MAXIMUM {
            return Err(Error::TooBig);
        }
        plan.arguments = arguments;
        plan.environment = environment;
        Ok(plan)
    }

    fn strings(&self, address: u64) -> Result<Vec<Vec<u8>>, Error> {
        if address == 0 {
            return Ok(Vec::new());
        }
        let pointers = self.marshaller.pointer_vector(address, EXEC_VECTOR_MAXIMUM)?;
        pointers
            .into_iter()
            .map(|pointer| {
                self.marshaller
                    .c_string(pointer, EXEC_STRING_MAXIMUM)
                    .map_err(Into::into)
            })
            .collect()
    }

    fn stage(&self, destination: u64, bytes: Vec<u8>) -> Result<StagedProcessCopyout, Error> {
        let available = self.marshaller.probe(destination, bytes.len(), GuestAccess::Write)?;
        if available != bytes.len() {
            return Err(Error::Fault);
        }
        Ok(StagedProcessCopyout { destination, bytes })
    }

    const fn plain_clone(flags: u64) -> ClonePlan {
        ClonePlan {
            flags,
            stack: 0,
            stack_size: 0,
            parent_tid: 0,
            child_tid: 0,
            tls: 0,
            exit_signal: 17,
            pidfd: 0,
            set_tid: 0,
            set_tid_count: 0,
            cgroup: 0,
        }
    }

    fn validate_clone(flags: u64, exit_signal: u32) -> Result<(), Error> {
        if exit_signal > 64 || flags & 0xffff_0000_0000_0000 != 0 {
            Err(Error::Invalid)
        } else {
            Ok(())
        }
    }

    const fn wait_pid(pid: i32) -> WaitKind {
        if pid == -1 {
            WaitKind::Any
        } else if pid == 0 {
            WaitKind::SameProcessGroup
        } else if pid < -1 {
            WaitKind::ProcessGroup(pid.unsigned_abs())
        } else {
            WaitKind::Process(pid as u32)
        }
    }

    fn word(bytes: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("word"))
    }

    fn optional_word(bytes: &[u8], offset: usize) -> u64 {
        bytes
            .get(offset..offset + 8)
            .map_or(0, |part| u64::from_le_bytes(part.try_into().expect("word")))
    }

    fn nonzero_extension(bytes: &[u8]) -> bool {
        bytes.iter().any(|byte| *byte != 0)
    }
}
