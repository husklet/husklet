//! Interval-timer scheduling composition.

use std::sync::Arc;

use super::readiness::deadline::Queue;

pub(super) struct Scheduler(pub(super) Arc<Queue>);

impl hl_runtime::AlarmScheduler for Scheduler {
    fn now(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    fn schedule(&self, deadline: u64, callback: Arc<dyn Fn() + Send + Sync>) -> Result<u64, ()> {
        self.0.schedule_callback(deadline, callback).map_err(|_| ())
    }

    fn cancel(&self, token: u64) {
        self.0.cancel(token);
    }
}
