//! Instance-wide CPU, UTS, and system projections used by procfs.

use std::fmt::Write as _;

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
impl SystemView {
    pub(in crate::procfs) fn meminfo(self) -> Vec<u8> {
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

    pub(in crate::procfs) fn uptime(self) -> Vec<u8> {
        format!("{}.00 0.00\n", self.uptime_seconds).into_bytes()
    }
}

impl CpuView {
    #[must_use]
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

    pub(in crate::procfs) fn mask(&self, bit: Option<usize>) -> String {
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

    pub(in crate::procfs) fn version(&self) -> Vec<u8> {
        let architecture = match &self.model {
            CpuModel::Aarch64 { .. } => "aarch64",
            CpuModel::X86_64 { .. } => "x86_64",
        };
        format!("Linux version 6.1.0 (hl-engine) {architecture}\n").into_bytes()
    }

    pub(in crate::procfs) fn cpuinfo(&self) -> Vec<u8> {
        let mut output = String::new();
        for cpu in 0..self.online {
            match &self.model {
                CpuModel::Aarch64 { .. } => {
                    let features = self.model.capability_names();
                    let _ = write!(
                        output,
                        "processor\t: {cpu}\nBogoMIPS\t: 100.00\nFeatures\t: {features}\n\
                         CPU implementer\t: 0x61\nCPU architecture: 8\nCPU variant\t: 0x0\n\
                         CPU part\t: 0x000\nCPU revision\t: 0\n\n"
                    );
                }
                CpuModel::X86_64 {
                    vendor,
                    family,
                    model,
                    stepping,
                    name,
                    flags,
                } => {
                    let _ = write!(
                        output,
                        "processor\t: {cpu}\nvendor_id\t: {vendor}\ncpu family\t: {family}\nmodel\t\t: {model}\n\
                     model name\t: {name}\nstepping\t: {stepping}\nfpu\t\t: yes\n\
                     fpu_exception\t: yes\nflags\t\t: {}\n\n",
                        flags.join(" ")
                    );
                }
            }
        }
        output.into_bytes()
    }

    pub(in crate::procfs) fn stat(&self, system: SystemView) -> Vec<u8> {
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
            let _ = writeln!(
                output,
                "cpu{cpu} {} {} {} {} 0 0 0 0 0 0",
                ticks.user, ticks.nice, ticks.system, ticks.idle,
            );
        }
        let activity = system.uptime_seconds.saturating_mul(100).saturating_add(1);
        let _ = write!(
            output,
            "intr {}\nctxt {}\nbtime 1\nprocesses {}\nprocs_running 1\nprocs_blocked 0\n",
            activity.saturating_mul(137),
            activity.saturating_mul(509),
            system.process_creations.saturating_add(256),
        );
        output.into_bytes()
    }

    pub(in crate::procfs) fn range_bytes(&self) -> Vec<u8> {
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
