use super::model::TransactionGuard;
use crate::{MutationAction, MutationError, PinnedParent, VfsMutationHost, VfsMutations};

impl<H: VfsMutationHost> VfsMutations<'_, H> {
    pub(crate) fn pin<'a>(parent: &'a crate::ResolvedParent<'_, H>) -> PinnedParent<'a> {
        PinnedParent {
            node: parent.parent(),
            name: parent.final_component(),
        }
    }

    pub(crate) fn name<'parent>(
        parent: &'parent crate::ResolvedParent<'_, H>,
    ) -> Result<&'parent crate::GuestName, MutationError> {
        parent.final_name().ok_or(MutationError::InvalidName)
    }

    pub(crate) fn publish(
        &self,
        parents: &[PinnedParent<'_>],
        actions: &[MutationAction],
    ) -> Result<(), MutationError> {
        let transaction = self.resolver.host().begin(parents)?;
        let mut guard = TransactionGuard::new(self.resolver.host(), transaction);
        for action in actions {
            self.resolver.host().stage(transaction, action)?;
        }
        self.resolver.host().commit(transaction)?;
        guard.mark_committed();
        Ok(())
    }
}
