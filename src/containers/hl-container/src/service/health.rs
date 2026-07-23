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
            tokio::select! {
                () = tokio::time::sleep(self.check.interval) => {}
                changed = self.cancel.changed() => {
                    let _ = changed;
                    return;
                }
            }
            let probe = self
                .service
                .execute_probe(&self.container, self.generation, &self.check)
                .await;
            if !self
                .service
                .record_probe(
                    &self.container,
                    self.generation,
                    &self.check,
                    started.elapsed(),
                    probe,
                )
                .await
            {
                return;
            }
        }
    }
}
