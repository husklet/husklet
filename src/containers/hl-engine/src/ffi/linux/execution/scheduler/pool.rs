//! Native executor pool state shared by the scheduler.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::{NATIVE_BOUNDARY_CAPACITY, NATIVE_SITE_LIMIT, NATIVE_SOURCE_LIMIT};

/// Above this many observed sources, re-reading each one costs more than dropping the whole set.
const SOURCE_RESCAN_LIMIT: usize = 1024;
use crate::activation::GuestIsa;
use crate::launch_plan::RuntimeLaunchPlan;

/// Leaky flip score that identifies a process whose entries alternate between direct
/// authority and the operand resolver, and the runs each such finding holds for.
const DIRECT_FLIP_LIMIT: u32 = 32;
const DIRECT_HOLD_RUNS: u64 = 1 << 16;
/// Sticky arm: a flip discards the whole translation cache, so the score does not decay
/// and the hold that follows never expires. Two flips are tolerated because the warm-up
/// run is a resolver run by construction and entering direct mode after it is one flip.
const DIRECT_STICKY_FLIP_LIMIT: u32 = 4;
const DIRECT_HOLD_PERMANENT: u64 = u64::MAX;

#[derive(Clone, Copy)]
pub(super) struct NativeBoundary {
    pub(super) process: hl_task::ProcessId,
    pub(super) start: u64,
    pub(super) exit: crate::native::NativeExit,
    pub(super) instruction: u64,
    pub(super) next: u64,
    pub(super) rip: u64,
    pub(super) stack: u64,
    pub(super) registers: [u64; 16],
    pub(super) vectors: [u128; 16],
    pub(super) vector_upper: [u128; 16],
    pub(super) flags: u16,
    pub(super) mxcsr: u32,
    pub(super) fs_base: u64,
    pub(super) gs_base: u64,
    pub(super) direction: bool,
    pub(super) alignment_check: bool,
    pub(super) id_flag: bool,
    pub(super) fault_address: u64,
    pub(super) provenance: u64,
}

pub(super) struct NativeBoundaries {
    pub(super) records: [Option<NativeBoundary>; NATIVE_BOUNDARY_CAPACITY],
    pub(super) next: usize,
}

impl NativeBoundaries {
    pub(super) fn new() -> Self {
        Self {
            records: [None; NATIVE_BOUNDARY_CAPACITY],
            next: 0,
        }
    }

    pub(super) fn push(&mut self, record: NativeBoundary) {
        self.records[self.next] = Some(record);
        self.next = (self.next + 1) % NATIVE_BOUNDARY_CAPACITY;
    }

    pub(super) fn report(&self, process: Option<hl_task::ProcessId>) {
        for offset in 0..NATIVE_BOUNDARY_CAPACITY {
            let index = (self.next + offset) % NATIVE_BOUNDARY_CAPACITY;
            let Some(record) =
                self.records[index].filter(|record| process.is_none_or(|process| record.process == process))
            else {
                continue;
            };
            eprintln!(
                "hl-native-boundary: process={:?} start={:#x} exit={:?} instruction={:#x} next={:#x} rip={:#x} rsp={:#x} registers={:#x?} vectors={:#034x?} vector_upper={:#034x?} rflags={:#x} mxcsr={:#x} fs_base={:#x} gs_base={:#x} direction={} alignment_check={} id_flag={} fault={:#x} provenance={:#x}",
                record.process,
                record.start,
                record.exit,
                record.instruction,
                record.next,
                record.rip,
                record.stack,
                record.registers,
                record.vectors,
                record.vector_upper,
                record.flags,
                record.mxcsr,
                record.fs_base,
                record.gs_base,
                record.direction,
                record.alignment_check,
                record.id_flag,
                record.fault_address,
                record.provenance,
            );
        }
    }
}

#[derive(Default)]
pub(super) struct NativeCounters {
    pub(super) builds: u64,
    pub(super) hits: u64,
    pub(super) fallbacks: u64,
    pub(super) runs: u64,
    pub(super) services: u64,
    /// Turns that asked for native entry, and the named reasons entry was declined.
    /// Anything not named lands in `probes - entries - (the named reasons)`.
    pub(super) probes: u64,
    pub(super) entries: u64,
    pub(super) declined_suppressed: u64,
    pub(super) declined_cold: u64,
    pub(super) declined_executable: u64,
    /// Diagnostic: how the suppression verdict is faring. `verdicts` counts fallbacks whose
    /// run retired too little, `latches` the ones that newly entered `suppressed`, and
    /// `clears` the entries any reset or sweep removed from it.
    pub(super) suppress_verdicts: u64,
    pub(super) suppress_latches: u64,
    pub(super) suppress_deferred: u64,
    pub(super) suppress_clears: u64,
    /// Latched entries whose retry fell short again and re-armed to another span.
    pub(super) suppress_rearms: u64,
    /// Latches made permanent because the entry has never once cleared the bar.
    pub(super) suppress_permanent: u64,
    /// Cumulative insertions into the productive table, and cumulative removals from it by
    /// any reset, purge or live-process sweep. The table's own length at teardown is a
    /// live-table reading and cannot tell an empty history from a swept one.
    pub(super) productive_marks: u64,
    pub(super) productive_swept: u64,
    /// Permanent latches whose entry PC had been productive at some earlier point in the
    /// run, i.e. the ones decided against evidence a sweep had already discarded.
    pub(super) permanent_after_productive: u64,
    /// Permanent latches split by the most this entry ever retired in any run, as a share
    /// of the budget. The rule's premise is that the entry "has now fallen short on every
    /// run it has ever had"; these three say how often that premise is false.
    pub(super) permanent_high_water_half: u64,
    pub(super) permanent_high_water_quarter: u64,
    pub(super) permanent_high_water_eighth: u64,
    /// Diagnostic: where direct authority went. `direct_admitted` counts admissions that
    /// entered with it, `direct_held` the ones a process-wide hold refused, `direct_cold`
    /// the ones still spending their warm-up resolver run, and `direct_site` the ones a
    /// per-site decline refused. `direct_flips` counts observed run-mode changes, which
    /// discard the shared translation cache when split mode is off; `direct_holds` counts
    /// the holds installed, and
    /// `direct_executors_created` proves a split run allocated its lazy direct sibling.
    pub(super) direct_admitted: u64,
    pub(super) direct_held: u64,
    pub(super) direct_cold: u64,
    pub(super) direct_site: u64,
    pub(super) direct_flips: u64,
    pub(super) direct_holds: u64,
    pub(super) direct_executors_created: u64,
}

/// Process, image incarnation, executable-range version, entry PC. The ledger
/// generation is deliberately absent: it counts every mapping transition in the
/// space, while the range version already moves for exactly the transitions that
/// can change the bytes at this PC.
pub(super) type NativeSite = (hl_task::ProcessId, u64, u64, u64);
pub(super) type NativeSource = (hl_task::ProcessId, u64, u64, u64);

/// Refusals a fresh latch serves before it lets the entry back in.
pub(super) const SUPPRESSION_SPAN: u64 = 64;

/// How much longer one suppressed entry stays refused, and whether any refusal expires
/// the latch at all.
///
/// A refusal count alone cannot tell the two populations apart, which is what sank the
/// earlier bounded-suppression attempts. On malloc and sqlite the latched entry has a
/// history of long native runs and the ban is an anomaly worth expiring; on the
/// exit-heavy phases the run genuinely retires little *every* time, so expiring on a
/// schedule buys a re-entry that fails, re-latches, and pays a rebuild. What separates
/// them is not how often the entry has been refused but whether it has ever been
/// productive, so every entry gets exactly one probationary span and only an entry that
/// has cleared the bar at least once keeps earning more.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Probation {
    pub(super) remaining: u64,
    pub(super) permanent: bool,
}

/// The previous admission of this pool, reused when the next one repeats it exactly.
/// `version` is the executable-range token version, so a peer rewriting the code moves
/// the key and the cached bytes are never served for code that has changed.
pub(super) struct Admission {
    pub(super) site: NativeSite,
    pub(super) length: usize,
    pub(super) epoch: u64,
    pub(super) bytes: [u8; 256],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SourceChange {
    Stable,
    Changed,
    Disabled,
}

pub(super) struct NativeExecutors {
    pub(super) resolver: crate::native::NativeExecutor,
    pub(super) direct: Option<crate::native::NativeExecutor>,
    direct_unavailable: bool,
}

impl NativeExecutors {
    pub(super) fn cache_resets(&self) -> u64 {
        self.resolver.cache_resets().saturating_add(
            self.direct
                .as_ref()
                .map_or(0, crate::native::NativeExecutor::cache_resets),
        )
    }
}

pub(in crate::ffi::linux::execution) struct NativePool {
    pub(super) enabled: bool,
    /// Gates the opt-in profiling tables and the boundary ring, whose recording costs
    /// memory on the native path; failures are reported through `hl_log` regardless.
    pub(super) diagnostics: bool,
    /// Gates the repeat-admission cache below.
    pub(super) admission_cache: bool,
    /// Gates the sticky arm of the direct-authority decision below.
    pub(super) direct_sticky: bool,
    /// The sticky arm's flip budget. Invalid or zero experimental values preserve the default.
    pub(super) direct_sticky_limit: u32,
    /// Resolver admissions a bounded direct hold must serve before authority may warm again.
    pub(super) direct_hold_runs: u64,
    /// Makes the sticky arm's hold permanent instead of bounded.
    pub(super) direct_sticky_permanent: bool,
    /// False drops the aarch64 exact write journal; every crossing then publishes
    /// its whole window instead of the exact ranges the journal would have carried.
    write_reserve: bool,
    write_commit: bool,
    /// Emits the reservation everywhere but gates it at run time on a per-store-site
    /// saturation byte, so translation stays a pure function of the pc.
    runtime_write_reserve: bool,
    /// Keeps `AArch64` native execution running after the exact dirty journal fills.
    pub(super) dirty_overflow_continue: bool,
    /// Keeps resolver and direct translations in separate AArch64 caches. The direct
    /// executor is allocated only after the process earns and can use direct authority.
    pub(super) split_mode_executors: bool,
    pub(super) admitted: Option<Admission>,
    pub(super) executors: BTreeMap<hl_task::ProcessId, NativeExecutors>,
    pub(super) suppressed: BTreeMap<NativeSite, Probation>,
    /// Entries that have completed a native run retiring a substantial share of their
    /// budget at least once. Only these have shown there is native work here to lose,
    /// so only these earn a second probationary span after a latch.
    pub(super) productive: BTreeSet<NativeSite>,
    /// Diagnostics-only: every entry PC ever marked productive, never swept. Answers
    /// whether an empty live table means "no productive history" or "history discarded".
    pub(super) ever_productive: BTreeSet<u64>,
    /// The most any single run of this entry has ever retired, recorded on every run
    /// whatever its exit. The productive table records only clean exits, so this is the
    /// only record of a long run that ended in a guard fault.
    pub(super) high_water: BTreeMap<NativeSite, u64>,
    /// Diagnostics-only, never swept: the per-PC high water and the PCs a permanent latch
    /// condemned, so the refusal table can be read against what the entry had shown.
    pub(super) high_water_pc: BTreeMap<u64, u64>,
    pub(super) permanent_pcs: BTreeSet<u64>,
    /// Set once the productive table has refused an insertion at its cap. From then on no
    /// latch is made permanent, because absence from the table no longer means anything.
    pub(super) productive_saturated: bool,
    /// Entries whose only failure so far was a memory access under direct authority, which
    /// runs without the operand resolver and so cannot recover an access outside its region.
    pub(super) direct_declined: BTreeSet<NativeSite>,
    pub(super) fallbacks: BTreeSet<NativeSite>,
    pub(super) observations: BTreeMap<NativeSite, u8>,
    /// Diagnostics-only: how often each guest PC forced a fallback, and how
    /// often each entry PC was refused because a previous fallback suppressed it.
    pub(super) fallback_weight: BTreeMap<u64, u64>,
    pub(super) suppressed_weight: BTreeMap<u64, u64>,
    pub(super) sources: BTreeMap<NativeSource, hl_memory::ExecutableToken>,
    pub(super) source_incarnations: BTreeMap<hl_task::ProcessId, u64>,
    pub(super) instruction_epochs: BTreeMap<hl_task::ProcessId, u64>,
    /// Last run mode and the leaky score of how much that mode has been alternating.
    pub(super) direct_modes: BTreeMap<hl_task::ProcessId, (bool, u32)>,
    /// Runs still owed before a thrashing process may be offered direct authority again.
    pub(super) direct_holds: BTreeMap<hl_task::ProcessId, u64>,
    /// Processes that armed an interval or POSIX timer, whose expiry is only observed
    /// on a scheduler round trip. A merely pending signal is not recorded here: it is
    /// already read live through the run's interrupt flag.
    pub(super) boundary_sensitive: BTreeSet<hl_task::ProcessId>,
    pub(super) boundaries: Option<NativeBoundaries>,
    pub(super) counters: NativeCounters,
    pub(super) host_faults: Option<Arc<dyn crate::native::HostFaultOwner>>,
    /// Live process set the last retain sweep ran against, so an unchanged set skips it.
    swept: Option<BTreeSet<hl_task::ProcessId>>,
}

impl NativePool {
    pub(super) fn selected(isa: GuestIsa, plan: &RuntimeLaunchPlan) -> bool {
        cfg!(target_arch = "aarch64")
            && matches!(isa, GuestIsa::Aarch64 | GuestIsa::X86_64)
            && plan.options.get("HL_NATIVE_EXECUTION") == Some("1")
    }

    pub(in crate::ffi::linux::execution) fn new(
        isa: GuestIsa,
        plan: &RuntimeLaunchPlan,
        host_faults: Option<Arc<dyn crate::native::HostFaultOwner>>,
    ) -> Self {
        let pool = Self {
            enabled: Self::selected(isa, plan),
            diagnostics: plan.options.get("HL_NATIVE_DIAGNOSTICS") == Some("1"),
            admission_cache: plan.options.get("HL_NATIVE_ADMISSION_CACHE") == Some("1"),
            direct_sticky: plan.options.get("HL_NATIVE_DIRECT_STICKY") == Some("1"),
            direct_sticky_limit: plan
                .options
                .get("HL_NATIVE_DIRECT_STICKY_LIMIT")
                .and_then(|value| value.parse::<u32>().ok())
                .filter(|limit| *limit > 0)
                .unwrap_or(DIRECT_STICKY_FLIP_LIMIT),
            direct_hold_runs: plan
                .options
                .integer("HL_NATIVE_DIRECT_HOLD_RUNS")
                .ok()
                .flatten()
                .filter(|runs| *runs > 0 && *runs < DIRECT_HOLD_PERMANENT)
                .unwrap_or(DIRECT_HOLD_RUNS),
            direct_sticky_permanent: plan.options.get("HL_NATIVE_DIRECT_STICKY_PERMANENT") == Some("1"),
            write_reserve: plan.options.get("HL_A64_NO_WRITE_RESERVE") != Some("1"),
            write_commit: plan.options.get("HL_A64_NO_WRITE_COMMIT") != Some("1"),
            runtime_write_reserve: plan.options.get("HL_A64_RUNTIME_WRITE_RESERVE") == Some("1"),
            dirty_overflow_continue: plan.options.get("HL_A64_DIRTY_OVERFLOW_CONTINUE") == Some("1"),
            split_mode_executors: isa == GuestIsa::Aarch64
                && plan.options.get("HL_NATIVE_SPLIT_MODE_EXECUTORS") == Some("1"),
            admitted: None,
            executors: BTreeMap::new(),
            suppressed: BTreeMap::new(),
            productive: BTreeSet::new(),
            ever_productive: BTreeSet::new(),
            high_water: BTreeMap::new(),
            high_water_pc: BTreeMap::new(),
            permanent_pcs: BTreeSet::new(),
            productive_saturated: false,
            direct_declined: BTreeSet::new(),
            fallbacks: BTreeSet::new(),
            observations: BTreeMap::new(),
            fallback_weight: BTreeMap::new(),
            suppressed_weight: BTreeMap::new(),
            sources: BTreeMap::new(),
            source_incarnations: BTreeMap::new(),
            instruction_epochs: BTreeMap::new(),
            direct_modes: BTreeMap::new(),
            direct_holds: BTreeMap::new(),
            boundary_sensitive: BTreeSet::new(),
            boundaries: (plan.options.get("HL_NATIVE_DIAGNOSTICS") == Some("1")).then(NativeBoundaries::new),
            counters: NativeCounters::default(),
            host_faults,
            swept: None,
        };
        // Which options the pool actually accepted, named once at construction. Every
        // `HL_NATIVE_*` option is a silent comparison against `Some("1")`: a misspelled
        // name, a value of `true`, or an option the plan dropped all read as "off" while
        // every phase still reports plausible timings. This is the one place that can say
        // what was believed, so it says all of it rather than only the odd ones.
        hl_log::hl_event!(
            hl_log::tag::EXEC,
            hl_log::Level::Info,
            "native.pool.configured",
            enabled = pool.enabled,
            isa = ?isa,
            diagnostics = pool.diagnostics,
            admission_cache = pool.admission_cache,
            direct_sticky = pool.direct_sticky,
            direct_sticky_limit = pool.direct_sticky_limit,
            direct_hold_runs = pool.direct_hold_runs,
            direct_sticky_permanent = pool.direct_sticky_permanent,
            direct_mode_holds = pool.direct_mode_holds_enabled(),
            write_reserve = pool.write_reserve,
            write_commit = pool.write_commit,
            runtime_write_reserve = pool.runtime_write_reserve,
            dirty_overflow_continue = pool.dirty_overflow_continue,
            split_mode_executors = pool.split_mode_executors
        );
        pool
    }

    pub(super) fn production_faults(
        selected: Option<Arc<dyn crate::native::HostFaultOwner>>,
    ) -> Option<Arc<dyn crate::native::HostFaultOwner>> {
        if selected.is_some() {
            return selected;
        }
        #[cfg(target_os = "linux")]
        return crate::native::NativeFaultOwner::create().ok();
        #[cfg(not(target_os = "linux"))]
        None
    }

    fn create_executor(&self) -> Option<crate::native::NativeExecutor> {
        crate::native::NativeExecutor::create_with_journal(
            self.diagnostics,
            self.write_reserve,
            self.write_commit,
            self.runtime_write_reserve,
            self.dirty_overflow_continue,
            self.host_faults.clone(),
        )
        .ok()
    }

    fn ensure_executor(&mut self, process: hl_task::ProcessId) -> Option<&mut NativeExecutors> {
        if !self.enabled {
            return None;
        }
        if !self.executors.contains_key(&process) {
            let resolver = self.create_executor()?;
            self.executors.insert(
                process,
                NativeExecutors {
                    resolver,
                    direct: None,
                    direct_unavailable: false,
                },
            );
        }
        self.executors.get_mut(&process)
    }

    pub(super) fn executor(&mut self, process: hl_task::ProcessId) -> Option<&crate::native::NativeExecutor> {
        Some(&self.ensure_executor(process)?.resolver)
    }

    /// Selects the AArch64 cache for the exact mode `run_lease` will use. The
    /// default and resolver paths retain the original single executor; only the
    /// opt-in direct path creates a sibling, and allocation failure safely falls
    /// back to resolver mode for the process.
    pub(super) fn aarch64_executor(
        &mut self,
        process: hl_task::ProcessId,
        direct: bool,
    ) -> Option<(&crate::native::NativeExecutor, bool)> {
        if !self.enabled {
            return None;
        }
        let diagnostics = self.diagnostics;
        let write_reserve = self.write_reserve;
        let write_commit = self.write_commit;
        let runtime_write_reserve = self.runtime_write_reserve;
        let dirty_overflow_continue = self.dirty_overflow_continue;
        let host_faults = self.host_faults.clone();
        let create_executor = || {
            crate::native::NativeExecutor::create_with_journal(
                diagnostics,
                write_reserve,
                write_commit,
                runtime_write_reserve,
                dirty_overflow_continue,
                host_faults.clone(),
            )
            .ok()
        };
        let executors = match self.executors.entry(process) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => entry.insert(NativeExecutors {
                resolver: create_executor()?,
                direct: None,
                direct_unavailable: false,
            }),
        };
        if self.split_mode_executors && direct && executors.direct.is_none() && !executors.direct_unavailable {
            match create_executor() {
                Some(executor) => {
                    executors.direct = Some(executor);
                    self.counters.direct_executors_created += 1;
                }
                None => executors.direct_unavailable = true,
            }
        }
        Some(match executors.direct.as_ref() {
            Some(executor) if self.split_mode_executors && direct => (executor, true),
            _ => (&executors.resolver, direct && !self.split_mode_executors),
        })
    }

    fn reset_executors(&mut self, process: hl_task::ProcessId, mapping_epoch: u64) -> Option<()> {
        let reset = {
            let executors = self.ensure_executor(process)?;
            executors.resolver.reset(mapping_epoch).is_ok()
                && executors
                    .direct
                    .as_ref()
                    .is_none_or(|executor| executor.reset(mapping_epoch).is_ok())
        };
        if !reset {
            // Do not retain a half-reset pair: either cache could otherwise certify an
            // instruction generation that its sibling has already discarded.
            self.executors.remove(&process);
            return None;
        }
        Some(())
    }

    pub(super) fn invalidate_executors(
        &mut self,
        process: hl_task::ProcessId,
        ranges: &[(u64, u64)],
        mapping_incarnation: u64,
    ) -> Option<()> {
        let invalidated = {
            let executors = self.ensure_executor(process)?;
            executors
                .resolver
                .invalidate_ranges(ranges, mapping_incarnation)
                .is_ok()
                && executors
                    .direct
                    .as_ref()
                    .is_none_or(|executor| executor.invalidate_ranges(ranges, mapping_incarnation).is_ok())
        };
        if !invalidated {
            self.executors.remove(&process);
            return None;
        }
        Some(())
    }

    /// Whether this process may still be offered direct authority. A held process is
    /// serving out a hold because its run mode was alternating; each call spends one run.
    pub(super) fn direct_admitted(&mut self, process: hl_task::ProcessId) -> bool {
        if !self.direct_mode_holds_enabled() {
            return true;
        }
        let Some(remaining) = self.direct_holds.get_mut(&process) else {
            return true;
        };
        if *remaining == DIRECT_HOLD_PERMANENT {
            return false;
        }
        *remaining = remaining.saturating_sub(1);
        if *remaining != 0 {
            return false;
        }
        self.direct_holds.remove(&process);
        self.direct_modes.remove(&process);
        true
    }

    /// Whether this process has earned direct authority. Direct mode carries no operand
    /// resolver, so an access outside the entry window ends the run; on a process's first
    /// entry neither `direct_holds` nor `direct_declined` holds anything that could decline
    /// it, and a short program never gets the second entry the decline would protect. Spend
    /// one resolver run first, which is what `direct_modes` records.
    pub(super) fn direct_earned(&mut self, process: hl_task::ProcessId) -> bool {
        // `direct_admitted` spends a hold run, so it must run before the warm-up test:
        // a held process has no `direct_modes` entry and would otherwise never serve out
        // its hold.
        self.direct_admitted(process) && self.direct_modes.contains_key(&process)
    }

    /// Split caches remove the reason for the default anti-flip hold, but an explicit
    /// sticky policy remains an operator decision and must not be silently overridden.
    pub(super) const fn direct_mode_holds_enabled(&self) -> bool {
        !self.split_mode_executors || self.direct_sticky || self.direct_sticky_permanent
    }

    /// Applies every direct-authority gate and classifies the result when diagnostics are enabled.
    /// Keeping the non-diagnostic expression intact preserves the timing binary's hot-path shape.
    pub(super) fn direct_authority(&mut self, process: hl_task::ProcessId, entry: NativeSite) -> bool {
        if !self.diagnostics {
            return self.direct_earned(process) && !self.direct_declined.contains(&entry);
        }
        let admitted = self.direct_admitted(process);
        let warm = self.direct_modes.contains_key(&process);
        let site = !self.direct_declined.contains(&entry);
        if !admitted {
            self.counters.direct_held += 1;
        } else if !warm {
            self.counters.direct_cold += 1;
        } else if !site {
            self.counters.direct_site += 1;
        } else {
            self.counters.direct_admitted += 1;
        }
        admitted && warm && site
    }

    /// Records the run mode this process just used. A shared cache carries the mode in its
    /// identity, so sustained alternation resets every translation twice per cycle; split
    /// caches retain the logical observation without installing the default hold.
    pub(super) fn observe_direct_mode(&mut self, process: hl_task::ProcessId, direct: bool) {
        if !self.direct_mode_holds_enabled() {
            let flipped = self
                .direct_modes
                .insert(process, (direct, 0))
                .is_some_and(|(previous, _)| previous != direct);
            if self.diagnostics && flipped {
                self.counters.direct_flips += 1;
            }
            return;
        }
        let sticky = self.direct_sticky || self.direct_sticky_permanent;
        let permanent = self.direct_sticky_permanent;
        let (previous, flips) = self.direct_modes.entry(process).or_insert((direct, 0));
        if *previous == direct {
            // A steady run pays down the score, so an isolated flip never accumulates.
            // The sticky arm keeps the score, because a steady run is worth far less than
            // a flip costs and a slow alternation otherwise never reaches the limit.
            if !sticky {
                *flips = flips.saturating_sub(1);
            }
            return;
        }
        *previous = direct;
        *flips = flips.saturating_add(2);
        let flips = *flips;
        let diagnostics = self.diagnostics;
        if diagnostics {
            self.counters.direct_flips += 1;
        }
        let (limit, hold) = if sticky {
            (
                self.direct_sticky_limit,
                if permanent {
                    DIRECT_HOLD_PERMANENT
                } else {
                    self.direct_hold_runs
                },
            )
        } else {
            (DIRECT_FLIP_LIMIT, self.direct_hold_runs)
        };
        if flips >= limit {
            self.direct_modes.remove(&process);
            self.direct_holds.insert(process, hold);
            if diagnostics {
                self.counters.direct_holds += 1;
            }
            // The decision, not the flip: this process is off direct authority, and under
            // the permanent arm it never comes back. Bounded by the live process set.
            hl_log::hl_event!(
                hl_log::tag::EXEC,
                hl_log::Level::Debug,
                "native.direct.held",
                process = process.number(),
                flips = flips,
                limit = limit,
                permanent = hold == DIRECT_HOLD_PERMANENT
            );
        }
    }

    pub(super) fn bounded_direct_hold_remaining(&self) -> u64 {
        self.direct_holds
            .values()
            .copied()
            .filter(|remaining| *remaining != DIRECT_HOLD_PERMANENT)
            .fold(0, u64::saturating_add)
    }

    /// Records that this entry completed a run retiring a substantial share of its budget,
    /// which is the evidence that a later short run is an anomaly rather than its nature.
    pub(super) fn mark_productive(&mut self, entry: NativeSite, executed: u64, budget: u64) {
        if self.diagnostics {
            self.record_high_water(entry, executed);
        }
        if Self::fallback_suppresses(executed, budget) || self.productive.contains(&entry) {
            return;
        }
        if self.productive.len() >= NATIVE_SITE_LIMIT {
            // The table is the only evidence that an entry has native work to lose, so a
            // full table is missing evidence, not evidence of absence. Every other capped
            // set here fails towards *not* suppressing; this one would fail towards a
            // permanent latch, so record the saturation and stop making latches permanent
            // instead of condemning whichever entries lost the insertion race.
            self.productive_saturated = true;
            return;
        }
        self.productive.insert(entry);
        self.counters.productive_marks += 1;
        if self.diagnostics && self.ever_productive.len() < NATIVE_SITE_LIMIT {
            self.ever_productive.insert(entry.3);
        }
    }

    /// The most this entry has retired in any one run, recorded whatever the exit, and only
    /// under diagnostics: this is read to say whether the permanence rule's premise held,
    /// not to decide anything, and `mark_productive` runs on every clean native exit.
    fn record_high_water(&mut self, entry: NativeSite, executed: u64) {
        if let Some(reached) = self.high_water.get_mut(&entry) {
            *reached = (*reached).max(executed);
        } else if self.high_water.len() < NATIVE_SITE_LIMIT {
            self.high_water.insert(entry, executed);
        }
        if self.diagnostics && self.high_water_pc.len() < NATIVE_SITE_LIMIT {
            let reached = self.high_water_pc.entry(entry.3).or_default();
            *reached = (*reached).max(executed);
        }
    }

    /// Spends one refusal of this entry's latch, reporting whether native entry is still
    /// refused. The refusal that exhausts the span is the last one: the next probe enters
    /// natively, and only another short fallback re-arms the latch.
    pub(super) fn refuses(&mut self, entry: NativeSite) -> bool {
        let Some(probation) = self.suppressed.get_mut(&entry) else {
            return false;
        };
        if probation.permanent {
            return true;
        }
        if probation.remaining == 0 {
            return false;
        }
        probation.remaining -= 1;
        if probation.remaining == 0 {
            self.counters.suppress_clears += 1;
        }
        true
    }

    /// Suppression is reserved for entries whose run retired too little to be worth
    /// re-entering, so a run that did substantial native work keeps its entry. A first
    /// memory fallback under direct authority buys one retry without it instead, because
    /// that run mode has no operand resolver and so cannot be the entry's verdict.
    ///
    /// Every arm64 fallback on malloc, syscall, file, mmap and sqlite is a guard fault on an
    /// operand address, never an untranslatable instruction: `a64_fallback_form_memory` equals
    /// `a64_fallback_generated` equals `guard_read + guard_write`, and
    /// `a64_fallback_entry_rejection` is zero on all five. The failing PCs disassemble as plain
    /// loads that pass their guard millions of times each, so there is no bad instruction for a
    /// shorter block to cut before; the address coverage of the operand views is what fails.
    pub(super) fn record_fallback(
        &mut self,
        entry: NativeSite,
        instruction: NativeSite,
        executed: u64,
        budget: u64,
        direct_guard: bool,
    ) {
        if self.diagnostics {
            *self.fallback_weight.entry(instruction.3).or_default() += 1;
        }
        if self.diagnostics {
            self.record_high_water(entry, executed);
        }
        if Self::fallback_suppresses(executed, budget) {
            self.counters.suppress_verdicts += 1;
            if direct_guard && !self.direct_declined.contains(&entry) && self.direct_declined.len() < NATIVE_SITE_LIMIT
            {
                self.counters.suppress_deferred += 1;
                self.direct_declined.insert(entry);
            } else if let Some(probation) = self.suppressed.get_mut(&entry) {
                if self.productive.contains(&entry) {
                    // This entry has retired real native work before, so a short run is an
                    // anomaly. Doubling bounds it at O(log refusals) retries.
                    probation.remaining = SUPPRESSION_SPAN;
                    self.counters.suppress_rearms += 1;
                } else if self.productive_saturated {
                    // Absence from a saturated table is not evidence, so keep expiring.
                    probation.remaining = SUPPRESSION_SPAN;
                    self.counters.suppress_rearms += 1;
                } else {
                    // It has now fallen short on every run it has ever had, including the
                    // one the probationary span bought it. There is no native work here to
                    // recover, so stop paying a re-entry and a rebuild to rediscover that.
                    probation.permanent = true;
                    probation.remaining = 0;
                    self.counters.suppress_permanent += 1;
                    if self.ever_productive.contains(&entry.3) {
                        self.counters.permanent_after_productive += 1;
                    }
                    if self.diagnostics && self.permanent_pcs.len() < NATIVE_SITE_LIMIT {
                        self.permanent_pcs.insert(entry.3);
                    }
                    let reached = self.high_water.get(&entry).copied().unwrap_or_default();
                    let bar = budget.min(super::SLICE_BUDGET);
                    if reached * 2 >= bar {
                        self.counters.permanent_high_water_half += 1;
                    }
                    if reached * 4 >= bar {
                        self.counters.permanent_high_water_quarter += 1;
                    }
                    if reached * 8 >= bar {
                        self.counters.permanent_high_water_eighth += 1;
                    }
                    // The terminal decision for this entry: it will never be entered
                    // natively again. The teardown counter says how many latched, never
                    // which entry or when, which is the question a lane actually has.
                    hl_log::hl_event!(
                        hl_log::tag::EXEC,
                        hl_log::Level::Debug,
                        "native.suppress.permanent",
                        process = entry.0.number(),
                        entry = entry.3,
                        instruction = instruction.3,
                        executed = executed,
                        budget = budget
                    );
                }
            } else if self.suppressed.len() < NATIVE_SITE_LIMIT {
                self.suppressed.insert(
                    entry,
                    Probation {
                        remaining: SUPPRESSION_SPAN,
                        permanent: false,
                    },
                );
                self.counters.suppress_latches += 1;
                if self.diagnostics {
                    // One line per latch rather than per refusal, so the site limit bounds the output.
                    eprintln!(
                        "hl-native-suppress-cause: entry={:#x} instruction={:#x} executed={executed} budget={budget}",
                        entry.3, instruction.3,
                    );
                }
            }
        }
        if self.fallbacks.len() < NATIVE_SITE_LIMIT {
            self.fallbacks.insert(instruction);
        }
    }

    /// Declining an entry hands the turn a whole `SLICE_BUDGET` interpreter slice, while a
    /// fallback yields only what the run retired plus one interpreted instruction, so a
    /// retry only pays for itself once it retires a substantial part of its budget. The
    /// slice is what a decline actually buys, so a larger native budget cannot raise the
    /// bar the retry has to clear.
    ///
    /// This weighs one retry against one slice from a single observation, which is why the
    /// latch serves a bounded span rather than the life of the process: one short run is
    /// not always the entry's fault. Under a SIGURG storm a signal invalidates the operand
    /// projection and the next run guard-faults after a few hundred instructions, so a
    /// permanent latch condemned the hottest loop in the guest to the interpreter on the
    /// strength of one interrupted run.
    pub(super) const fn fallback_suppresses(executed: u64, budget: u64) -> bool {
        executed * 2
            < if budget < super::SLICE_BUDGET {
                budget
            } else {
                super::SLICE_BUDGET
            }
    }

    /// The observation table is a warm-up heuristic keyed by image incarnation,
    /// so an exec fills it with keys no lookup can match again. Reclaim those
    /// instead of giving up native execution for the whole process.
    pub(super) fn observe(&mut self, key: NativeSite) -> u8 {
        let (process, lease, _, _) = key;
        if !self.observations.contains_key(&key) && self.observations.len() >= NATIVE_SITE_LIMIT {
            self.observations
                .retain(|(owner, held, _, _), _| *owner != process || *held == lease);
            if self.observations.len() >= NATIVE_SITE_LIMIT {
                self.observations.clear();
            }
        }
        let observations = self.observations.entry(key).or_default();
        *observations = observations.saturating_add(1);
        *observations
    }

    /// Copies the previous admission's code bytes when this admission repeats it exactly:
    /// same process, image incarnation, executable-range version, entry PC, length and
    /// instruction epoch. Under that key the warm-up gates have already admitted, the
    /// guest code cannot have changed, and `prepare_source` would take no action, so the
    /// caller may skip all three. The projection is deliberately not cached.
    pub(super) fn admitted_bytes(&self, site: NativeSite, length: usize, epoch: u64, into: &mut [u8]) -> bool {
        self.admission_cache
            && self.admitted.as_ref().is_some_and(|admitted| {
                admitted.site == site && admitted.length == length && admitted.epoch == epoch && {
                    into.copy_from_slice(&admitted.bytes[..length]);
                    true
                }
            })
    }

    pub(super) fn record_admission(&mut self, site: NativeSite, length: usize, epoch: u64, bytes: &[u8]) {
        if !self.admission_cache || length > 256 {
            return;
        }
        let mut held = [0_u8; 256];
        held[..length].copy_from_slice(bytes);
        self.admitted = Some(Admission {
            site,
            length,
            epoch,
            bytes: held,
        });
    }

    pub(super) fn disable(&mut self) {
        if self.enabled {
            // Native execution stops for the rest of the process and nothing else says so:
            // the counters at teardown show probes that never became entries, which reads
            // identically to a guest that simply never qualified. Not tag-gated, because
            // the operator who did not open `exec` is exactly the one measuring a run that
            // is quietly interpreted.
            hl_log::hl_verdict!(
                hl_log::tag::EXEC,
                "native.disabled",
                reason = "source_table_full",
                sources = self.sources.len(),
                executors = self.executors.len();
                "native execution disabled for the rest of this process: the source table filled"
            );
        }
        self.enabled = false;
        self.admitted = None;
        self.executors.clear();
        self.sources.clear();
        self.source_incarnations.clear();
        self.instruction_epochs.clear();
        self.observations.clear();
        self.suppressed.clear();
        self.counters.productive_swept += self.productive.len() as u64;
        self.productive.clear();
        self.high_water.clear();
        self.productive_saturated = false;
        self.direct_declined.clear();
        self.fallbacks.clear();
        self.direct_modes.clear();
        self.direct_holds.clear();
    }

    pub(super) fn reset_observed_sources(&mut self, process: hl_task::ProcessId, incarnation: u64) -> Option<()> {
        // A reset throws away every translation this process owns, so a run that resets
        // repeatedly rebuilds continuously and reports as merely slow. Debug, tag-gated,
        // and compiled out of release: this is off unless someone is asking the question.
        hl_log::hl_event!(
            hl_log::tag::EXEC,
            hl_log::Level::Debug,
            "native.cache.reset",
            process = process.number(),
            incarnation = incarnation,
            sources = self.sources.len(),
            suppressed = self.suppressed.len()
        );
        self.admitted = None;
        self.reset_executors(process, incarnation)?;
        self.sources.retain(|(owner, _, _, _), _| *owner != process);
        self.observations.retain(|(owner, _, _, _), _| *owner != process);
        let held = self.suppressed.len();
        self.suppressed.retain(|(owner, _, _, _), _| *owner != process);
        self.counters.suppress_clears += (held - self.suppressed.len()) as u64;
        let productive_held = self.productive.len();
        self.productive.retain(|(owner, _, _, _)| *owner != process);
        self.high_water.retain(|(owner, _, _, _), _| *owner != process);
        self.counters.productive_swept += (productive_held - self.productive.len()) as u64;
        self.direct_declined.retain(|(owner, _, _, _)| *owner != process);
        self.fallbacks.retain(|(owner, _, _, _)| *owner != process);
        Some(())
    }

    pub(super) fn purge_process_metadata(&mut self, process: hl_task::ProcessId) {
        self.admitted = None;
        self.sources.retain(|(owner, _, _, _), _| *owner != process);
        self.observations.retain(|(owner, _, _, _), _| *owner != process);
        let held = self.suppressed.len();
        self.suppressed.retain(|(owner, _, _, _), _| *owner != process);
        self.counters.suppress_clears += (held - self.suppressed.len()) as u64;
        let productive_held = self.productive.len();
        self.productive.retain(|(owner, _, _, _)| *owner != process);
        self.high_water.retain(|(owner, _, _, _), _| *owner != process);
        self.counters.productive_swept += (productive_held - self.productive.len()) as u64;
        self.direct_declined.retain(|(owner, _, _, _)| *owner != process);
        self.fallbacks.retain(|(owner, _, _, _)| *owner != process);
        self.source_incarnations.remove(&process);
        self.instruction_epochs.remove(&process);
        self.direct_modes.remove(&process);
        self.direct_holds.remove(&process);
        self.boundary_sensitive.remove(&process);
    }

    pub(super) fn reset_process(&mut self, process: hl_task::ProcessId) -> Option<()> {
        self.reset_executors(process, 0)?;
        self.purge_process_metadata(process);
        Some(())
    }

    /// True when some per-process table holds an entry, so the caller can skip both
    /// the retain sweep and the live-process set it would need to build.
    pub(super) fn tracks_processes(&self) -> bool {
        !self.executors.is_empty()
            || !self.sources.is_empty()
            || !self.observations.is_empty()
            || !self.suppressed.is_empty()
            || !self.productive.is_empty()
            || !self.direct_declined.is_empty()
            || !self.fallbacks.is_empty()
            || !self.source_incarnations.is_empty()
            || !self.instruction_epochs.is_empty()
            || !self.direct_modes.is_empty()
            || !self.direct_holds.is_empty()
            || !self.boundary_sensitive.is_empty()
    }

    pub(super) fn retain_processes(&mut self, live: &BTreeSet<hl_task::ProcessId>) {
        // The sweep only drops entries keyed by a process outside `live`, and every
        // insertion between turns is keyed by a process the same set already holds,
        // so repeating it for an unchanged live set can never drop anything.
        if self.swept.as_ref().is_some_and(|swept| swept == live) {
            return;
        }
        self.swept = Some(live.clone());
        if !self.executors.is_empty() {
            self.executors.retain(|process, _| live.contains(process));
        }
        if !self.sources.is_empty() {
            self.sources.retain(|(process, _, _, _), _| live.contains(process));
        }
        if !self.observations.is_empty() {
            self.observations.retain(|(process, _, _, _), _| live.contains(process));
        }
        if !self.suppressed.is_empty() {
            let held = self.suppressed.len();
            self.suppressed.retain(|(process, _, _, _), _| live.contains(process));
            self.counters.suppress_clears += (held - self.suppressed.len()) as u64;
        }
        if !self.productive.is_empty() {
            let productive_held = self.productive.len();
            self.productive.retain(|(process, _, _, _)| live.contains(process));
            self.high_water.retain(|(process, _, _, _), _| live.contains(process));
            self.counters.productive_swept += (productive_held - self.productive.len()) as u64;
        }
        if !self.direct_declined.is_empty() {
            self.direct_declined.retain(|(process, _, _, _)| live.contains(process));
        }
        if !self.fallbacks.is_empty() {
            self.fallbacks.retain(|(process, _, _, _)| live.contains(process));
        }
        if !self.source_incarnations.is_empty() {
            self.source_incarnations.retain(|process, _| live.contains(process));
        }
        if !self.instruction_epochs.is_empty() {
            self.instruction_epochs.retain(|process, _| live.contains(process));
        }
        if !self.direct_modes.is_empty() {
            self.direct_modes.retain(|process, _| live.contains(process));
        }
        if !self.direct_holds.is_empty() {
            self.direct_holds.retain(|process, _| live.contains(process));
        }
        if !self.boundary_sensitive.is_empty() {
            self.boundary_sensitive.retain(|process| live.contains(process));
        }
    }

    pub(super) fn merge_observed_sources(
        &mut self,
        process: hl_task::ProcessId,
        incarnation: u64,
        observed: Vec<(u64, u64, hl_memory::ExecutableToken)>,
        complete: bool,
    ) -> Option<()> {
        let conflict = observed.iter().any(|(first, last, token)| {
            token.incarnation != incarnation
                || last <= first
                || self
                    .sources
                    .get(&(process, incarnation, *first, *last))
                    .is_some_and(|existing| existing != token)
        });
        let additions = observed
            .iter()
            .filter(|(first, last, _)| !self.sources.contains_key(&(process, incarnation, *first, *last)))
            .count();
        if !complete || conflict || self.sources.len().saturating_add(additions) > NATIVE_SOURCE_LIMIT {
            self.reset_observed_sources(process, incarnation)?;
        }
        for (first, last, token) in observed {
            if token.incarnation != incarnation || last <= first {
                continue;
            }
            self.sources.insert((process, incarnation, first, last), token);
        }
        Some(())
    }

    pub(super) fn track_source(
        &mut self,
        process: hl_task::ProcessId,
        first: u64,
        last: u64,
        token: hl_memory::ExecutableToken,
        limit: usize,
    ) -> SourceChange {
        if self
            .source_incarnations
            .insert(process, token.incarnation)
            .is_some_and(|previous| previous != token.incarnation)
        {
            self.sources.retain(|(owner, _, _, _), _| *owner != process);
        }
        let key = (process, token.incarnation, first, last);
        if let Some(previous) = self.sources.get_mut(&key) {
            let changed = *previous != token;
            *previous = token;
            return if changed {
                SourceChange::Changed
            } else {
                SourceChange::Stable
            };
        }
        if self.sources.len() >= limit {
            self.disable();
            return SourceChange::Disabled;
        }
        self.sources.insert(key, token);
        SourceChange::Stable
    }

    /// Re-reads every source this incarnation observed and invalidates the ranges whose backing
    /// token moved, because a new instruction epoch may have remapped code underneath them.
    ///
    /// Three situations collapse to the same conservative answer — drop everything observed and
    /// start again: a population too large to walk, a source the caller can no longer resolve,
    /// and a source that now belongs to a different incarnation.
    fn rescan_sources<F>(&mut self, process: hl_task::ProcessId, incarnation: u64, current_token: &mut F) -> Option<()>
    where
        F: FnMut(u64, u64) -> Option<hl_memory::ExecutableToken>,
    {
        let observed: Vec<_> = self
            .sources
            .iter()
            .filter(|((owner, source_incarnation, _, _), _)| *owner == process && *source_incarnation == incarnation)
            .map(|(key @ (_, _, first, last), previous)| (*key, *previous, *first, *last))
            .collect();
        if observed.len() > SOURCE_RESCAN_LIMIT {
            return self.reset_observed_sources(process, incarnation);
        }
        let mut changed = Vec::new();
        let mut refreshed = Vec::new();
        let mut gap = false;
        for (key, previous, source_first, source_last) in observed {
            let Some(current) = current_token(source_first, source_last) else {
                gap = true;
                break;
            };
            if current.incarnation != incarnation {
                gap = true;
                break;
            }
            if current != previous {
                changed.push((source_first, source_last));
            }
            refreshed.push((key, current));
        }
        if gap {
            return self.reset_observed_sources(process, incarnation);
        }
        if !changed.is_empty() && self.invalidate_executors(process, &changed, incarnation).is_none() {
            self.reset_observed_sources(process, incarnation)?;
            refreshed.clear();
        }
        for (key, current) in refreshed {
            self.sources.insert(key, current);
        }
        Some(())
    }

    pub(super) fn prepare_source<F>(
        &mut self,
        process: hl_task::ProcessId,
        first: u64,
        last: u64,
        token: hl_memory::ExecutableToken,
        instruction_epoch: u64,
        mut current_token: F,
    ) -> Option<()>
    where
        F: FnMut(u64, u64) -> Option<hl_memory::ExecutableToken>,
    {
        let published = self.instruction_epochs.get(&process).copied();
        if published.is_some_and(|previous| previous != instruction_epoch) {
            self.rescan_sources(process, token.incarnation, &mut current_token)?;
        }
        self.instruction_epochs.insert(process, instruction_epoch);
        let change = self.track_source(process, first, last, token, NATIVE_SOURCE_LIMIT);
        if change == SourceChange::Disabled {
            return None;
        }
        self.executor(process)?;
        if change == SourceChange::Changed {
            self.invalidate_executors(process, &[(first, last)], token.incarnation)?;
        }
        Some(())
    }
}

impl Drop for NativePool {
    fn drop(&mut self) {
        // Native execution and all signal/fault callbacks have returned before
        // this coordinator-owned report. The fixed ring performs no capture
        // allocation and formatting is confined to diagnostics-enabled teardown.
        if let Some(boundaries) = &self.boundaries {
            boundaries.report(None);
        }
        if self.diagnostics {
            // The Rust interpreter's share of the guest instruction stream, which the native
            // `completed` counter cannot see. Reported here so both arms print from one place.
            use std::sync::atomic::Ordering::Relaxed;
            eprintln!(
                "hl-interp: instructions={} blocks={} slices={}",
                hl_execution::INTERPRETED_INSTRUCTIONS.load(Relaxed),
                hl_execution::INTERPRETED_BLOCKS.load(Relaxed),
                hl_execution::INTERPRETED_SLICES.load(Relaxed),
            );
            eprintln!(
                "hl-native: runs={} builds={} hits={} fallbacks={} sites={} services={}",
                self.counters.runs,
                self.counters.builds,
                self.counters.hits,
                self.counters.fallbacks,
                self.fallbacks.len(),
                self.counters.services,
            );
            eprintln!(
                "hl-native-entry: probes={} entries={} declined_executable={} declined_suppressed={} declined_cold={} declined_other={}",
                self.counters.probes,
                self.counters.entries,
                self.counters.declined_executable,
                self.counters.declined_suppressed,
                self.counters.declined_cold,
                self.counters
                    .probes
                    .saturating_sub(self.counters.entries)
                    .saturating_sub(self.counters.declined_executable)
                    .saturating_sub(self.counters.declined_suppressed)
                    .saturating_sub(self.counters.declined_cold),
            );
            eprintln!(
                "hl-native-suppress: verdicts={} latches={} deferred={} clears={} rearms={} permanent={} productive={}/{} saturated={} held={}",
                self.counters.suppress_verdicts,
                self.counters.suppress_latches,
                self.counters.suppress_deferred,
                self.counters.suppress_clears,
                self.counters.suppress_rearms,
                self.counters.suppress_permanent,
                self.productive.len(),
                NATIVE_SITE_LIMIT,
                self.productive_saturated,
                self.suppressed.len(),
            );
            eprintln!(
                "hl-native-productive: marks={} swept={} ever_pcs={} permanent_after_productive={} permanent_hw_half={} permanent_hw_quarter={} permanent_hw_eighth={} hw_entries={}",
                self.counters.productive_marks,
                self.counters.productive_swept,
                self.ever_productive.len(),
                self.counters.permanent_after_productive,
                self.counters.permanent_high_water_half,
                self.counters.permanent_high_water_quarter,
                self.counters.permanent_high_water_eighth,
                self.high_water.len(),
            );
            let resets: u64 = self.executors.values().map(NativeExecutors::cache_resets).sum();
            let hold_remaining = self.bounded_direct_hold_remaining();
            eprintln!(
                "hl-native-direct: admitted={} held={} cold={} site_declined={} flips={} holds={} direct_executors_created={} sticky={} sticky_limit={} hold_runs={} modes={} holding={} hold_remaining={} declined_sites={} resets={}",
                self.counters.direct_admitted,
                self.counters.direct_held,
                self.counters.direct_cold,
                self.counters.direct_site,
                self.counters.direct_flips,
                self.counters.direct_holds,
                self.counters.direct_executors_created,
                self.direct_sticky || self.direct_sticky_permanent,
                self.direct_sticky_limit,
                self.direct_hold_runs,
                self.direct_modes.len(),
                self.direct_holds.len(),
                hold_remaining,
                self.direct_declined.len(),
                resets,
            );
            let mut fallback: Vec<_> = self.fallback_weight.iter().map(|(pc, n)| (*n, *pc)).collect();
            fallback.sort_unstable_by(|a, b| b.cmp(a));
            for (count, pc) in fallback.iter().take(24) {
                eprintln!("hl-native-fallback-pc: pc={pc:#x} count={count}");
            }
            let mut refused: Vec<_> = self.suppressed_weight.iter().map(|(pc, n)| (*n, *pc)).collect();
            refused.sort_unstable_by(|a, b| b.cmp(a));
            for (count, pc) in refused.iter().take(24) {
                eprintln!(
                    "hl-native-suppressed-entry: pc={pc:#x} refused={count} high_water={} permanent={}",
                    self.high_water_pc.get(pc).copied().unwrap_or_default(),
                    self.permanent_pcs.contains(pc),
                );
            }
        }
    }
}
