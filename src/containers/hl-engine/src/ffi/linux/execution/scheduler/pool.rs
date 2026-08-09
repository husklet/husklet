//! Native executor pool state shared by the scheduler.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::{NATIVE_BOUNDARY_CAPACITY, NATIVE_SITE_LIMIT, NATIVE_SOURCE_LIMIT};
use crate::activation::GuestIsa;
use crate::launch_plan::RuntimeLaunchPlan;

/// Leaky flip score that identifies a process whose entries alternate between direct
/// authority and the operand resolver, and the runs each such finding holds for.
const DIRECT_FLIP_LIMIT: u32 = 32;
const DIRECT_HOLD_RUNS: u64 = 1 << 16;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SourceChange {
    Stable,
    Changed,
    Disabled,
}

pub(in crate::ffi::linux::execution) struct NativePool {
    pub(super) enabled: bool,
    /// Gates the opt-in profiling tables and the boundary ring, whose recording costs
    /// memory on the native path; failures are reported through `hl_log` regardless.
    pub(super) diagnostics: bool,
    pub(super) executors: BTreeMap<hl_task::ProcessId, crate::native::NativeExecutor>,
    pub(super) suppressed: BTreeMap<NativeSite, Probation>,
    /// Entries that have completed a native run retiring a substantial share of their
    /// budget at least once. Only these have shown there is native work here to lose,
    /// so only these earn a second probationary span after a latch.
    pub(super) productive: BTreeSet<NativeSite>,
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
        Self {
            enabled: Self::selected(isa, plan),
            diagnostics: plan.options.get("HL_NATIVE_DIAGNOSTICS") == Some("1"),
            executors: BTreeMap::new(),
            suppressed: BTreeMap::new(),
            productive: BTreeSet::new(),
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
        }
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

    pub(super) fn executor(&mut self, process: hl_task::ProcessId) -> Option<&crate::native::NativeExecutor> {
        if !self.enabled {
            return None;
        }
        if !self.executors.contains_key(&process) {
            self.executors.insert(
                process,
                crate::native::NativeExecutor::create_with_fault_owner(self.diagnostics, self.host_faults.clone())
                    .ok()?,
            );
        }
        self.executors.get(&process)
    }

    /// Whether this process may still be offered direct authority. A held process is
    /// serving out a hold because its run mode was alternating; each call spends one run.
    pub(super) fn direct_admitted(&mut self, process: hl_task::ProcessId) -> bool {
        let Some(remaining) = self.direct_holds.get_mut(&process) else {
            return true;
        };
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

    /// Records the run mode this process just used. The cache identity carries the mode, so
    /// sustained alternation resets every translation twice per cycle; hold direct
    /// authority off for a bounded run of turns so the resolver mode can keep its cache.
    pub(super) fn observe_direct_mode(&mut self, process: hl_task::ProcessId, direct: bool) {
        let (previous, flips) = self.direct_modes.entry(process).or_insert((direct, 0));
        if *previous == direct {
            // A steady run pays down the score, so an isolated flip never accumulates.
            *flips = flips.saturating_sub(1);
            return;
        }
        *previous = direct;
        *flips += 2;
        if *flips >= DIRECT_FLIP_LIMIT {
            self.direct_modes.remove(&process);
            self.direct_holds.insert(process, DIRECT_HOLD_RUNS);
        }
    }

    /// Records that this entry completed a run retiring a substantial share of its budget,
    /// which is the evidence that a later short run is an anomaly rather than its nature.
    pub(super) fn mark_productive(&mut self, entry: NativeSite, executed: u64, budget: u64) {
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

    pub(super) fn disable(&mut self) {
        self.enabled = false;
        self.executors.clear();
        self.sources.clear();
        self.source_incarnations.clear();
        self.instruction_epochs.clear();
        self.observations.clear();
        self.suppressed.clear();
        self.productive.clear();
        self.productive_saturated = false;
        self.direct_declined.clear();
        self.fallbacks.clear();
        self.direct_modes.clear();
        self.direct_holds.clear();
    }

    pub(super) fn reset_observed_sources(&mut self, process: hl_task::ProcessId, incarnation: u64) -> Option<()> {
        self.executor(process)?.reset(incarnation).ok()?;
        self.sources.retain(|(owner, _, _, _), _| *owner != process);
        self.observations.retain(|(owner, _, _, _), _| *owner != process);
        let held = self.suppressed.len();
        self.suppressed.retain(|(owner, _, _, _), _| *owner != process);
        self.counters.suppress_clears += (held - self.suppressed.len()) as u64;
        self.productive.retain(|(owner, _, _, _)| *owner != process);
        self.direct_declined.retain(|(owner, _, _, _)| *owner != process);
        self.fallbacks.retain(|(owner, _, _, _)| *owner != process);
        Some(())
    }

    pub(super) fn purge_process_metadata(&mut self, process: hl_task::ProcessId) {
        self.sources.retain(|(owner, _, _, _), _| *owner != process);
        self.observations.retain(|(owner, _, _, _), _| *owner != process);
        let held = self.suppressed.len();
        self.suppressed.retain(|(owner, _, _, _), _| *owner != process);
        self.counters.suppress_clears += (held - self.suppressed.len()) as u64;
        self.productive.retain(|(owner, _, _, _)| *owner != process);
        self.direct_declined.retain(|(owner, _, _, _)| *owner != process);
        self.fallbacks.retain(|(owner, _, _, _)| *owner != process);
        self.source_incarnations.remove(&process);
        self.instruction_epochs.remove(&process);
        self.direct_modes.remove(&process);
        self.direct_holds.remove(&process);
        self.boundary_sensitive.remove(&process);
    }

    pub(super) fn reset_process(&mut self, process: hl_task::ProcessId) -> Option<()> {
        self.executor(process)?.reset(0).ok()?;
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
            self.productive.retain(|(process, _, _, _)| live.contains(process));
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
            let observed: Vec<_> = self
                .sources
                .iter()
                .filter(|((owner, incarnation, _, _), _)| *owner == process && *incarnation == token.incarnation)
                .map(|(key @ (_, _, first, last), previous)| (*key, *previous, *first, *last))
                .collect();
            if observed.len() > 1024 {
                self.reset_observed_sources(process, token.incarnation)?;
            } else {
                let mut changed = Vec::new();
                let mut refreshed = Vec::new();
                let mut gap = false;
                for (key, previous, source_first, source_last) in observed {
                    let Some(current) = current_token(source_first, source_last) else {
                        gap = true;
                        break;
                    };
                    if current.incarnation != token.incarnation {
                        gap = true;
                        break;
                    }
                    if current != previous {
                        changed.push((source_first, source_last));
                    }
                    refreshed.push((key, current));
                }
                if gap {
                    self.reset_observed_sources(process, token.incarnation)?;
                } else {
                    if !changed.is_empty()
                        && self
                            .executor(process)?
                            .invalidate_ranges(&changed, token.incarnation)
                            .is_err()
                    {
                        self.reset_observed_sources(process, token.incarnation)?;
                        refreshed.clear();
                    }
                    for (key, current) in refreshed {
                        self.sources.insert(key, current);
                    }
                }
            }
        }
        self.instruction_epochs.insert(process, instruction_epoch);
        let change = self.track_source(process, first, last, token, NATIVE_SOURCE_LIMIT);
        if change == SourceChange::Disabled {
            return None;
        }
        let executor = self.executor(process)?;
        if change == SourceChange::Changed {
            executor.invalidate(first, last, token.incarnation).ok()?;
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
            let mut fallback: Vec<_> = self.fallback_weight.iter().map(|(pc, n)| (*n, *pc)).collect();
            fallback.sort_unstable_by(|a, b| b.cmp(a));
            for (count, pc) in fallback.iter().take(24) {
                eprintln!("hl-native-fallback-pc: pc={pc:#x} count={count}");
            }
            let mut refused: Vec<_> = self.suppressed_weight.iter().map(|(pc, n)| (*n, *pc)).collect();
            refused.sort_unstable_by(|a, b| b.cmp(a));
            for (count, pc) in refused.iter().take(24) {
                eprintln!("hl-native-suppressed-entry: pc={pc:#x} refused={count}");
            }
        }
    }
}
