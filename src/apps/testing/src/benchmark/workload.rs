//! Named guest workloads and the machine surface each one exercises.

use clap::ValueEnum;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum Workload {
    /// Warm integer and float busyloop.
    Compute,
    /// Unpredictable data-dependent branches.
    Branch,
    /// Indirect and recursive call sequences.
    Calls,
    /// Streaming and random access over a working set larger than cache.
    Memory,
    /// Large-stride access that defeats the TLB.
    Tlb,
    /// Allocator churn.
    Malloc,
    /// Tight `gettid`-shaped syscall loop.
    Syscall,
    /// Map, touch, protect, and unmap cycles.
    Mmap,
    /// Buffered file read and write traffic.
    File,
    /// Pipe write and read round trips.
    Pipe,
    /// Signal delivery through a handler.
    Signal,
    /// Every phase the guest owns, bounded by a divisor.
    Full,
}

impl Workload {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Compute => "compute",
            Self::Branch => "branch",
            Self::Calls => "calls",
            Self::Memory => "memory",
            Self::Tlb => "tlb",
            Self::Malloc => "malloc",
            Self::Syscall => "syscall",
            Self::Mmap => "mmap",
            Self::File => "file",
            Self::Pipe => "pipe",
            Self::Signal => "signal",
            Self::Full => "full",
        }
    }

    /// Guest arguments; `Full` selects no phase so the guest runs all of them.
    pub(crate) fn guest(self, divisor: u32) -> Vec<String> {
        let mut guest = vec!["--divisor".to_owned(), divisor.to_string()];
        if self != Self::Full {
            guest.push("--phase".to_owned());
            guest.push(self.name().to_owned());
        }
        guest
    }

    pub(crate) const fn surface(self) -> &'static str {
        match self {
            Self::Compute | Self::Branch | Self::Calls => "cpu",
            Self::Memory | Self::Tlb | Self::Malloc => "memory",
            Self::Syscall | Self::Mmap | Self::File | Self::Pipe | Self::Signal => "kernel",
            Self::Full => "cpu+memory+kernel",
        }
    }

    pub(crate) fn all() -> [Self; 12] {
        [
            Self::Compute,
            Self::Branch,
            Self::Calls,
            Self::Memory,
            Self::Tlb,
            Self::Malloc,
            Self::Syscall,
            Self::Mmap,
            Self::File,
            Self::Pipe,
            Self::Signal,
            Self::Full,
        ]
    }
}

/// Surfaces the guest cannot reach today, reported so coverage gaps stay visible.
pub(crate) const GAPS: [(&str, &str); 2] = [
    ("process", "no fork/exec/wait phase exists in tests/bench/combined/main.c"),
    ("thread", "no clone/futex contention phase exists in tests/bench/combined/main.c"),
];

pub(crate) fn report() {
    println!("workload\tsurface\tguest_arguments");
    for workload in Workload::all() {
        println!("{}\t{}\t{}", workload.name(), workload.surface(), workload.guest(1).join(" "));
    }
    for (surface, detail) in GAPS {
        println!("-\t{surface}\tGAP: {detail}");
    }
}

#[cfg(test)]
mod tests {
    use super::Workload;

    #[test]
    fn full_selects_every_phase_and_named_workloads_select_one() {
        assert_eq!(Workload::Full.guest(4), ["--divisor", "4"]);
        assert_eq!(Workload::Syscall.guest(1), ["--divisor", "1", "--phase", "syscall"]);
    }

    #[test]
    fn every_workload_declares_a_surface_and_unique_name() {
        let mut names = Workload::all().map(Workload::name).to_vec();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count);
        assert!(Workload::all().iter().all(|workload| !workload.surface().is_empty()));
    }
}
