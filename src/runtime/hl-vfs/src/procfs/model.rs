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

/// Instance-owned CPU topology projected through procfs and sysfs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CpuView {
    online: usize,
    range: String,
    model: CpuModel,
    ticks: Vec<CpuTicks>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CpuTicks {
    pub user: u64,
    pub nice: u64,
    pub system: u64,
    pub idle: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CpuModel {
    Aarch64 {
        hardware: u64,
        hardware_second: u64,
    },
    X86_64 {
        vendor: String,
        family: u32,
        model: u32,
        stepping: u32,
        name: String,
        flags: Vec<&'static str>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UtsView {
    pub namespace: u64,
    pub hostname: Vec<u8>,
    pub domainname: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SystemView {
    pub uptime_seconds: u64,
    pub process_creations: u64,
    pub total_memory: u64,
    pub free_memory: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryView {
    pub page_bytes: u64,
    pub total_pages: u64,
    pub resident_pages: u64,
    pub shared_pages: u64,
    pub text_pages: u64,
    pub data_pages: u64,
}

/// One immutable, generation-qualified view of a guest address space.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AddressSpaceView {
    pub generation: u64,
    pub page_bytes: u64,
    pub regions: Vec<MemoryRegionView>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryRegionView {
    pub start: u64,
    pub end: u64,
    pub protection: u8,
    pub shared: bool,
    pub backing_offset: u64,
    pub device: u64,
    pub inode: u64,
    pub path: Option<Vec<u8>>,
    pub label: Option<MemoryRegionLabel>,
    pub resident_pages: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryRegionLabel {
    Heap,
    Stack,
    StackGuard,
}

impl AddressSpaceView {
    pub fn new(generation: u64, page_bytes: u64, regions: Vec<MemoryRegionView>) -> Option<Self> {
        if generation == 0 || !page_bytes.is_power_of_two() {
            return None;
        }
        let valid = regions.iter().enumerate().all(|(index, region)| {
            let pages = region
                .end
                .checked_sub(region.start)
                .map(|bytes| bytes.div_ceil(page_bytes));
            region.start < region.end
                && region.protection & !7 == 0
                && pages.is_some_and(|pages| region.resident_pages <= pages)
                && region.path.as_ref().is_none_or(|path| !path.contains(&0))
                && index
                    .checked_sub(1)
                    .is_none_or(|previous| regions[previous].end <= region.start)
        });
        valid.then_some(Self {
            generation,
            page_bytes,
            regions,
        })
    }

    #[must_use]
    pub fn maps(&self, smaps: bool) -> Vec<u8> {
        let mut output = Vec::new();
        for region in &self.regions {
            let read = if region.protection & 1 != 0 { 'r' } else { '-' };
            let write = if region.protection & 2 != 0 { 'w' } else { '-' };
            let execute = if region.protection & 4 != 0 { 'x' } else { '-' };
            let sharing = if region.shared { 's' } else { 'p' };
            let major = (region.device >> 8) & 0xff;
            let minor = region.device & 0xff;
            output.extend_from_slice(
                format!(
                    "{:08x}-{:08x} {read}{write}{execute}{sharing} {:08x} {major:02x}:{minor:02x} {}",
                    region.start, region.end, region.backing_offset, region.inode,
                )
                .as_bytes(),
            );
            if let Some(path) = &region.path {
                output.push(b' ');
                output.extend_from_slice(path);
            } else if let Some(label) = region.label {
                output.extend_from_slice(match label {
                    MemoryRegionLabel::Heap => b" [heap]".as_slice(),
                    MemoryRegionLabel::Stack => b" [stack]",
                    MemoryRegionLabel::StackGuard => b"",
                });
            }
            output.push(b'\n');
            if smaps {
                let pages = region.end.saturating_sub(region.start).div_ceil(self.page_bytes);
                let size = pages.saturating_mul(self.page_bytes) / 1024;
                let resident = region.resident_pages.saturating_mul(self.page_bytes) / 1024;
                output.extend_from_slice(
                    format!(
                        "Size:{size:19} kB\nKernelPageSize:{:9} kB\nMMUPageSize:{:12} kB\n\
                     Rss:{resident:20} kB\nPss:{resident:20} kB\nShared_Clean:{:11} kB\n\
                     Shared_Dirty:{:11} kB\nPrivate_Clean:{:10} kB\nPrivate_Dirty:{:10} kB\n\
                     Referenced:{resident:13} kB\nAnonymous:{:14} kB\nSwap:{:19} kB\nLocked:{:17} kB\n\
                     VmFlags:{}{}{} mr mw me ac \n",
                        self.page_bytes / 1024,
                        self.page_bytes / 1024,
                        0,
                        if region.shared { resident } else { 0 },
                        if region.inode != 0 { resident } else { 0 },
                        if !region.shared && region.inode == 0 {
                            resident
                        } else {
                            0
                        },
                        if region.inode == 0 { resident } else { 0 },
                        0,
                        0,
                        if read == 'r' { " rd" } else { "" },
                        if write == 'w' { " wr" } else { "" },
                        if execute == 'x' { " ex" } else { "" },
                    )
                    .as_bytes(),
                );
            }
        }
        output
    }

    #[must_use]
    pub fn numa(&self) -> Vec<u8> {
        let mut output = Vec::new();
        for region in &self.regions {
            output.extend_from_slice(format!("{:08x} default", region.start).as_bytes());
            match region.label {
                Some(MemoryRegionLabel::Heap) => output.extend_from_slice(b" heap"),
                Some(MemoryRegionLabel::Stack) => output.extend_from_slice(b" stack"),
                _ if region.inode != 0 => {
                    if let Some(path) = &region.path {
                        output.extend_from_slice(b" file=");
                        for byte in path {
                            if *byte == b' ' {
                                output.extend_from_slice(br"\040");
                            } else {
                                output.push(*byte);
                            }
                        }
                    }
                }
                _ => {}
            }
            let pages = region.end.saturating_sub(region.start) / self.page_bytes;
            if region.protection != 0 && pages != 0 {
                if region.inode == 0 {
                    output.extend_from_slice(format!(" anon={pages} dirty={pages}").as_bytes());
                } else {
                    output.extend_from_slice(format!(" mapped={pages}").as_bytes());
                }
                output.extend_from_slice(format!(" active=0 N0={pages} kernelpagesize_kB=4").as_bytes());
            }
            output.push(b'\n');
        }
        output
    }

    #[must_use]
    pub fn rollup(&self) -> Vec<u8> {
        let low = self.regions.first().map_or(0, |region| region.start);
        let high = self.regions.last().map_or(0, |region| region.end);
        let mut rss = 0_u64;
        let mut clean = 0_u64;
        let mut dirty = 0_u64;
        for region in &self.regions {
            if region.protection == 0 {
                continue;
            }
            let bytes = region.resident_pages.saturating_mul(self.page_bytes) / 1024;
            rss = rss.saturating_add(bytes);
            if region.inode == 0 {
                dirty = dirty.saturating_add(bytes);
            } else {
                clean = clean.saturating_add(bytes);
            }
        }
        format!(
            "{low:08x}-{high:08x} ---p 00000000 00:00 0 [rollup]\n\
             Rss:{rss:20} kB\nPss:{rss:20} kB\nPss_Dirty:{dirty:14} kB\n\
             Pss_Anon:{dirty:15} kB\nPss_File:{clean:15} kB\nPss_Shmem:{:14} kB\n\
             Shared_Clean:{:11} kB\nShared_Dirty:{dirty:11} kB\nPrivate_Clean:{clean:10} kB\n\
             Private_Dirty:{:10} kB\nReferenced:{rss:13} kB\nAnonymous:{dirty:14} kB\n\
             Swap:{:19} kB\nLocked:{:17} kB\n",
            0, 0, 0, 0, 0,
        )
        .into_bytes()
    }

    #[must_use]
    pub fn memory(&self) -> MemoryView {
        let mut view = MemoryView {
            page_bytes: self.page_bytes,
            total_pages: 0,
            resident_pages: 0,
            shared_pages: 0,
            text_pages: 0,
            data_pages: 0,
        };
        for region in &self.regions {
            let pages = region.end.saturating_sub(region.start).div_ceil(self.page_bytes);
            view.total_pages = view.total_pages.saturating_add(pages);
            view.resident_pages = view.resident_pages.saturating_add(region.resident_pages.min(pages));
            if region.shared {
                view.shared_pages = view.shared_pages.saturating_add(region.resident_pages.min(pages));
            }
            if region.protection & 4 != 0 {
                view.text_pages = view.text_pages.saturating_add(pages);
            } else if region.protection & 2 != 0 {
                view.data_pages = view.data_pages.saturating_add(pages);
            }
        }
        view
    }
}

impl MemoryView {
    pub(super) fn statm(self) -> Vec<u8> {
        format!(
            "{} {} {} {} 0 {} 0\n",
            self.total_pages, self.resident_pages, self.shared_pages, self.text_pages, self.data_pages
        )
        .into_bytes()
    }
}

impl SystemView {
    pub(super) fn meminfo(self) -> Vec<u8> {
        let total_bytes = self.total_memory.max(4096);
        let total = total_bytes / 1024;
        let free = self.free_memory.min(total_bytes) / 1024;
        format!(
            "MemTotal:       {total} kB\nMemFree:        {free} kB\nMemAvailable:   {free} kB\n\
             Buffers:        0 kB\nCached:         0 kB\nSwapCached:     0 kB\n\
             Active:         0 kB\nInactive:       0 kB\nDirty:          0 kB\nAnonPages:      0 kB\n\
             SwapTotal:      0 kB\nSwapFree:       0 kB\n"
        )
        .into_bytes()
    }

    pub(super) fn uptime(self) -> Vec<u8> {
        format!("{}.00 0.00\n", self.uptime_seconds).into_bytes()
    }
}

impl CpuView {
    pub fn new(online: usize, model: CpuModel) -> Option<Self> {
        if online == 0 || online > 64 {
            return None;
        }
        let range = if online == 1 {
            String::from("0")
        } else {
            format!("0-{}", online - 1)
        };
        Some(Self {
            online,
            range,
            model,
            ticks: Vec::new(),
        })
    }

    #[must_use]
    pub fn with_ticks(mut self, ticks: Vec<CpuTicks>) -> Self {
        self.ticks = ticks;
        self
    }

    #[must_use]
    pub const fn online(&self) -> usize {
        self.online
    }

    pub(super) fn mask(&self, bit: Option<usize>) -> String {
        let value = bit.map_or_else(
            || {
                if self.online == 64 {
                    u64::MAX
                } else {
                    (1_u64 << self.online) - 1
                }
            },
            |number| 1_u64 << number,
        );
        if self.online <= 32 {
            let width = self.online.div_ceil(4).max(1);
            return format!("{value:0width$x}");
        }
        let width = (self.online - 32).div_ceil(4).max(1);
        format!("{:0width$x},{:08x}", value >> 32, value as u32)
    }

    pub(super) fn version(&self) -> Vec<u8> {
        let architecture = match &self.model {
            CpuModel::Aarch64 { .. } => "aarch64",
            CpuModel::X86_64 { .. } => "x86_64",
        };
        format!("Linux version 6.1.0 (hl-engine) {architecture}\n").into_bytes()
    }

    pub(super) fn cpuinfo(&self) -> Vec<u8> {
        let mut output = String::new();
        for cpu in 0..self.online {
            match &self.model {
                CpuModel::Aarch64 { .. } => {
                    let features = self.model.capability_names();
                    output.push_str(&format!(
                        "processor\t: {cpu}\nBogoMIPS\t: 100.00\nFeatures\t: {features}\n\
                         CPU implementer\t: 0x61\nCPU architecture: 8\nCPU variant\t: 0x0\n\
                         CPU part\t: 0x000\nCPU revision\t: 0\n\n"
                    ));
                }
                CpuModel::X86_64 {
                    vendor,
                    family,
                    model,
                    stepping,
                    name,
                    flags,
                } => output.push_str(&format!(
                    "processor\t: {cpu}\nvendor_id\t: {vendor}\ncpu family\t: {family}\nmodel\t\t: {model}\n\
                     model name\t: {name}\nstepping\t: {stepping}\nfpu\t\t: yes\n\
                     fpu_exception\t: yes\nflags\t\t: {}\n\n",
                    flags.join(" ")
                )),
            }
        }
        output.into_bytes()
    }

    pub(super) fn stat(&self, system: SystemView) -> Vec<u8> {
        let aggregate = self
            .ticks
            .iter()
            .take(self.online)
            .fold(CpuTicks::default(), |sum, ticks| CpuTicks {
                user: sum.user.saturating_add(ticks.user),
                nice: sum.nice.saturating_add(ticks.nice),
                system: sum.system.saturating_add(ticks.system),
                idle: sum.idle.saturating_add(ticks.idle),
            });
        let mut output = format!(
            "cpu  {} {} {} {} 0 0 0 0 0 0\n",
            aggregate.user, aggregate.nice, aggregate.system, aggregate.idle,
        );
        for cpu in 0..self.online {
            let ticks = self.ticks.get(cpu).copied().unwrap_or_default();
            output.push_str(&format!(
                "cpu{cpu} {} {} {} {} 0 0 0 0 0 0\n",
                ticks.user, ticks.nice, ticks.system, ticks.idle,
            ));
        }
        let activity = system.uptime_seconds.saturating_mul(100).saturating_add(1);
        output.push_str(&format!(
            "intr {}\nctxt {}\nbtime 1\nprocesses {}\nprocs_running 1\nprocs_blocked 0\n",
            activity.saturating_mul(137),
            activity.saturating_mul(509),
            system.process_creations.saturating_add(256),
        ));
        output.into_bytes()
    }

    pub(super) fn range_bytes(&self) -> Vec<u8> {
        format!("{}\n", self.range).into_bytes()
    }
}

impl CpuModel {
    fn capability_names(&self) -> String {
        const FIRST: [&str; 32] = [
            "fp", "asimd", "evtstrm", "aes", "pmull", "sha1", "sha2", "crc32", "atomics", "fphp", "asimdhp", "cpuid",
            "asimdrdm", "jscvt", "fcma", "lrcpc", "dcpop", "sha3", "sm3", "sm4", "asimddp", "sha512", "sve",
            "asimdfhm", "dit", "uscat", "ilrcpc", "flagm", "ssbs", "sb", "paca", "pacg",
        ];
        const SECOND: [&str; 22] = [
            "dcpodp",
            "sve2",
            "sveaes",
            "svepmull",
            "svebitperm",
            "svesha3",
            "svesm4",
            "flagm2",
            "frint",
            "svei8mm",
            "svef32mm",
            "svef64mm",
            "svebf16",
            "i8mm",
            "bf16",
            "dgh",
            "rng",
            "bti",
            "mte",
            "ecv",
            "afp",
            "rpres",
        ];
        let Self::Aarch64 {
            hardware,
            hardware_second,
        } = self
        else {
            return String::new();
        };
        FIRST
            .iter()
            .enumerate()
            .filter(|(bit, _)| hardware & (1_u64 << bit) != 0)
            .chain(
                SECOND
                    .iter()
                    .enumerate()
                    .filter(|(bit, _)| hardware_second & (1_u64 << bit) != 0),
            )
            .map(|(_, name)| *name)
            .collect::<Vec<_>>()
            .join(" ")
    }
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

/// Linux-visible values for one AF_UNIX row in a coherent network snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnixSocketView {
    pub identity: u64,
    pub reference_count: u32,
    pub protocol: u32,
    pub flags: u32,
    pub socket_type: u16,
    pub state: u8,
    pub inode: u64,
    pub path: Option<Vec<u8>>,
}

/// One generation-qualified view of a task's network namespace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkView {
    pub generation: u64,
    pub unix: Vec<UnixSocketView>,
    pub interfaces: Vec<NetworkInterfaceView>,
    pub internet: Vec<InternetSocketView>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InternetSocketView {
    pub ipv6: bool,
    pub udp: bool,
    pub local: [u8; 16],
    pub local_port: u16,
    pub remote: [u8; 16],
    pub remote_port: u16,
    pub state: u8,
    pub inode: u64,
}

/// One namespace-owned network interface and its Linux-visible accounting.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkInterfaceView {
    pub name: Vec<u8>,
    pub index: u32,
    pub loopback: bool,
    pub address: [u8; 6],
    pub ipv4: Option<[u8; 4]>,
    pub prefix: u8,
    pub receive: [u64; 8],
    pub transmit: [u64; 8],
}

impl NetworkView {
    pub(super) fn unix_bytes(&self) -> Vec<u8> {
        let mut bytes = b"Num       RefCount Protocol Flags    Type St Inode Path\n".to_vec();
        for socket in &self.unix {
            bytes.extend_from_slice(
                format!(
                    "{:016x}: {:08x} {:08x} {:08x} {:04x} {:02x} {:5} ",
                    socket.identity,
                    socket.reference_count,
                    socket.protocol,
                    socket.flags,
                    socket.socket_type,
                    socket.state,
                    socket.inode,
                )
                .as_bytes(),
            );
            if let Some(path) = &socket.path {
                bytes.extend_from_slice(path);
            }
            bytes.push(b'\n');
        }
        bytes
    }

    pub(super) fn entries(&self) -> impl Iterator<Item = (&'static [u8], u8)> {
        const NAMES: &[&[u8]] = &[
            b"arp",
            b"dev",
            b"dev_mcast",
            b"if_inet6",
            b"igmp",
            b"ipv6_route",
            b"netstat",
            b"route",
            b"snmp",
            b"snmp6",
            b"sockstat",
            b"tcp",
            b"tcp6",
            b"udp",
            b"udp6",
            b"unix",
        ];
        NAMES.iter().map(|name| (*name, 8))
    }

    pub(super) fn bytes(&self, leaf: NetworkLeaf) -> Vec<u8> {
        match leaf {
            NetworkLeaf::Unix => self.unix_bytes(),
            NetworkLeaf::Dev => self.dev_bytes(),
            NetworkLeaf::Route => self.route_bytes(),
            NetworkLeaf::IfInet6 => b"00000000000000000000000000000001 01 80 10 80        lo\n".to_vec(),
            NetworkLeaf::Tcp => self.socket_bytes(false, false),
            NetworkLeaf::Tcp6 => self.socket_bytes(true, false),
            NetworkLeaf::Udp => self.socket_bytes(false, true),
            NetworkLeaf::Udp6 => self.socket_bytes(true, true),
            NetworkLeaf::Arp => b"IP address       HW type     Flags       HW address            Mask     Device\n".to_vec(),
            NetworkLeaf::Igmp => self.igmp_bytes(),
            NetworkLeaf::DevMcast => Vec::new(),
            NetworkLeaf::Ipv6Route => b"00000000000000000000000000000001 80 00000000000000000000000000000000 00 00000000000000000000000000000000 00000000 00000002 00000000 80200001       lo\n".to_vec(),
            NetworkLeaf::Snmp => SNMP.as_bytes().to_vec(),
            NetworkLeaf::Netstat => NETSTAT.as_bytes().to_vec(),
            NetworkLeaf::Snmp6 => SNMP6.as_bytes().to_vec(),
            NetworkLeaf::Sockstat => format!(
                "sockets: used {}\nTCP: inuse 0 orphan 0 tw 0 alloc 0 mem 0\nUDP: inuse 0 mem 0\n",
                self.unix.len()
            )
            .into_bytes(),
        }
    }

    fn dev_bytes(&self) -> Vec<u8> {
        let mut output = String::from(
            "Inter-|   Receive                                                |  Transmit\n face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n",
        );
        for interface in &self.interfaces {
            let name = String::from_utf8_lossy(&interface.name);
            let receive = interface
                .receive
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(" ");
            let transmit = interface
                .transmit
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(" ");
            output.push_str(&format!("{name:>6}: {receive} {transmit}\n"));
        }
        output.into_bytes()
    }

    fn route_bytes(&self) -> Vec<u8> {
        let mut output =
            String::from("Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n");
        for interface in self.interfaces.iter().filter(|interface| !interface.loopback) {
            let Some(ip) = interface.ipv4 else { continue };
            let host = u32::from_le_bytes(ip);
            let mask = if interface.prefix == 32 {
                u32::MAX
            } else {
                (1_u32 << interface.prefix) - 1
            };
            let network = host & mask;
            let gateway = network | 0x0100_0000;
            output.push_str(&format!(
                "{}\t00000000\t{gateway:08X}\t0003\t0\t0\t0\t00000000\t0\t0\t0\n{}\t{network:08X}\t00000000\t0001\t0\t0\t0\t{mask:08X}\t0\t0\t0\n",
                String::from_utf8_lossy(&interface.name), String::from_utf8_lossy(&interface.name)
            ));
        }
        output.into_bytes()
    }

    fn igmp_bytes(&self) -> Vec<u8> {
        let mut output = String::from("Idx\tDevice    : Count Querier\tGroup    Users Timer\tReporter\n");
        for interface in &self.interfaces {
            output.push_str(&format!(
                "{}\t{:<10}:     1      V3\n\t\t\t\t010000E0     1 0:00000000\t\t0\n",
                interface.index,
                String::from_utf8_lossy(&interface.name)
            ));
        }
        output.into_bytes()
    }

    fn socket_header(ipv6: bool, udp: bool) -> Vec<u8> {
        if ipv6 {
            format!("  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode{}\n", if udp { " ref pointer drops" } else { "" }).into_bytes()
        } else {
            format!(
                "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode{}\n",
                if udp { " ref pointer drops" } else { "" }
            )
            .into_bytes()
        }
    }

    fn socket_bytes(&self, ipv6: bool, udp: bool) -> Vec<u8> {
        let mut output = Self::socket_header(ipv6, udp);
        for (slot, socket) in self
            .internet
            .iter()
            .filter(|socket| socket.ipv6 == ipv6 && socket.udp == udp)
            .enumerate()
        {
            let encode = |bytes: &[u8]| {
                bytes
                    .chunks_exact(4)
                    .map(|part| format!("{:08X}", u32::from_le_bytes(part.try_into().unwrap())))
                    .collect::<String>()
            };
            output.extend_from_slice(format!(" {:3}: {}:{:04X} {}:{:04X} {:02X} 00000000:00000000 00:00000000 00000000 0 0 {} 1 0000000000000000 100 0 0 10 0\n", slot, encode(if ipv6 { &socket.local } else { &socket.local[..4] }), socket.local_port, encode(if ipv6 { &socket.remote } else { &socket.remote[..4] }), socket.remote_port, socket.state, socket.inode).as_bytes());
        }
        output
    }

    pub(super) fn interface(&self, name: &[u8]) -> Option<&NetworkInterfaceView> {
        self.interfaces.iter().find(|interface| interface.name == name)
    }
}

impl NetworkInterfaceView {
    pub(super) fn attribute(&self, attribute: InterfaceAttribute) -> Vec<u8> {
        let value = match attribute {
            InterfaceAttribute::Address => format!(
                "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}\n",
                self.address[0], self.address[1], self.address[2], self.address[3], self.address[4], self.address[5]
            ),
            InterfaceAttribute::Mtu => format!("{}\n", if self.loopback { 65536 } else { 1500 }),
            InterfaceAttribute::IfIndex => format!("{}\n", self.index),
            InterfaceAttribute::Type => format!("{}\n", if self.loopback { 772 } else { 1 }),
            InterfaceAttribute::Flags => format!("{}\n", if self.loopback { "0x9" } else { "0x1003" }),
            InterfaceAttribute::Operstate => format!("{}\n", if self.loopback { "unknown" } else { "up" }),
            InterfaceAttribute::Statistic(_) => String::from("0\n"),
        };
        value.into_bytes()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum InterfaceAttribute {
    Address,
    IfIndex,
    Mtu,
    Operstate,
    Type,
    Flags,
    Statistic(Statistic),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Statistic {
    RxBytes,
    TxPackets,
}

impl InterfaceAttribute {
    pub(super) const fn identity(self) -> u64 {
        match self {
            Self::Address => 1,
            Self::IfIndex => 2,
            Self::Mtu => 3,
            Self::Operstate => 4,
            Self::Type => 5,
            Self::Flags => 6,
            Self::Statistic(Statistic::RxBytes) => 7,
            Self::Statistic(Statistic::TxPackets) => 8,
        }
    }

    fn parse(path: &[u8]) -> Option<Self> {
        Some(match path {
            b"address" => Self::Address,
            b"ifindex" => Self::IfIndex,
            b"mtu" => Self::Mtu,
            b"operstate" => Self::Operstate,
            b"type" => Self::Type,
            b"flags" => Self::Flags,
            b"statistics/rx_bytes" => Self::Statistic(Statistic::RxBytes),
            b"statistics/tx_packets" => Self::Statistic(Statistic::TxPackets),
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NetworkLeaf {
    Arp,
    Dev,
    DevMcast,
    IfInet6,
    Igmp,
    Ipv6Route,
    Netstat,
    Route,
    Snmp,
    Snmp6,
    Sockstat,
    Tcp,
    Tcp6,
    Udp,
    Udp6,
    Unix,
}

impl NetworkLeaf {
    fn parse(name: &[u8]) -> Option<Self> {
        Some(match name {
            b"arp" => Self::Arp,
            b"dev" => Self::Dev,
            b"dev_mcast" => Self::DevMcast,
            b"if_inet6" => Self::IfInet6,
            b"igmp" => Self::Igmp,
            b"ipv6_route" => Self::Ipv6Route,
            b"netstat" => Self::Netstat,
            b"route" => Self::Route,
            b"snmp" => Self::Snmp,
            b"snmp6" => Self::Snmp6,
            b"sockstat" => Self::Sockstat,
            b"tcp" => Self::Tcp,
            b"tcp6" => Self::Tcp6,
            b"udp" => Self::Udp,
            b"udp6" => Self::Udp6,
            b"unix" => Self::Unix,
            _ => return None,
        })
    }
}

const SNMP: &str = "Ip: Forwarding DefaultTTL InReceives\nIp: 2 64 0\nIcmp: InMsgs InErrors\nIcmp: 0 0\nTcp: RtoAlgorithm RtoMin RtoMax MaxConn\nTcp: 1 200 120000 -1\nUdp: InDatagrams OutDatagrams\nUdp: 0 0\n";
const NETSTAT: &str = "TcpExt: TCPFastRetrans\nTcpExt: 0\nIpExt: InOctets\nIpExt: 0\n";
const SNMP6: &str = "Ip6InReceives                   \t0\nUdp6InDatagrams                 \t0\n";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeKind {
    Directory,
    Regular,
    Link,
}

impl DescriptorView {
    pub(super) fn info(&self) -> Vec<u8> {
        let mut output = format!("pos:\t{}\nflags:\t0{:o}\n", self.offset, self.flags);
        if let Some(mount) = self.mount {
            output.push_str(&format!("mnt_id:\t{mount}\n"));
        }
        output.push_str(&format!("ino:\t{}\n", self.inode));
        output.into_bytes()
    }
}

impl ProcessView {
    pub(super) fn comm(&self) -> Vec<u8> {
        let mut name = self.name.split(|byte| *byte == 0).next().unwrap_or(&[]).to_vec();
        name.push(b'\n');
        name
    }

    pub(super) fn status(&self) -> Vec<u8> {
        let name = self.name.split(|byte| *byte == 0).next().unwrap_or(&[]);
        let name = String::from_utf8_lossy(name);
        let groups = self.groups.iter().map(u32::to_string).collect::<Vec<_>>().join(" ");
        let mut output = format!("Name:\t{name}\n");
        if let Some(umask) = self.umask {
            output.push_str(&format!("Umask:\t{umask:04o}\n"));
        }
        output.push_str(&format!(
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
        ));
        output.push_str(&format!(
            "Cpus_allowed:\t{}\nCpus_allowed_list:\t{}\n",
            self.allowed_mask, self.allowed_list,
        ));
        if let Some(memory) = self.memory {
            let page_kb = memory.page_bytes / 1024;
            let size = memory.total_pages.saturating_mul(page_kb);
            let resident = memory.resident_pages.saturating_mul(page_kb);
            output.push_str(&format!("VmSize:\t{size} kB\nVmRSS:\t{resident} kB\n"));
        }
        output.into_bytes()
    }

    pub(super) fn limits(&self) -> Vec<u8> {
        let mut output = format!(
            "{:<25} {:<20} {:<20} {:<10}\n",
            "Limit", "Soft Limit", "Hard Limit", "Units",
        );
        let mut limits = self.limits.clone();
        limits.sort_by_key(|limit| limit.resource);
        for limit in limits {
            let (name, units) = limit.resource.label();
            output.push_str(&format!(
                "{name:<25} {:<20} {:<20} {units:<10}\n",
                limit.soft_text(),
                limit.hard_text(),
            ));
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Node {
    ProcRoot,
    ProcessDirectory,
    TaskDirectory,
    ThreadDirectory,
    NamespaceDirectory,
    CgroupNamespace,
    IpcNamespace,
    MountNamespace,
    NetworkNamespace,
    PidNamespace,
    TimeNamespace,
    UserNamespace,
    NetworkDirectory,
    NetworkFile(NetworkLeaf),
    InterfaceRoot,
    InterfaceDirectory,
    StatisticsDirectory,
    InterfaceFile(InterfaceAttribute),
    UtsNamespace,
    Comm,
    Cmdline,
    Environ,
    OomScore,
    OomScoreAdj,
    OomAdj,
    Status,
    ProcessStat,
    Statm,
    Io,
    Maps,
    NumaMaps,
    SmapsRollup,
    MapFiles,
    MapFile(u64, u64),
    Smaps,
    Limits,
    Mounts,
    MountInfo,
    MountStats,
    Root,
    Cwd,
    Fd,
    FdInfo,
    FdLink(i32),
    FdInfoFile(i32),
    CpuInfo,
    CpuStat,
    CpuDirectory,
    CpuRange,
    CpuLeaf(usize),
    CpuTopology(usize),
    CpuCoreId(usize),
    CpuPackageId(usize),
    CpuClusterId(usize),
    CpuThreadMask(usize),
    CpuThreadList(usize),
    CpuCoreMask(usize),
    CpuCoreList(usize),
    CpuPackageMask(usize),
    CpuPackageList(usize),
    CpuClusterMask(usize),
    CpuClusterList(usize),
    BlockDirectory,
    TtyDirectory,
    TtyDrivers,
    TtyDisciplines,
    BootIdentity,
    RandomIdentity,
    EntropyAvailable,
    RandomPoolSize,
    Sysctl(&'static [u8]),
    MemInfo,
    Devices,
    Uptime,
    LoadAverage,
    KernelCommandLine,
    Filesystems,
    Version,
    Hostname,
    Domainname,
    CgroupRoot,
    Cgroup(super::cgroup::Leaf),
    Membership,
}

impl Node {
    pub(super) fn static_namespace(self) -> Option<(&'static str, u64)> {
        match self {
            Self::CgroupNamespace => Some(("cgroup", 4_026_531_835)),
            Self::IpcNamespace => Some(("ipc", 4_026_531_839)),
            Self::MountNamespace => Some(("mnt", 4_026_531_841)),
            Self::PidNamespace => Some(("pid", 4_026_531_836)),
            Self::TimeNamespace => Some(("time", 4_026_531_834)),
            Self::UserNamespace => Some(("user", 4_026_531_837)),
            _ => None,
        }
    }

    pub(super) fn interface_name(path: &[u8]) -> Option<&[u8]> {
        let path = path
            .strip_prefix(b"/")
            .unwrap_or(path)
            .strip_prefix(b"sys/class/net/")?;
        path.split(|byte| *byte == b'/').next()
    }

    pub(super) fn entries(self) -> Option<&'static [(&'static [u8], u8)]> {
        const PROCESS: &[(&[u8], u8)] = &[
            (b"cmdline", 8),
            (b"comm", 8),
            (b"cwd", 10),
            (b"fd", 4),
            (b"fdinfo", 4),
            (b"environ", 8),
            (b"oom_score", 8),
            (b"oom_score_adj", 8),
            (b"oom_adj", 8),
            (b"io", 8),
            (b"limits", 8),
            (b"maps", 8),
            (b"numa_maps", 8),
            (b"smaps_rollup", 8),
            (b"map_files", 4),
            (b"mounts", 8),
            (b"mountinfo", 8),
            (b"mountstats", 8),
            (b"pagemap", 8),
            (b"ns", 4),
            (b"root", 10),
            (b"status", 8),
            (b"stat", 8),
            (b"statm", 8),
            (b"smaps", 8),
            (b"task", 4),
            (b"net", 4),
        ];
        const THREAD: &[(&[u8], u8)] = &[
            (b"comm", 8),
            (b"cwd", 10),
            (b"fd", 4),
            (b"fdinfo", 4),
            (b"environ", 8),
            (b"oom_score", 8),
            (b"oom_score_adj", 8),
            (b"oom_adj", 8),
            (b"io", 8),
            (b"limits", 8),
            (b"maps", 8),
            (b"numa_maps", 8),
            (b"smaps_rollup", 8),
            (b"map_files", 4),
            (b"mounts", 8),
            (b"mountinfo", 8),
            (b"mountstats", 8),
            (b"pagemap", 8),
            (b"ns", 4),
            (b"root", 10),
            (b"status", 8),
            (b"stat", 8),
            (b"statm", 8),
            (b"smaps", 8),
        ];
        match self {
            Self::ProcessDirectory => Some(PROCESS),
            Self::ThreadDirectory => Some(THREAD),
            _ => None,
        }
    }

    pub(super) fn parse(path: &[u8], current: u32) -> Option<(u32, Option<u32>, Self)> {
        let path = path.strip_prefix(b"/").unwrap_or(path);
        if path == b"dev/fd" || path == b"dev/fd/" {
            return Some((current, None, Self::Fd));
        }
        if let Some(number) = path.strip_prefix(b"dev/fd/") {
            return Some((current, None, Self::FdLink(Self::number(number)?)));
        }
        if path == b"proc/mounts" {
            return Some((current, None, Self::Mounts));
        }
        if path == b"proc/net" || path == b"proc/net/" {
            return Some((current, None, Self::NetworkDirectory));
        }
        if let Some(name) = path.strip_prefix(b"proc/net/") {
            return Some((current, None, Self::NetworkFile(NetworkLeaf::parse(name)?)));
        }
        if path == b"sys/class/net" || path == b"sys/class/net/" {
            return Some((current, None, Self::InterfaceRoot));
        }
        if let Some(tail) = path.strip_prefix(b"sys/class/net/") {
            let slash = tail.iter().position(|byte| *byte == b'/');
            let (name, leaf) = slash.map_or((tail, &b""[..]), |slash| (&tail[..slash], &tail[slash + 1..]));
            if name.is_empty() {
                return None;
            }
            return Some((
                current,
                None,
                match leaf {
                    b"" => Self::InterfaceDirectory,
                    b"statistics" | b"statistics/" => Self::StatisticsDirectory,
                    _ => Self::InterfaceFile(InterfaceAttribute::parse(leaf)?),
                },
            ));
        }
        if path == b"sys/fs/cgroup" || path == b"sys/fs/cgroup/" {
            return Some((0, None, Self::CgroupRoot));
        }
        if let Some(name) = path.strip_prefix(b"sys/fs/cgroup/") {
            return Some((0, None, Self::Cgroup(super::cgroup::Leaf::parse(name)?)));
        }
        let global = match path {
            b"proc" | b"proc/" => Some(Self::ProcRoot),
            b"proc/cpuinfo" => Some(Self::CpuInfo),
            b"proc/stat" => Some(Self::CpuStat),
            b"proc/meminfo" => Some(Self::MemInfo),
            b"proc/devices" => Some(Self::Devices),
            b"proc/uptime" => Some(Self::Uptime),
            b"proc/loadavg" => Some(Self::LoadAverage),
            b"proc/cmdline" => Some(Self::KernelCommandLine),
            b"proc/filesystems" => Some(Self::Filesystems),
            b"proc/version" => Some(Self::Version),
            b"proc/tty" | b"proc/tty/" => Some(Self::TtyDirectory),
            b"proc/tty/drivers" => Some(Self::TtyDrivers),
            b"proc/tty/ldiscs" => Some(Self::TtyDisciplines),
            b"proc/sys/kernel/random/boot_id" => Some(Self::BootIdentity),
            b"proc/sys/kernel/random/uuid" => Some(Self::RandomIdentity),
            b"proc/sys/kernel/random/entropy_avail" => Some(Self::EntropyAvailable),
            b"proc/sys/kernel/random/poolsize" => Some(Self::RandomPoolSize),
            b"proc/sys/kernel/ostype" => Some(Self::Sysctl(b"Linux\n")),
            b"proc/sys/kernel/osrelease" => Some(Self::Sysctl(b"6.1.0\n")),
            b"proc/sys/kernel/pid_max" => Some(Self::Sysctl(b"4194304\n")),
            b"proc/sys/kernel/cap_last_cap" => Some(Self::Sysctl(b"40\n")),
            b"proc/sys/kernel/threads-max" => Some(Self::Sysctl(b"63488\n")),
            b"proc/sys/kernel/ngroups_max" => Some(Self::Sysctl(b"65536\n")),
            b"proc/sys/kernel/overflowuid" => Some(Self::Sysctl(b"65534\n")),
            b"proc/sys/kernel/overflowgid" => Some(Self::Sysctl(b"65534\n")),
            b"proc/sys/kernel/randomize_va_space" => Some(Self::Sysctl(b"2\n")),
            b"proc/sys/kernel/shmmax" | b"proc/sys/kernel/shmall" => Some(Self::Sysctl(b"18446744073692774399\n")),
            b"proc/sys/kernel/shmmni" => Some(Self::Sysctl(b"4096\n")),
            b"proc/sys/kernel/sem" => Some(Self::Sysctl(b"256\t131072\t500\t512\n")),
            b"proc/sys/vm/max_map_count" => Some(Self::Sysctl(b"1048576\n")),
            b"proc/sys/vm/mmap_min_addr" => Some(Self::Sysctl(b"32768\n")),
            b"proc/sys/vm/overcommit_memory" => Some(Self::Sysctl(b"1\n")),
            b"proc/sys/vm/swappiness" => Some(Self::Sysctl(b"20\n")),
            b"proc/sys/net/core/somaxconn" => Some(Self::Sysctl(b"4096\n")),
            b"proc/sys/net/core/rmem_max" | b"proc/sys/net/core/wmem_max" => Some(Self::Sysctl(b"7500000\n")),
            b"proc/sys/net/ipv4/ip_local_port_range" => Some(Self::Sysctl(b"32768\t60999\n")),
            b"proc/sys/net/ipv4/tcp_keepalive_time" => Some(Self::Sysctl(b"7200\n")),
            b"proc/sys/net/ipv4/tcp_max_syn_backlog" => Some(Self::Sysctl(b"1024\n")),
            b"proc/sys/net/ipv4/tcp_congestion_control" => Some(Self::Sysctl(b"cubic\n")),
            b"proc/sys/fs/file-max" => Some(Self::Sysctl(b"9223372036854775807\n")),
            b"proc/sys/fs/nr_open" => Some(Self::Sysctl(b"2147483584\n")),
            b"proc/sys/fs/aio-max-nr" => Some(Self::Sysctl(b"1048576\n")),
            b"proc/sys/fs/inotify/max_user_watches" => Some(Self::Sysctl(b"524288\n")),
            b"proc/sys/fs/inotify/max_user_instances" => Some(Self::Sysctl(b"524288\n")),
            b"proc/sys/fs/inotify/max_queued_events" => Some(Self::Sysctl(b"1048576\n")),
            b"proc/sys/fs/mqueue/msg_max" => Some(Self::Sysctl(b"10\n")),
            b"proc/sys/fs/mqueue/msgsize_max" => Some(Self::Sysctl(b"8192\n")),
            b"proc/sys/fs/mqueue/queues_max" => Some(Self::Sysctl(b"256\n")),
            b"sys/kernel/mm/transparent_hugepage/enabled" => Some(Self::Sysctl(b"always [madvise] never\n")),
            b"proc/sys/kernel/hostname" => Some(Self::Hostname),
            b"proc/sys/kernel/domainname" => Some(Self::Domainname),
            b"sys/devices/system/cpu" | b"sys/devices/system/cpu/" => Some(Self::CpuDirectory),
            b"sys/class/block" | b"sys/class/block/" | b"sys/block" | b"sys/block/" => Some(Self::BlockDirectory),
            b"sys/devices/system/cpu/online"
            | b"sys/devices/system/cpu/possible"
            | b"sys/devices/system/cpu/present" => Some(Self::CpuRange),
            _ => None,
        };
        if let Some(node) = global {
            return Some((0, None, node));
        }
        if let Some(tail) = path.strip_prefix(b"sys/devices/system/cpu/cpu") {
            let separator = tail.iter().position(|byte| *byte == b'/');
            let (number, leaf) = separator.map_or((tail, &b""[..]), |separator| {
                (&tail[..separator], &tail[separator + 1..])
            });
            let number = std::str::from_utf8(number).ok()?.parse().ok()?;
            let node = match leaf {
                b"" => Self::CpuLeaf(number),
                b"topology" | b"topology/" => Self::CpuTopology(number),
                b"topology/core_id" => Self::CpuCoreId(number),
                b"topology/physical_package_id" => Self::CpuPackageId(number),
                b"topology/cluster_id" => Self::CpuClusterId(number),
                b"topology/thread_siblings" => Self::CpuThreadMask(number),
                b"topology/thread_siblings_list" => Self::CpuThreadList(number),
                b"topology/core_cpus" => Self::CpuCoreMask(number),
                b"topology/core_cpus_list" => Self::CpuCoreList(number),
                b"topology/core_siblings" | b"topology/package_cpus" => Self::CpuPackageMask(number),
                b"topology/core_siblings_list" => Self::CpuPackageList(number),
                b"topology/package_cpus_list" => Self::CpuPackageList(number),
                b"topology/cluster_cpus" => Self::CpuClusterMask(number),
                b"topology/cluster_cpus_list" => Self::CpuClusterList(number),
                _ => return None,
            };
            return Some((0, None, node));
        }
        let tail = path.strip_prefix(b"proc/")?;
        let separator = tail.iter().position(|byte| *byte == b'/');
        let (owner, leaf) = separator.map_or((tail, &b""[..]), |separator| {
            (&tail[..separator], &tail[separator + 1..])
        });
        let process = if owner == b"self" {
            current
        } else {
            std::str::from_utf8(owner).ok()?.parse::<u32>().ok()?
        };
        if leaf.is_empty() {
            return Some((process, None, Self::ProcessDirectory));
        }
        if leaf == b"cgroup" {
            return Some((process, None, Self::Membership));
        }
        if leaf == b"task" || leaf == b"task/" {
            return Some((process, None, Self::TaskDirectory));
        }
        if leaf == b"ns" || leaf == b"ns/" {
            return Some((process, None, Self::NamespaceDirectory));
        }
        if leaf == b"net" || leaf == b"net/" {
            return Some((process, None, Self::NetworkDirectory));
        }
        if let Some(name) = leaf.strip_prefix(b"net/") {
            return Some((process, None, Self::NetworkFile(NetworkLeaf::parse(name)?)));
        }
        let (thread, leaf) = if let Some(task) = leaf.strip_prefix(b"task/") {
            let separator = task.iter().position(|byte| *byte == b'/');
            let (thread, leaf) = separator.map_or((task, &b""[..]), |separator| {
                (&task[..separator], &task[separator + 1..])
            });
            let thread = Self::task_number(thread)?;
            if leaf.is_empty() {
                return Some((process, Some(thread), Self::ThreadDirectory));
            }
            (Some(thread), leaf)
        } else {
            (None, leaf)
        };
        let leaf = match leaf {
            b"ns" | b"ns/" => Self::NamespaceDirectory,
            b"comm" => Self::Comm,
            b"cmdline" => Self::Cmdline,
            b"environ" => Self::Environ,
            b"oom_score" => Self::OomScore,
            b"oom_score_adj" => Self::OomScoreAdj,
            b"oom_adj" => Self::OomAdj,
            b"status" => Self::Status,
            b"stat" => Self::ProcessStat,
            b"statm" => Self::Statm,
            b"io" => Self::Io,
            b"maps" => Self::Maps,
            b"numa_maps" => Self::NumaMaps,
            b"smaps_rollup" => Self::SmapsRollup,
            b"map_files" | b"map_files/" => Self::MapFiles,
            b"smaps" => Self::Smaps,
            b"limits" => Self::Limits,
            b"mounts" => Self::Mounts,
            b"mountinfo" => Self::MountInfo,
            b"mountstats" => Self::MountStats,
            b"ns/uts" => Self::UtsNamespace,
            b"ns/net" => Self::NetworkNamespace,
            b"ns/cgroup" => Self::CgroupNamespace,
            b"ns/ipc" => Self::IpcNamespace,
            b"ns/mnt" => Self::MountNamespace,
            b"ns/pid" => Self::PidNamespace,
            b"ns/time" => Self::TimeNamespace,
            b"ns/user" => Self::UserNamespace,
            b"root" => Self::Root,
            b"cwd" => Self::Cwd,
            b"fd" => Self::Fd,
            b"fdinfo" => Self::FdInfo,
            _ if leaf.starts_with(b"fd/") => Self::FdLink(Self::number(&leaf[3..])?),
            _ if leaf.starts_with(b"fdinfo/") => Self::FdInfoFile(Self::number(&leaf[7..])?),
            _ if leaf.starts_with(b"map_files/") => {
                let (start, end) = Self::map_range(&leaf[10..])?;
                Self::MapFile(start, end)
            }
            _ => return None,
        };
        Some((process, thread, leaf))
    }

    fn number(bytes: &[u8]) -> Option<i32> {
        if bytes.is_empty() || bytes.contains(&b'/') {
            return None;
        }
        std::str::from_utf8(bytes)
            .ok()?
            .parse()
            .ok()
            .filter(|number| *number >= 0)
    }

    fn task_number(bytes: &[u8]) -> Option<u32> {
        if bytes.is_empty() || bytes.contains(&b'/') {
            return None;
        }
        std::str::from_utf8(bytes)
            .ok()?
            .parse()
            .ok()
            .filter(|number| *number > 0)
    }

    fn map_range(bytes: &[u8]) -> Option<(u64, u64)> {
        let separator = bytes.iter().position(|byte| *byte == b'-')?;
        let (start, end) = (&bytes[..separator], &bytes[separator + 1..]);
        if start.is_empty() || end.is_empty() || end.contains(&b'/') {
            return None;
        }
        let start = u64::from_str_radix(std::str::from_utf8(start).ok()?, 16).ok()?;
        let end = u64::from_str_radix(std::str::from_utf8(end).ok()?, 16).ok()?;
        (start < end).then_some((start, end))
    }
}
