use std::sync::Arc;

use hl_linux::{Errno, GuestMemory, LinuxResult, SignalAbi};
use hl_sync::{Interruption, WaitOutcome, WaitQueue};
use hl_task::SignalActivityWake;
use hl_time::{ClockError, Deadline, Duration, MonotonicClock, MonotonicInstant};

use crate::RuntimeProcessSyscalls;

struct WaitWake(Arc<WaitQueue>);
struct NoDeadlineClock;

impl MonotonicClock for NoDeadlineClock {
    fn monotonic_now(&self) -> Result<MonotonicInstant, ClockError> {
        Err(ClockError::NotSupported)
    }
}

impl SignalActivityWake for WaitWake {
    fn signal_activity_changed(&self) {
        self.0.notify_all();
    }
}

impl<M: GuestMemory> RuntimeProcessSyscalls<M> {
    pub(crate) fn rt_sigsuspend(&self, arguments: [u64; 6]) -> LinuxResult {
        let mask = match SignalAbi::new(&self.memory, self.architecture).suspend(arguments[0], arguments[1] as usize) {
            Ok(mask) => mask,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        self.suspend_with_mask(Some(mask))
    }

    pub(crate) fn pause(&self) -> LinuxResult {
        self.suspend_with_mask(None)
    }

    pub(crate) fn rt_sigtimedwait(&self, arguments: [u64; 6]) -> LinuxResult {
        let abi = SignalAbi::new(&self.memory, self.architecture);
        let plan = match abi.timed_wait(arguments[0], arguments[1], arguments[2], arguments[3] as usize) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let queue = Arc::new(WaitQueue::new());
        let wake: Arc<dyn SignalActivityWake> = Arc::new(WaitWake(queue.clone()));
        let _subscription = self.tasks.subscribe_signal_activity(wake);
        let deadline = match self.wait_deadline(plan.timeout) {
            Ok(value) => value,
            Err(result) => return result,
        };
        let prepared = loop {
            let observed = queue.observation();
            match self.tasks.prepare_signal_wait(self.thread, plan.selected) {
                Ok(Some(value)) => break value,
                Err(_) => return LinuxResult::Error(Errno::ESRCH),
                Ok(None) => {}
            }
            match self.tasks.has_deliverable_except(self.thread, plan.selected) {
                Ok(true) => return LinuxResult::Error(Errno::EINTR),
                Err(_) => return LinuxResult::Error(Errno::ESRCH),
                Ok(false) => {}
            }
            if deadline == Some(Deadline::from_nanoseconds(0)) {
                return LinuxResult::Error(Errno::EAGAIN);
            }
            let clock: &dyn MonotonicClock = self.clock.as_deref().map_or(&NoDeadlineClock, |clock| clock);
            match queue.wait(observed, &Interruption::new(), deadline, clock) {
                Ok(WaitOutcome::Notified) => {}
                Ok(WaitOutcome::TimedOut) => {
                    return LinuxResult::Error(Errno::EAGAIN);
                }
                Ok(WaitOutcome::Interrupted) => {
                    return LinuxResult::Error(Errno::EINTR);
                }
                Err(_) => return LinuxResult::Error(Errno::EIO),
            }
        };
        let info = prepared.info();
        if plan.information != 0 {
            let copyout = match abi.stage_info(plan.information, info) {
                Ok(value) => value,
                Err(error) => return LinuxResult::Error(error.errno()),
            };
            let marshaller = hl_linux::GuestMarshaller::new(&self.memory, self.architecture);
            if let Err(error) = copyout.commit(&marshaller) {
                return LinuxResult::Error(error.errno());
            }
        }
        match self.tasks.commit_signal_wait(prepared) {
            Ok(true) => LinuxResult::Value(info.signal.get() as u64),
            Ok(false) => LinuxResult::Error(Errno::EAGAIN),
            Err(_) => LinuxResult::Error(Errno::ESRCH),
        }
    }

    fn wait_deadline(&self, timeout: Option<hl_time::Timespec>) -> Result<Option<Deadline>, LinuxResult> {
        let Some(timeout) = timeout else {
            return Ok(None);
        };
        if timeout.seconds() == 0 && timeout.subsecond_nanoseconds() == 0 {
            return Ok(Some(Deadline::from_nanoseconds(0)));
        }
        let Some(clock) = self.clock.as_deref() else {
            return Err(LinuxResult::Error(Errno::ENOSYS));
        };
        let now = clock.monotonic_now().map_err(|_| LinuxResult::Error(Errno::EIO))?;
        let duration = Duration::from_timespec(timeout).ok_or(LinuxResult::Error(Errno::EINVAL))?;
        Ok(Some(now.deadline_after(duration)))
    }

    fn suspend_with_mask(&self, temporary: Option<hl_task::SignalMask>) -> LinuxResult {
        let queue = Arc::new(WaitQueue::new());
        let wake: Arc<dyn SignalActivityWake> = Arc::new(WaitWake(queue.clone()));
        let _subscription = self.tasks.subscribe_signal_activity(wake);
        let old_mask = match temporary {
            Some(mask) => match self.tasks.replace_signal_mask(self.thread, mask) {
                Ok(previous) => Some(previous),
                Err(_) => return LinuxResult::Error(Errno::ESRCH),
            },
            None => None,
        };
        let result = loop {
            let observed = queue.observation();
            match self.force_deliverable_signal() {
                Ok(true) => break LinuxResult::Error(Errno::EINTR),
                Ok(false) => {}
                Err(error) => break LinuxResult::Error(error),
            }
            let clock: &dyn MonotonicClock = self.clock.as_deref().map_or(&NoDeadlineClock, |clock| clock);
            match queue.wait(observed, &Interruption::new(), None, clock) {
                Ok(WaitOutcome::Notified) => {}
                Ok(WaitOutcome::Interrupted) => {
                    break LinuxResult::Error(Errno::EINTR);
                }
                Ok(WaitOutcome::TimedOut) => {}
                Err(_) => break LinuxResult::Error(Errno::EIO),
            }
        };
        if let Some(mask) = old_mask {
            if self.tasks.replace_signal_mask(self.thread, mask).is_err() {
                return LinuxResult::Error(Errno::ESRCH);
            }
        }
        result
    }

    fn force_deliverable_signal(&self) -> Result<bool, Errno> {
        let prepared = self
            .tasks
            .prepare_deliverable_signal(self.thread)
            .map_err(|_| Errno::ESRCH)?;
        let Some(prepared) = prepared else {
            return Ok(false);
        };
        self.tasks.force_signal_delivery(prepared).map_err(|_| Errno::ESRCH)?;
        Ok(true)
    }
}
