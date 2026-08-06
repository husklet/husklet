use std::fmt::Write;
use std::os::unix::ffi::OsStringExt;

use hl_runtime::RuntimeSyscallRouter;

use crate::engine::EngineError;
use crate::launch_plan::RuntimeLaunchPlan;

pub(super) struct TraceReport<'a> {
    plan: &'a RuntimeLaunchPlan,
    router: &'a RuntimeSyscallRouter,
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
