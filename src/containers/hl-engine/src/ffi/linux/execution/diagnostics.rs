use std::fmt::Write;
use std::os::unix::ffi::OsStringExt;

use hl_runtime::RuntimeSyscallRouter;

use crate::engine::EngineError;
use crate::launch_plan::RuntimeLaunchPlan;

// Retained-C oracle audit (read-only, 2026-08-05): `src/core/launch.c`
// (`hl_run_config_file_with`), `src/core/lifecycle.c` (production launch
// ownership and teardown), `src/core/target/{aarch64,x86_64}.c`
// (`hl_engine_entry`, `hl_standalone_run`, `load_elf`), and
// `src/core/dispatch.c` (`run_guest`). Both ISA targets order validated launch
// material -> ELF/main+interpreter load -> ABI/task publication -> dispatch;
// checkpoint restoration precedes dispatch. Target-owned CPU/ELF state lives
// through the dispatch call and is torn down by the lifecycle owner after it
// returns; launch errors do not transfer ownership to a worker. ISA branches
// select CPU layout/entry only, while POSIX/Windows host process waiting stays
// outside this guest launch phase. Rust owners map respectively to the launch
// plan, Loader/ImageSpace, routing/process registration, checkpoint assembly,
// transfer, waiter pool, and scheduler. These labels expose that ordering but
// intentionally retain the existing public `EngineError::LaunchFailed`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LaunchPhase {
    Entropy,
    Executable,
    Image,
    Routing,
    Registration,
    Thread,
    Checkpoint,
    Transfer,
    Waiter,
}

impl LaunchPhase {
    const fn label(self) -> &'static str {
        match self {
            Self::Entropy => "entropy",
            Self::Executable => "executable",
            Self::Image => "image",
            Self::Routing => "routing",
            Self::Registration => "registration",
            Self::Thread => "thread",
            Self::Checkpoint => "checkpoint",
            Self::Transfer => "transfer",
            Self::Waiter => "waiter",
        }
    }
}

pub(super) fn launch_failure(plan: &RuntimeLaunchPlan, phase: LaunchPhase) -> EngineError {
    if plan.options.get("HL_NATIVE_DIAGNOSTICS") == Some("1") {
        // Labels are closed and bounded: guest input is never written here.
        eprintln!("hl-engine: launch phase {} failed", phase.label());
    }
    EngineError::LaunchFailed
}

pub(super) struct TraceReport<'a> {
    plan: &'a RuntimeLaunchPlan,
    router: &'a RuntimeSyscallRouter,
}

#[cfg(test)]
mod tests {
    use super::LaunchPhase;

    #[test]
    fn launch_phase_labels_are_bounded_and_exhaustive() {
        let phases = [
            LaunchPhase::Entropy,
            LaunchPhase::Executable,
            LaunchPhase::Image,
            LaunchPhase::Routing,
            LaunchPhase::Registration,
            LaunchPhase::Thread,
            LaunchPhase::Checkpoint,
            LaunchPhase::Transfer,
            LaunchPhase::Waiter,
        ];
        assert_eq!(
            phases.map(LaunchPhase::label),
            [
                "entropy",
                "executable",
                "image",
                "routing",
                "registration",
                "thread",
                "checkpoint",
                "transfer",
                "waiter",
            ]
        );
        assert!(phases.into_iter().all(|phase| phase.label().len() <= 12));
    }
}

impl<'a> TraceReport<'a> {
    pub(super) fn new(plan: &'a RuntimeLaunchPlan, router: &'a RuntimeSyscallRouter) -> Self {
        Self { plan, router }
    }

    pub(super) fn write(&self) -> Result<(), EngineError> {
        let Some(path) = self.plan.result_path.as_deref() else {
            return Ok(());
        };
        let Some(records) = self.router.trace() else {
            return Ok(());
        };
        let mut output = String::new();
        for record in records {
            writeln!(
                &mut output,
                "{:?}\t{:#x}\t{}\t{:?}\t{:#x}\t{:#x}",
                record.architecture, record.number, record.name, record.arguments, record.result, record.pc
            )
            .map_err(|_| EngineError::WaitFailed)?;
        }
        std::fs::write(std::ffi::OsString::from_vec(path.to_vec()), output).map_err(|_| EngineError::WaitFailed)
    }
}
