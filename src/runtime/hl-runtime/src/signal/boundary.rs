use hl_linux::{Errno, GuestMemory, LinuxResult};
use hl_task::{DeliveryAction, PendingTarget, SignalDisposition, SignalInfo, TraceError, TraceStop};

use crate::{RuntimeProcessSyscalls, SignalBoundaryOutcome};

impl<M: GuestMemory> RuntimeProcessSyscalls<M> {
    pub fn deliver_signal_boundary(&self) -> Result<SignalBoundaryOutcome, Errno> {
        self.poll_cpu_itimers();
        self.reconcile_signal_frames()?;
        let signal = if let Some(forced) = self.tasks.prepare_forced_delivery(self.thread) {
            let signal = forced.info().signal;
            drop(forced);
            signal
        } else {
            let prepared = self
                .tasks
                .prepare_deliverable_signal(self.thread)
                .map_err(|_| Errno::ESRCH)?;
            let Some(prepared) = prepared else {
                return Ok(SignalBoundaryOutcome::None);
            };
            let signal = prepared.info().signal;
            self.tasks.force_signal_delivery(prepared).map_err(|_| Errno::ESRCH)?;
            signal
        };
        let pass = {
            let mut pass = self.trace_signal_pass.lock().unwrap_or_else(|error| error.into_inner());
            if *pass == Some(u32::from(signal.get())) {
                *pass = None;
                true
            } else {
                false
            }
        };
        if !pass && signal.get() != 9 {
            let stop = if matches!(signal.get(), 19..=22) {
                TraceStop::Group(u32::from(signal.get()))
            } else {
                TraceStop::Signal(u32::from(signal.get()))
            };
            match self.tasks.trace_stop(self.process, stop) {
                Ok(event) => {
                    return Ok(SignalBoundaryOutcome::Trace {
                        event,
                        signal: signal.get(),
                    });
                }
                Err(TraceError::InvalidLink) => {}
                Err(_) => return Err(Errno::ESRCH),
            }
        }
        let action = self.tasks.action(self.process, signal).map_err(|_| Errno::ESRCH)?;
        if matches!(action.disposition, SignalDisposition::Handler(_)) {
            return match self.deliver_forced_frame() {
                LinuxResult::Value(_) => Ok(SignalBoundaryOutcome::Handled),
                LinuxResult::Error(error) => Err(error),
                LinuxResult::Restart(_) => Err(Errno::EINTR),
            };
        }
        let forced = self.tasks.prepare_forced_delivery(self.thread).ok_or(Errno::ESRCH)?;
        let (_, outcome, control_epoch) = self
            .tasks
            .commit_forced_delivery(forced)
            .map_err(|_| Errno::ESRCH)?
            .ok_or(Errno::ESRCH)?;
        Ok(match outcome {
            DeliveryAction::Ignore => SignalBoundaryOutcome::None,
            DeliveryAction::Stop => SignalBoundaryOutcome::Stop { control_epoch },
            DeliveryAction::Continue => SignalBoundaryOutcome::Continue,
            DeliveryAction::Terminate { dumped_core } => SignalBoundaryOutcome::Terminate {
                signal: signal.get(),
                dumped_core,
            },
            DeliveryAction::Handle(_) => return Err(Errno::EINVAL),
        })
    }

    pub fn resolve_trace_signal(&self, injection: Option<u32>) -> Result<(), Errno> {
        let committed = if let Some(forced) = self.tasks.prepare_forced_delivery(self.thread) {
            self.tasks.discard_forced_delivery(forced).map_err(|_| Errno::ESRCH)?
        } else {
            let prepared = self
                .tasks
                .prepare_deliverable_signal(self.thread)
                .map_err(|_| Errno::ESRCH)?
                .ok_or(Errno::ESRCH)?;
            self.tasks.commit_signal_wait(prepared).map_err(|_| Errno::ESRCH)?
        };
        if !committed {
            return Err(Errno::ESRCH);
        }
        let Some(raw) = injection else { return Ok(()) };
        let signal = hl_task::SignalNumber::new(raw as u8).map_err(|_| Errno::EINVAL)?;
        let queued = self
            .tasks
            .enqueue_signal(PendingTarget::Thread(self.thread), SignalInfo::bare(signal))
            .map_err(|_| Errno::ESRCH)?;
        if queued {
            *self.trace_signal_pass.lock().unwrap_or_else(|error| error.into_inner()) = Some(raw);
        }
        Ok(())
    }

    pub fn restore_signal_boundary(&self) -> Result<(), Errno> {
        match self.rt_sigreturn() {
            LinuxResult::Value(_) => Ok(()),
            LinuxResult::Error(error) => Err(error),
            LinuxResult::Restart(_) => Err(Errno::EINTR),
        }
    }
}
