use std::sync::Arc;

use hl_descriptor::{
    ObjectError, ObjectKind, OpenFileDescription, Readiness, ReadinessObserver, ReadinessSubscription,
};

use crate::epoll::Epoll;

impl Default for Epoll {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenFileDescription for Epoll {
    fn transfer_dependencies(&self) -> Vec<hl_descriptor::DescriptionRef> {
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .watches
            .iter()
            .map(|watch| watch.target.transfer_reference())
            .collect()
    }

    fn kind(&self) -> ObjectKind {
        ObjectKind::Poll
    }

    fn metadata(&self) -> Result<hl_descriptor::OfdMetadata, ObjectError> {
        Ok(self.status().metadata())
    }

    fn readiness(&self, interests: Readiness) -> Readiness {
        self.poll_readiness(interests)
    }

    fn subscribe_readiness(
        &self,
        observer: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn ReadinessSubscription>, ObjectError> {
        self.subscribe_observer(observer)
    }

    fn retire(&self) {
        self.retire_description();
    }

    fn close(&self) {
        self.finish_retirement();
    }
}
