//! Native-execution slice entry points for the guest executor.

use hl_execution::{ExecutionCpuSnapshot, StepOutcome};
use hl_isa::GuestAddress;
use hl_memory::Protection;

use super::super::{GuestExecutor, threads};
use super::pool::{NativeBoundary, NativePool};

impl GuestExecutor {
    pub(super) fn native_slice(
        run: &threads::ThreadRun,
        memory: &mut super::super::operand::SliceMemory<'_>,
        pool: &mut NativePool,
        native_budget: u64,
        continuation: Option<&threads::SchedulerContinuation>,
    ) -> Option<StepOutcome> {
        // A disabled pool can never enter native code, and probing anyway grows the
        // observation and source tables the scheduler has to sweep every turn.
        if !pool.enabled {
            return None;
        }
        pool.counters.probes += 1;
        enum NativeCoordinates {
            Aarch64 { pc: u64, stack: u64 },
            X86_64,
        }
        let mut coordinates = None;
        let observed = run.machine.handle_syscall(1, |snapshot| {
            coordinates = Some(match snapshot {
                ExecutionCpuSnapshot::Aarch64(cpu) => NativeCoordinates::Aarch64 {
                    pc: cpu.pc,
                    stack: cpu.sp,
                },
                ExecutionCpuSnapshot::X86_64(_) => NativeCoordinates::X86_64,
            });
            StepOutcome::Continue
        });
        if observed != StepOutcome::Continue {
            return Some(observed);
        }
        let NativeCoordinates::Aarch64 { pc, stack } = coordinates? else {
            return Self::native_x86(run, memory, pool, native_budget, continuation);
        };
        let lease = super::super::operand::ImageMemory::lease(memory);
        let mappings = lease.mappings();
        let Some(executable) = mappings.ledger().resolve(GuestAddress::new(pc), Protection::EXECUTE) else {
            pool.counters.declined_executable += 1;
            return None;
        };
        let executable = executable.region;
        let available = executable.range().end().get().saturating_sub(pc).min(256);
        let length = available - available % 4;
        if length == 0 {
            return None;
        }
        let source_range = hl_isa::AddressRange::nonempty(GuestAddress::new(pc), length).ok()?;
        let token = mappings.executable_token(source_range, lease.generation());
        let fallback_key = (run.process, lease.generation(), token.version, pc);
        if pool.suppressed.contains(&fallback_key) {
            if pool.diagnostics {
                *pool.suppressed_weight.entry(pc).or_default() += 1;
            }
            pool.counters.declined_suppressed += 1;
            return None;
        }
        if pool.observe(fallback_key) < 2 {
            pool.counters.declined_cold += 1;
            return None;
        }
        let allow_direct = pool.direct_admitted(run.process) && !pool.direct_declined.contains(&fallback_key);
        let mut bytes = [0_u8; 256];
        let bytes = &mut bytes[..usize::try_from(length).ok()?];
        mappings
            .read_spans(GuestAddress::new(pc), bytes, Protection::EXECUTE)
            .ok()?;
        let projection = if let Some((first, last)) =
            crate::native::InstructionWord::read(bytes).and_then(|instruction| instruction.literal_interval(pc))
        {
            mappings
                .project_contiguous(
                    GuestAddress::new(first),
                    last.checked_sub(first)?,
                    Protection::READ,
                    lease.generation(),
                )
                .ok()?
        } else {
            let stack_region = mappings
                .ledger()
                .resolve(
                    GuestAddress::new(stack.saturating_sub(1)),
                    Protection::READ.union(Protection::WRITE),
                )?
                .region;
            let stack_first = stack.saturating_sub(8 << 10).max(stack_region.range().start().get()) & !4095;
            let stack_last = stack_first
                .saturating_add(16 << 10)
                .min(stack_region.range().end().get());
            mappings
                .project_contiguous(
                    GuestAddress::new(stack_first),
                    stack_last.checked_sub(stack_first)?,
                    Protection::READ.union(Protection::WRITE),
                    lease.generation(),
                )
                .ok()?
        };
        pool.prepare_source(
            run.process,
            pc,
            pc + length,
            token,
            super::super::operand::ImageMemory::epoch(memory).writes,
            |first, last| {
                let range = hl_isa::AddressRange::nonempty(GuestAddress::new(first), last - first).ok()?;
                Some(mappings.executable_token(range, lease.generation()))
            },
        )?;
        pool.counters.entries += 1;
        let executor = pool.executor(run.process)?;
        let source = [crate::native::NativeSource { guest_first: pc, bytes }];
        let checkpoint = projection.checkpoint_continuation();
        let mapping = projection.request_continuation();
        // A still-current grant extends the run in place at the same instruction
        // granularity the budget would have yielded at.
        let mut poll = |_: u64, admitted: u64| {
            Self::may_extend_activation(admitted)
                && checkpoint.is_current()
                && mapping.is_current()
                && continuation.is_some_and(threads::SchedulerContinuation::is_current)
        };
        let mut fallback = false;
        let mut fallback_pc = None;
        let mut statistics = None;
        let mut resolve = |target: u64, output: &mut [u8]| {
            let region = mappings
                .ledger()
                .resolve(GuestAddress::new(target), Protection::EXECUTE)?
                .region;
            let available = region
                .range()
                .end()
                .get()
                .saturating_sub(target)
                .min(output.len() as u64);
            let length = usize::try_from(available - available % 4).ok()?;
            if length == 0 {
                return None;
            }
            mappings
                .read_spans(GuestAddress::new(target), &mut output[..length], Protection::EXECUTE)
                .ok()?;
            let range = hl_isa::AddressRange::nonempty(GuestAddress::new(target), length as u64).ok()?;
            Some((length, mappings.executable_token(range, lease.generation())))
        };
        let outcome = run.machine.handle_syscall(1, |snapshot| {
            let ExecutionCpuSnapshot::Aarch64(cpu) = snapshot else {
                return StepOutcome::Fault(hl_execution::ExecutionFault::CacheEpoch);
            };
            let original = cpu.clone();
            let Ok((result, stats)) =
                executor.run_lease(
                    cpu,
                    &source,
                    projection,
                    token,
                    &run.interrupt,
                    native_budget,
                    &mut resolve,
                    allow_direct,
                    continuation.map(|_| &mut poll as &mut dyn FnMut(u64, u64) -> bool),
                )
            else {
                hl_log::hl_error!(
                    hl_log::tag::EXEC,
                    "native run refused isa=aarch64 pc={:#x} invariant={}",
                    original.pc,
                    crate::native::state_invariant(),
                );
                *cpu = original;
                return StepOutcome::Fault(hl_execution::ExecutionFault::NativeFatal {
                    code: 100 + u64::from(executor.last_lease_failure()),
                });
            };
            statistics = Some(stats);
            match result.exit {
                crate::native::NativeExit::Branch => StepOutcome::Continue,
                crate::native::NativeExit::Syscall => {
                    let instruction = result.instruction;
                    let next = instruction.wrapping_add(4);
                    cpu.pc = next;
                    StepOutcome::Syscall { instruction, next }
                }
                crate::native::NativeExit::Yield => StepOutcome::Yield,
                crate::native::NativeExit::Fallback | crate::native::NativeExit::Fault => {
                    fallback = true;
                    fallback_pc = Some((result.instruction, result.executed));
                    StepOutcome::Continue
                }
                crate::native::NativeExit::Epoch | crate::native::NativeExit::Interrupt => {
                    Self::native_boundary(cpu, original, result.exit, result.instruction, result.executed)
                        .expect("native boundary exit")
                }
                crate::native::NativeExit::Fatal => {
                    hl_log::hl_error!(
                        hl_log::tag::EXEC,
                        "native execution died isa=aarch64 pc={pc:#x} instruction={:#x} code={} remaining={} executed={} invariant={}",
                        result.instruction,
                        result.code,
                        result.remaining,
                        result.executed,
                        crate::native::state_invariant(),
                    );
                    StepOutcome::Fault(hl_execution::ExecutionFault::NativeFatal { code: result.code })
                }
            }
        });
        let direct_guard = statistics.as_ref().is_some_and(|stats| stats.direct_guard);
        if let Some(stats) = statistics {
            pool.counters.runs += 1;
            pool.counters.builds += stats.builds;
            pool.counters.hits += stats.hits;
            pool.observe_direct_mode(run.process, stats.direct);
            pool.merge_observed_sources(run.process, lease.generation(), stats.sources, stats.sources_complete)?;
        }
        if fallback {
            pool.counters.fallbacks += 1;
            if let Some((pc, executed)) = fallback_pc {
                let instruction = (run.process, lease.generation(), token.version, pc);
                pool.record_fallback(fallback_key, instruction, executed, native_budget, direct_guard);
            }
            Some(run.machine.run_step(1, memory))
        } else {
            Some(outcome)
        }
    }

    fn native_x86(
        run: &threads::ThreadRun,
        memory: &mut super::super::operand::SliceMemory<'_>,
        pool: &mut NativePool,
        native_budget: u64,
        continuation: Option<&threads::SchedulerContinuation>,
    ) -> Option<StepOutcome> {
        let mut coordinates = None;
        let observed = run.machine.handle_syscall(1, |snapshot| {
            if let ExecutionCpuSnapshot::X86_64(cpu) = snapshot {
                coordinates = Some((cpu.rip, cpu.registers[4]));
            }
            StepOutcome::Continue
        });
        if observed != StepOutcome::Continue {
            return Some(observed);
        }
        let (pc, stack) = coordinates?;
        let lease = super::super::operand::ImageMemory::lease(memory);
        let mappings = lease.mappings();
        let snapshot = mappings.snapshot();
        let executable = snapshot.regions.iter().copied().find(|region| {
            region.range().contains(GuestAddress::new(pc)) && region.protection().contains(Protection::EXECUTE)
        })?;
        let length = usize::try_from(executable.range().end().get().saturating_sub(pc).min(256)).ok()?;
        if length == 0 {
            return None;
        }
        let source_range = hl_isa::AddressRange::nonempty(GuestAddress::new(pc), length as u64).ok()?;
        let token = mappings.executable_token(source_range, lease.generation());
        let key = (run.process, lease.generation(), token.version, pc);
        if pool.suppressed.contains(&key) {
            if pool.diagnostics {
                *pool.suppressed_weight.entry(pc).or_default() += 1;
            }
            return None;
        }
        if pool.observe(key) < 2 {
            return None;
        }
        let mut bytes = vec![0_u8; length];
        mappings
            .read_spans(GuestAddress::new(pc), &mut bytes, Protection::EXECUTE)
            .ok()?;
        let stack_region = snapshot.regions.iter().copied().find(|region| {
            region.range().contains(GuestAddress::new(stack.saturating_sub(1)))
                && region.protection().contains(Protection::READ.union(Protection::WRITE))
        })?;
        let stack_first = stack.saturating_sub(8 << 10).max(stack_region.range().start().get()) & !4095;
        let stack_last = stack_first
            .saturating_add(16 << 10)
            .min(stack_region.range().end().get());
        let stack_projection = mappings
            .project_contiguous(
                GuestAddress::new(stack_first),
                stack_last.checked_sub(stack_first)?,
                Protection::READ.union(Protection::WRITE),
                lease.generation(),
            )
            .ok()?;
        pool.prepare_source(
            run.process,
            pc,
            pc + length as u64,
            token,
            super::super::operand::ImageMemory::epoch(memory).writes,
            |first, last| {
                let range = hl_isa::AddressRange::nonempty(GuestAddress::new(first), last - first).ok()?;
                Some(mappings.executable_token(range, lease.generation()))
            },
        )?;
        let diagnostics = pool.boundaries.is_some();
        let executor = pool.executor(run.process)?;
        let source = [crate::native::NativeSource {
            guest_first: pc,
            bytes: &bytes,
        }];
        let mut resolve = |target: u64, output: &mut [u8]| {
            let region = snapshot.regions.iter().copied().find(|region| {
                region.range().contains(GuestAddress::new(target)) && region.protection().contains(Protection::EXECUTE)
            })?;
            let length = usize::try_from(
                region
                    .range()
                    .end()
                    .get()
                    .saturating_sub(target)
                    .min(output.len() as u64),
            )
            .ok()?;
            if length == 0 {
                return None;
            }
            mappings
                .read_spans(GuestAddress::new(target), &mut output[..length], Protection::EXECUTE)
                .ok()?;
            let range = hl_isa::AddressRange::nonempty(GuestAddress::new(target), length as u64).ok()?;
            Some((length, mappings.executable_token(range, lease.generation())))
        };
        let mut fallback = None;
        let mut statistics = None;
        let mut boundary = None;
        let mut stack_projection = Some(stack_projection);
        let checkpoint = stack_projection.as_ref()?.checkpoint_continuation();
        let mapping = stack_projection.as_ref()?.request_continuation();
        let mut poll = |_: u64, admitted: u64| {
            Self::may_extend_activation(admitted)
                && checkpoint.is_current()
                && mapping.is_current()
                && continuation.is_some_and(threads::SchedulerContinuation::is_current)
        };
        let outcome = run.machine.handle_syscall(1, |snapshot| {
            let ExecutionCpuSnapshot::X86_64(cpu) = snapshot else {
                return StepOutcome::Fault(hl_execution::ExecutionFault::CacheEpoch);
            };
            let original = cpu.clone();
            let Some(projection) = stack_projection.take() else {
                return StepOutcome::Fault(hl_execution::ExecutionFault::NativeFatal {
                    code: 100 + crate::native::NativeLeaseStep::X86StackProjection as u64,
                });
            };
            let Ok((result, stats)) = executor.run_x86_lease(
                cpu,
                &source,
                projection,
                token,
                native_budget,
                false,
                &mut resolve,
                Some(&mut poll),
            ) else {
                *cpu = original;
                return StepOutcome::Fault(hl_execution::ExecutionFault::NativeFatal {
                    code: 100 + u64::from(executor.last_lease_failure()),
                });
            };
            statistics = Some(stats);
            boundary = diagnostics.then(|| NativeBoundary {
                process: run.process,
                start: pc,
                exit: result.exit,
                instruction: result.instruction,
                next: result.next,
                rip: cpu.rip,
                stack: cpu.registers[4],
                registers: cpu.registers,
                vectors: cpu.vectors,
                vector_upper: cpu.vector_upper,
                flags: cpu.flags.bits(),
                mxcsr: cpu.mxcsr,
                fs_base: cpu.fs_base,
                gs_base: cpu.gs_base,
                direction: cpu.direction,
                alignment_check: cpu.alignment_check,
                id_flag: cpu.id_flag,
                fault_address: result.address,
                provenance: token.version,
            });
            match result.exit {
                crate::native::NativeExit::Branch => StepOutcome::Continue,
                crate::native::NativeExit::Syscall => {
                    cpu.rip = result.next;
                    StepOutcome::Syscall {
                        instruction: result.instruction,
                        next: result.next,
                    }
                }
                crate::native::NativeExit::Yield if Self::x86_yield_needs_interpreter(result.exit, result.executed) => {
                    fallback = Some((result.instruction, result.executed));
                    StepOutcome::Continue
                }
                crate::native::NativeExit::Yield => StepOutcome::Yield,
                crate::native::NativeExit::Fallback | crate::native::NativeExit::Fault => {
                    fallback = Some((result.instruction, result.executed));
                    StepOutcome::Continue
                }
                crate::native::NativeExit::Epoch => {
                    if Self::epoch_rewinds(result.executed) {
                        *cpu = original;
                    }
                    StepOutcome::Yield
                }
                crate::native::NativeExit::Interrupt => {
                    *cpu = original;
                    StepOutcome::Yield
                }
                crate::native::NativeExit::Fatal => {
                    hl_log::hl_error!(
                        hl_log::tag::EXEC,
                        "native execution died isa=x86_64 pc={pc:#x} rip={:#x} code={} remaining={} executed={}",
                        cpu.rip,
                        result.code,
                        result.remaining,
                        result.executed,
                    );
                    StepOutcome::Fault(hl_execution::ExecutionFault::NativeFatal { code: result.code })
                }
            }
        });
        if let Some(stats) = statistics {
            pool.counters.runs += 1;
            pool.counters.builds += stats.builds;
            pool.counters.hits += stats.hits;
            pool.merge_observed_sources(run.process, lease.generation(), stats.sources, stats.sources_complete)?;
        }
        if let Some(record) = boundary
            && let Some(boundaries) = &mut pool.boundaries
        {
            boundaries.push(record);
        }
        drop(stack_projection);
        if let Some((fallback_pc, executed)) = fallback {
            pool.counters.fallbacks += 1;
            pool.record_fallback(
                key,
                (run.process, lease.generation(), token.version, fallback_pc),
                executed,
                native_budget,
                false,
            );
            Some(run.machine.run_step(1, memory))
        } else {
            Some(outcome)
        }
    }
}
