/// Immutable instance-owned values projected through one unified cgroup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct View {
    online_cpus: usize,
    cpu_limit: Option<usize>,
    memory_limit: Option<u64>,
    process_limit: Option<usize>,
    memory_current: u64,
    processes: Vec<u32>,
    threads: Vec<u32>,
}

pub(super) const ROOT_NAMES: &[&[u8]] = &[
    b"cgroup.controllers",
    b"cgroup.subtree_control",
    b"cgroup.type",
    b"cgroup.procs",
    b"cgroup.threads",
    b"cgroup.events",
    b"cgroup.stat",
    b"cgroup.max.depth",
    b"cgroup.max.descendants",
    b"cpu.max",
    b"cpu.stat",
    b"cpu.weight",
    b"cpuset.cpus",
    b"cpuset.mems",
    b"cpuset.cpus.effective",
    b"cpuset.mems.effective",
    b"memory.max",
    b"memory.min",
    b"memory.low",
    b"memory.high",
    b"memory.current",
    b"memory.peak",
    b"memory.events",
    b"memory.stat",
    b"memory.swap.max",
    b"memory.swap.current",
    b"memory.oom.group",
    b"pids.max",
    b"pids.current",
    b"pids.peak",
    b"pids.events",
    b"io.max",
    b"io.stat",
    b"io.weight",
];

impl View {
    /// Validates bounded topology, resource accounting, and membership identities.
    #[must_use]
    pub fn new(
        online_cpus: usize,
        cpu_limit: Option<usize>,
        memory_limit: Option<u64>,
        process_limit: Option<usize>,
        memory_current: u64,
        mut processes: Vec<u32>,
        mut threads: Vec<u32>,
    ) -> Option<Self> {
        if online_cpus == 0
            || online_cpus > 64
            || cpu_limit == Some(0)
            || cpu_limit.is_some_and(|limit| limit > online_cpus)
            || memory_limit == Some(0)
            || process_limit == Some(0)
            || memory_limit.is_some_and(|limit| memory_current > limit)
            || processes.contains(&0)
            || threads.contains(&0)
        {
            return None;
        }
        processes.sort_unstable();
        processes.dedup();
        threads.sort_unstable();
        threads.dedup();
        Some(Self {
            online_cpus,
            cpu_limit,
            memory_limit,
            process_limit,
            memory_current,
            processes,
            threads,
        })
    }

    pub(super) fn membership(&self, process: u32) -> Option<Vec<u8>> {
        self.processes.binary_search(&process).ok()?;
        Some(b"0::/\n".to_vec())
    }

    pub(super) fn render(&self, leaf: Leaf) -> Vec<u8> {
        match leaf {
            Leaf::Controllers => b"cpuset cpu io memory pids\n".to_vec(),
            Leaf::Subtree => Vec::new(),
            Leaf::Type => b"domain\n".to_vec(),
            Leaf::Processes => Self::members(&self.processes),
            Leaf::Threads => Self::members(&self.threads),
            Leaf::Events => b"populated 1\nfrozen 0\n".to_vec(),
            Leaf::Stat => b"nr_descendants 0\nnr_dying_descendants 0\n".to_vec(),
            Leaf::Maximum => b"max\n".to_vec(),
            Leaf::CpuMax => self.cpu_limit.map_or_else(|| b"max 100000\n".to_vec(), Self::quota),
            Leaf::CpuStat => concat!(
                "usage_usec 0\nuser_usec 0\nsystem_usec 0\nnr_periods 0\n",
                "nr_throttled 0\nthrottled_usec 0\nnr_bursts 0\nburst_usec 0\n",
            )
            .as_bytes()
            .to_vec(),
            Leaf::CpuWeight => b"100\n".to_vec(),
            Leaf::CpuBurst | Leaf::CpuNice | Leaf::CpuIdle => b"0\n".to_vec(),
            Leaf::CpuSet => self.cpu_range(),
            Leaf::MemorySet => b"0\n".to_vec(),
            Leaf::MemoryMax | Leaf::SwapMax => self
                .memory_limit
                .map_or_else(|| b"max\n".to_vec(), |limit| format!("{limit}\n").into_bytes()),
            Leaf::MemoryMin => b"0\n".to_vec(),
            Leaf::MemoryHigh => b"max\n".to_vec(),
            Leaf::MemoryCurrent | Leaf::MemoryPeak => format!("{}\n", self.memory_current).into_bytes(),
            Leaf::MemoryEvents => b"low 0\nhigh 0\nmax 0\noom 0\noom_kill 0\noom_group_kill 0\n".to_vec(),
            Leaf::MemoryStat => format!(
                concat!(
                    "anon {}\nfile 0\nkernel {}\nkernel_stack 0\npagetables 0\nsec_pagetables 0\n",
                    "percpu 0\nsock 0\nvmalloc 0\nshmem 0\nfile_mapped 0\nfile_dirty 0\n",
                    "file_writeback 0\nswapcached 0\nanon_thp 0\nfile_thp 0\nshmem_thp 0\n",
                    "inactive_anon {}\nactive_anon 0\ninactive_file 0\nactive_file 0\n",
                    "unevictable 0\nslab_reclaimable 0\nslab_unreclaimable 0\nslab 0\n",
                    "workingset_refault_anon 0\nworkingset_refault_file 0\npgfault 0\npgmajfault 0\n",
                ),
                self.memory_current, self.memory_current, self.memory_current,
            )
            .into_bytes(),
            Leaf::SwapCurrent | Leaf::SwapPeak | Leaf::OomGroup => b"0\n".to_vec(),
            Leaf::SwapHigh => b"max\n".to_vec(),
            Leaf::SwapEvents => b"high 0\nmax 0\nfail 0\n".to_vec(),
            Leaf::PidsMax => self
                .process_limit
                .map_or_else(|| b"max\n".to_vec(), |limit| format!("{limit}\n").into_bytes()),
            Leaf::PidsCurrent | Leaf::PidsPeak => format!("{}\n", self.threads.len()).into_bytes(),
            Leaf::PidsEvents => b"max 0\n".to_vec(),
            Leaf::CpuStatLocal => b"throttled_usec 0\n".to_vec(),
            Leaf::IoMax | Leaf::IoStat => Vec::new(),
            Leaf::IoWeight => b"default 100\n".to_vec(),
        }
    }

    fn cpu_range(&self) -> Vec<u8> {
        match self.online_cpus {
            1 => b"0\n".to_vec(),
            cpus => format!("0-{}\n", cpus - 1).into_bytes(),
        }
    }

    fn quota(cpus: usize) -> Vec<u8> {
        format!("{} 100000\n", cpus * 100_000).into_bytes()
    }

    fn members(members: &[u32]) -> Vec<u8> {
        use std::fmt::Write as _;
        members
            .iter()
            .fold(String::new(), |mut text, member| {
                let _ = writeln!(text, "{member}");
                text
            })
            .into_bytes()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Leaf {
    Controllers,
    Subtree,
    Type,
    Processes,
    Threads,
    Events,
    Stat,
    Maximum,
    CpuMax,
    CpuStat,
    CpuWeight,
    CpuBurst,
    CpuNice,
    CpuIdle,
    CpuSet,
    MemorySet,
    MemoryMax,
    MemoryMin,
    MemoryHigh,
    MemoryCurrent,
    MemoryPeak,
    MemoryEvents,
    MemoryStat,
    SwapMax,
    SwapCurrent,
    SwapHigh,
    SwapPeak,
    SwapEvents,
    OomGroup,
    PidsMax,
    PidsCurrent,
    PidsPeak,
    PidsEvents,
    CpuStatLocal,
    IoMax,
    IoStat,
    IoWeight,
}

impl Leaf {
    pub(super) fn parse(name: &[u8]) -> Option<Self> {
        Some(match name {
            b"cgroup.controllers" => Self::Controllers,
            b"cgroup.subtree_control" => Self::Subtree,
            b"cgroup.type" => Self::Type,
            b"cgroup.procs" => Self::Processes,
            b"cgroup.threads" => Self::Threads,
            b"cgroup.events" => Self::Events,
            b"cgroup.stat" => Self::Stat,
            b"cgroup.max.depth" | b"cgroup.max.descendants" => Self::Maximum,
            b"cpu.max" => Self::CpuMax,
            b"cpu.stat" => Self::CpuStat,
            b"cpu.weight" => Self::CpuWeight,
            b"cpu.max.burst" => Self::CpuBurst,
            b"cpu.weight.nice" => Self::CpuNice,
            b"cpu.idle" => Self::CpuIdle,
            b"cpuset.cpus" | b"cpuset.cpus.effective" => Self::CpuSet,
            b"cpuset.mems" | b"cpuset.mems.effective" => Self::MemorySet,
            b"memory.max" => Self::MemoryMax,
            b"memory.min" | b"memory.low" => Self::MemoryMin,
            b"memory.high" => Self::MemoryHigh,
            b"memory.current" => Self::MemoryCurrent,
            b"memory.peak" => Self::MemoryPeak,
            b"memory.events" | b"memory.events.local" => Self::MemoryEvents,
            b"memory.stat" => Self::MemoryStat,
            b"memory.swap.max" => Self::SwapMax,
            b"memory.swap.current" => Self::SwapCurrent,
            b"memory.swap.high" => Self::SwapHigh,
            b"memory.swap.peak" => Self::SwapPeak,
            b"memory.swap.events" => Self::SwapEvents,
            b"memory.oom.group" => Self::OomGroup,
            b"pids.max" => Self::PidsMax,
            b"pids.current" => Self::PidsCurrent,
            b"pids.peak" => Self::PidsPeak,
            b"pids.events" | b"pids.events.local" => Self::PidsEvents,
            b"cpu.stat.local" => Self::CpuStatLocal,
            b"io.max" => Self::IoMax,
            b"io.stat" => Self::IoStat,
            b"io.weight" => Self::IoWeight,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod test {
    use super::{Leaf, ROOT_NAMES, View};

    fn view(cpus: usize, cpu_limit: Option<usize>, memory_limit: Option<u64>) -> View {
        View::new(cpus, cpu_limit, memory_limit, None, 4096, vec![7, 1, 7], vec![9, 1, 8]).unwrap()
    }

    /// An unset process quota keeps `pids.max` unlimited; a set one is rendered exactly.
    #[test]
    fn process_quota_renders_only_when_configured() {
        assert_eq!(view(4, None, None).render(Leaf::PidsMax), b"max\n");
        let limited = View::new(4, None, None, Some(64), 4096, vec![7], vec![9]).unwrap();
        assert_eq!(limited.render(Leaf::PidsMax), b"64\n");
        assert!(View::new(4, None, None, Some(0), 4096, vec![7], vec![9]).is_none());
    }

    #[test]
    fn configured_limits() {
        let default = view(4, None, None);
        assert_eq!(default.render(Leaf::CpuMax), b"max 100000\n");
        assert_eq!(default.render(Leaf::MemoryMax), b"max\n");
        assert_eq!(default.render(Leaf::SwapMax), b"max\n");
        let capped = view(2, Some(2), Some(536_870_912));
        assert_eq!(capped.render(Leaf::CpuMax), b"200000 100000\n");
        assert_eq!(capped.render(Leaf::MemoryMax), b"536870912\n");
        assert_eq!(capped.render(Leaf::SwapMax), b"536870912\n");
    }

    #[test]
    fn membership_order() {
        let view = view(2, None, None);
        assert_eq!(view.membership(7), Some(b"0::/\n".to_vec()));
        assert_eq!(view.membership(2), None);
        assert_eq!(view.render(Leaf::Processes), b"1\n7\n");
        assert_eq!(view.render(Leaf::Threads), b"1\n8\n9\n");
        assert_eq!(view.render(Leaf::PidsCurrent), b"3\n");
    }

    #[test]
    fn completeness_leaves() {
        let view = view(2, None, None);
        for name in [
            b"cpuset.cpus.effective".as_slice(),
            b"cpuset.mems.effective",
            b"pids.peak",
            b"memory.oom.group",
            b"pids.events",
            b"pids.events.local",
            b"memory.swap.events",
            b"memory.swap.peak",
            b"cpu.stat.local",
        ] {
            assert!(!view.render(Leaf::parse(name).unwrap()).is_empty());
        }
        assert_eq!(Leaf::parse(b"nonexistent.controller"), None);
    }

    #[test]
    fn auxiliary_leaves() {
        let view = view(2, None, None);
        for name in [b"cpu.max.burst".as_slice(), b"cpu.weight.nice", b"cpu.idle"] {
            assert_eq!(view.render(Leaf::parse(name).unwrap()), b"0\n");
        }
        assert_eq!(view.render(Leaf::parse(b"memory.swap.high").unwrap()), b"max\n");
    }

    #[test]
    fn root_catalog() {
        assert_eq!(ROOT_NAMES.len(), 34);
        assert!(ROOT_NAMES.contains(&b"memory.max".as_slice()));
        assert!(!ROOT_NAMES.contains(&b"cpu.max.burst".as_slice()));
        assert!(!ROOT_NAMES.contains(&b"pids.events.local".as_slice()));
    }

    #[test]
    fn invalid_values() {
        assert!(View::new(0, None, None, None, 0, vec![1], vec![1]).is_none());
        assert!(View::new(1, Some(0), None, None, 0, vec![1], vec![1]).is_none());
        assert!(View::new(1, Some(2), None, None, 0, vec![1], vec![1]).is_none());
        assert!(View::new(1, None, Some(0), None, 0, vec![1], vec![1]).is_none());
        assert!(View::new(1, None, Some(5), None, 6, vec![1], vec![1]).is_none());
        assert!(View::new(1, None, None, None, 0, vec![0], vec![1]).is_none());
    }
}
