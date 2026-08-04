use super::Service;
use crate::{ContainerId, Healthcheck};
use std::sync::Arc;
use std::time::Instant;

/// One generation-bound health-check owner.
pub(super) struct Monitor {
    service: Arc<Service>,
    container: ContainerId,
    generation: u64,
    check: Healthcheck,
    cancel: tokio::sync::watch::Receiver<bool>,
}

impl Monitor {
    pub(super) fn new(
        service: Arc<Service>,
        container: ContainerId,
        generation: u64,
        check: Healthcheck,
        cancel: tokio::sync::watch::Receiver<bool>,
    ) -> Self {
        Self {
            service,
            container,
            generation,
            check,
            cancel,
        }
    }

    pub(super) async fn run(mut self) {
        let started = Instant::now();
        loop {
            let interval = if started.elapsed() < self.check.start_period {
                self.check.start_interval
            } else {
                self.check.interval
            };
            tokio::select! {
                () = tokio::time::sleep(interval) => {}
                changed = self.cancel.changed() => {
                    let _ = changed;
                    return;
                }
            }
            let probe = self
                .service
                .execute_probe(&self.container, self.generation, &self.check, &mut self.cancel)
                .await;
            let Some(probe) = probe else { return };
            if !self
                .service
                .record_probe(&self.container, self.generation, &self.check, started.elapsed(), probe)
                .await
            {
                return;
            }
        }
    }
}
