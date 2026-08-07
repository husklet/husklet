use std::collections::BTreeSet;

use hl_linux::Errno;

use super::{EventPort, PollEntry};
use crate::ffi::linux::execution::descriptor::Set;

impl EventPort {
    pub(super) fn subscriptions(
        &self,
        entries: &[PollEntry],
    ) -> Result<Vec<Box<dyn hl_descriptor::ReadinessSubscription>>, Errno> {
        let mut subscriptions = Vec::new();
        let mut descriptions = BTreeSet::new();
        for entry in entries
            .iter()
            .filter(|entry| entry.descriptor < 0 && entry.generation.is_some())
        {
            let Ok(lease) = self.descriptors.pin(entry.guest) else {
                continue;
            };
            if !descriptions.insert(lease.description_identity()) {
                continue;
            }
            subscriptions.push(
                lease
                    .subscribe_readiness(self.wake.clone())
                    .map_err(Set::object_errno)?,
            );
        }
        Ok(subscriptions)
    }
}
