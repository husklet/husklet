use std::fmt::Write as _;

mod memory;
mod network;
mod node;
mod system;
pub use memory::*;
pub use network::*;
pub use node::*;
pub use system::*;

/// Generation-qualified process identity resolved from a guest-visible PID.
///
/// Numeric procfs paths are lookup keys only. Consumers that retain a target
/// across operations must carry this complete identity and explicitly validate
/// it rather than resolving the number again after PID reuse.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProcessIdentity {
    slot: u32,
    generation: u16,
}

impl ProcessIdentity {
    #[must_use]
    pub const fn new(slot: u32, generation: u16) -> Option<Self> {
        if generation == 0 {
            None
        } else {
            Some(Self { slot, generation })
        }
    }

    #[must_use]
    pub const fn slot(self) -> u32 {
        self.slot
    }

    #[must_use]
    pub const fn generation(self) -> u16 {
        self.generation
    }
}

/// Generation-qualified thread identity resolved from a guest-visible TID.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ThreadIdentity {
    slot: u32,
    generation: u16,
}

impl ThreadIdentity {
    #[must_use]
    pub const fn new(slot: u32, generation: u16) -> Option<Self> {
        if generation == 0 {
            None
        } else {
            Some(Self { slot, generation })
        }
    }

    #[must_use]
    pub const fn slot(self) -> u32 {
        self.slot
    }

    #[must_use]
    pub const fn generation(self) -> u16 {
        self.generation
    }
}

/// Linux resource represented in `/proc/<pid>/limits`.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LimitResource {
    CpuTime,
    FileSize,
    Data,
    Stack,
    Core,
    ResidentSet,
    Processes,
    OpenFiles,
    LockedMemory,
    AddressSpace,
    Locks,
    PendingSignals,
    MessageQueue,
    Nice,
    RealtimePriority,
    RealtimeTime,
}

/// One task-owned resource-limit value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LimitView {
    pub resource: LimitResource,
    pub soft: u64,
    pub hard: u64,
}

/// Guest-visible task lifecycle used by procfs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessState {
    Running,
    Sleeping,
    Stopped,
    Exiting,
    Zombie,
}

impl ProcessState {
    const fn code(self) -> &'static str {
        match self {
            Self::Running => "R (running)",
            Self::Sleeping => "S (sleeping)",
            Self::Stopped => "T (stopped)",
            Self::Exiting => "X (dead)",
            Self::Zombie => "Z (zombie)",
        }
    }
}

/// Value-only task projection consumed by procfs rendering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessView {
    pub process: u32,
    pub parent: u32,
    pub name: [u8; 16],
    pub state: ProcessState,
    pub threads: usize,
    pub umask: Option<u32>,
    pub real_user: u32,
    pub effective_user: u32,
    pub saved_user: u32,
    pub filesystem_user: u32,
    pub real_group: u32,
    pub effective_group: u32,
    pub saved_group: u32,
    pub filesystem_group: u32,
    pub groups: Vec<u32>,
    pub inheritable: u64,
    pub permitted: u64,
    pub effective: u64,
    pub bounding: u64,
    pub ambient: u64,
    pub no_new_privileges: bool,
    pub seccomp_mode: u8,
    pub seccomp_filters: usize,
    pub pending_signals: u64,
    pub blocked_signals: u64,
    pub ignored_signals: u64,
    pub caught_signals: u64,
    pub limits: Vec<LimitView>,
    pub allowed_mask: String,
    pub allowed_list: String,
    pub memory: Option<MemoryView>,
}
/// One descriptor identity captured for procfs at open time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DescriptorView {
    pub number: i32,
    pub offset: u64,
    pub flags: u32,
    pub mount: Option<u64>,
    pub inode: u64,
    pub target: Option<Vec<u8>>,
}
impl DescriptorView {
    pub(in crate::procfs) fn info(&self) -> Vec<u8> {
        let mut output = format!("pos:\t{}\nflags:\t0{:o}\n", self.offset, self.flags);
        if let Some(mount) = self.mount {
            let _ = writeln!(output, "mnt_id:\t{mount}");
        }
        let _ = writeln!(output, "ino:\t{}", self.inode);
        output.into_bytes()
    }
}
impl ProcessView {
    pub(in crate::procfs) fn comm(&self) -> Vec<u8> {
        let mut name = self.name.split(|byte| *byte == 0).next().unwrap_or(&[]).to_vec();
        name.push(b'\n');
        name
    }

    pub(in crate::procfs) fn status(&self) -> Vec<u8> {
        let name = self.name.split(|byte| *byte == 0).next().unwrap_or(&[]);
        let name = String::from_utf8_lossy(name);
        let groups = self.groups.iter().map(u32::to_string).collect::<Vec<_>>().join(" ");
        let mut output = format!("Name:\t{name}\n");
        if let Some(umask) = self.umask {
            let _ = writeln!(output, "Umask:\t{umask:04o}");
        }
        let _ = write!(
            output,
            "State:\t{}\nTgid:\t{}\nNgid:\t0\nPid:\t{}\nPPid:\t{}\nTracerPid:\t0\n\
             Uid:\t{}\t{}\t{}\t{}\nGid:\t{}\t{}\t{}\t{}\nGroups:\t{}\nThreads:\t{}\n\
             SigPnd:\t{:016x}\nSigBlk:\t{:016x}\nSigIgn:\t{:016x}\nSigCgt:\t{:016x}\n\
             CapInh:\t{:016x}\nCapPrm:\t{:016x}\nCapEff:\t{:016x}\nCapBnd:\t{:016x}\n\
             CapAmb:\t{:016x}\nNoNewPrivs:\t{}\nSeccomp:\t{}\nSeccomp_filters:\t{}\n",
            self.state.code(),
            self.process,
            self.process,
            self.parent,
            self.real_user,
            self.effective_user,
            self.saved_user,
            self.filesystem_user,
            self.real_group,
            self.effective_group,
            self.saved_group,
            self.filesystem_group,
            groups,
            self.threads,
            self.pending_signals,
            self.blocked_signals,
            self.ignored_signals,
            self.caught_signals,
            self.inheritable,
            self.permitted,
            self.effective,
            self.bounding,
            self.ambient,
            u8::from(self.no_new_privileges),
            self.seccomp_mode,
            self.seccomp_filters,
        );
        let _ = write!(
            output,
            "Cpus_allowed:\t{}\nCpus_allowed_list:\t{}\n",
            self.allowed_mask, self.allowed_list,
        );
        if let Some(memory) = self.memory {
            let page_kb = memory.page_bytes / 1024;
            let size = memory.total_pages.saturating_mul(page_kb);
            let resident = memory.resident_pages.saturating_mul(page_kb);
            let _ = writeln!(output, "VmSize:\t{size} kB\nVmRSS:\t{resident} kB");
        }
        output.into_bytes()
    }

    pub(in crate::procfs) fn limits(&self) -> Vec<u8> {
        let mut output = format!(
            "{:<25} {:<20} {:<20} {:<10}\n",
            "Limit", "Soft Limit", "Hard Limit", "Units",
        );
        let mut limits = self.limits.clone();
        limits.sort_by_key(|limit| limit.resource);
        for limit in limits {
            let (name, units) = limit.resource.label();
            let _ = writeln!(
                output,
                "{name:<25} {:<20} {:<20} {units:<10}",
                limit.soft_text(),
                limit.hard_text(),
            );
        }
        output.into_bytes()
    }
}

impl LimitView {
    fn soft_text(self) -> String {
        Self::text(self.soft)
    }

    fn hard_text(self) -> String {
        Self::text(self.hard)
    }

    fn text(value: u64) -> String {
        if value == u64::MAX {
            String::from("unlimited")
        } else {
            value.to_string()
        }
    }
}

impl LimitResource {
    const fn label(self) -> (&'static str, &'static str) {
        match self {
            Self::CpuTime => ("Max cpu time", "seconds"),
            Self::FileSize => ("Max file size", "bytes"),
            Self::Data => ("Max data size", "bytes"),
            Self::Stack => ("Max stack size", "bytes"),
            Self::Core => ("Max core file size", "bytes"),
            Self::ResidentSet => ("Max resident set", "bytes"),
            Self::Processes => ("Max processes", "processes"),
            Self::OpenFiles => ("Max open files", "files"),
            Self::LockedMemory => ("Max locked memory", "bytes"),
            Self::AddressSpace => ("Max address space", "bytes"),
            Self::Locks => ("Max file locks", "locks"),
            Self::PendingSignals => ("Max pending signals", "signals"),
            Self::MessageQueue => ("Max msgqueue size", "bytes"),
            Self::Nice => ("Max nice priority", ""),
            Self::RealtimePriority => ("Max realtime priority", ""),
            Self::RealtimeTime => ("Max realtime timeout", "us"),
        }
    }
}
