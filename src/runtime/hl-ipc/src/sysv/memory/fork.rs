use std::sync::Arc;

use hl_memory::SharedBackingRef;

use crate::{InheritedAttachment, SharedMemoryError, SharedMemoryId};

use super::{Attachment, NamespaceState, SharedMemoryNamespace};

#[derive(Clone, Copy, Debug)]
struct PlannedAttachment {
    parent: u64,
    child: u64,
    segment: SharedMemoryId,
    backing: SharedBackingRef,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ForkAttachmentPlan {
    pub parent: u64,
    pub child: u64,
    pub backing: SharedBackingRef,
}

/// A mutation-free `SysV` shared-memory fork plan.
///
/// Commit validates the namespace topology and attachment counter under one
/// lock before publishing any child attachment. Dropping a plan therefore
/// preserves tokens and segment metadata exactly.
pub struct PreparedMemoryFork<'a> {
    namespace: &'a SharedMemoryNamespace,
    plan: ForkPlan,
}

#[derive(Clone)]
struct ForkPlan {
    parent: u32,
    child: u32,
    now: u64,
    expected_next: u64,
    next: u64,
    attachments: Vec<PlannedAttachment>,
}

pub struct OwnedPreparedFork {
    namespace: Arc<SharedMemoryNamespace>,
    plan: ForkPlan,
}
pub type OwnedPreparedMemoryFork = OwnedPreparedFork;

pub struct CommittedMemoryFork {
    namespace: Arc<SharedMemoryNamespace>,
    previous: NamespaceState,
    published: NamespaceState,
}

impl CommittedMemoryFork {
    pub fn rollback(self) -> Result<(), SharedMemoryError> {
        let mut state = self
            .namespace
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *state != self.published {
            return Err(SharedMemoryError::InvalidArgument);
        }
        *state = self.previous;
        Ok(())
    }

    pub fn finish(self) {}
}

impl OwnedPreparedFork {
    #[must_use]
    pub fn bindings(&self) -> Vec<ForkAttachmentPlan> {
        PreparedMemoryFork::binding_plans(&self.plan.attachments)
    }

    pub fn commit(self) -> Result<Vec<InheritedAttachment>, SharedMemoryError> {
        PreparedMemoryFork::commit_plan(&self.namespace, &self.plan).map(|(inherited, _, _)| inherited)
    }

    pub fn commit_reversible(self) -> Result<CommittedMemoryFork, SharedMemoryError> {
        let (_, previous, published) = PreparedMemoryFork::commit_plan(&self.namespace, &self.plan)?;
        Ok(CommittedMemoryFork {
            namespace: self.namespace,
            previous,
            published,
        })
    }
}

impl PreparedMemoryFork<'_> {
    #[must_use]
    pub fn inherited(&self) -> Vec<InheritedAttachment> {
        self.plan
            .attachments
            .iter()
            .map(|attachment| InheritedAttachment {
                parent: attachment.parent,
                child: attachment.child,
            })
            .collect()
    }

    #[must_use]
    pub fn bindings(&self) -> Vec<ForkAttachmentPlan> {
        Self::binding_plans(&self.plan.attachments)
    }

    fn binding_plans(attachments: &[PlannedAttachment]) -> Vec<ForkAttachmentPlan> {
        attachments
            .iter()
            .map(|attachment| ForkAttachmentPlan {
                parent: attachment.parent,
                child: attachment.child,
                backing: attachment.backing,
            })
            .collect()
    }

    pub fn commit(self) -> Result<Vec<InheritedAttachment>, SharedMemoryError> {
        Self::commit_plan(self.namespace, &self.plan).map(|(inherited, _, _)| inherited)
    }

    fn commit_plan(
        namespace: &SharedMemoryNamespace,
        plan: &ForkPlan,
    ) -> Result<(Vec<InheritedAttachment>, NamespaceState, NamespaceState), SharedMemoryError> {
        let mut state = namespace
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.next_attachment != plan.expected_next
            || state
                .attachments
                .values()
                .any(|attachment| attachment.pid == plan.child)
            || state.attachments.len().saturating_add(plan.attachments.len()) > namespace.limits.attachments
        {
            return Err(SharedMemoryError::InvalidArgument);
        }
        for planned in &plan.attachments {
            if state.attachments.get(&planned.parent).copied()
                != Some(Attachment {
                    segment: planned.segment,
                    pid: plan.parent,
                })
                || SharedMemoryNamespace::segment(&state, planned.segment).is_err()
            {
                return Err(SharedMemoryError::InvalidArgument);
            }
        }

        let mut replacement = state.clone();
        replacement.next_attachment = plan.next;
        for planned in &plan.attachments {
            let segment = SharedMemoryNamespace::segment_mut(&mut replacement, planned.segment)?;
            segment.metadata.attaches += 1;
            segment.metadata.last_pid = plan.child;
            segment.metadata.attached_at = Some(plan.now);
            replacement.attachments.insert(
                planned.child,
                Attachment {
                    segment: planned.segment,
                    pid: plan.child,
                },
            );
        }
        let previous = std::mem::replace(&mut *state, replacement.clone());
        let inherited = plan
            .attachments
            .iter()
            .map(|attachment| InheritedAttachment {
                parent: attachment.parent,
                child: attachment.child,
            })
            .collect();
        Ok((inherited, previous, replacement))
    }
}

impl SharedMemoryNamespace {
    pub fn prepare_fork_owned(
        self: &Arc<Self>,
        parent: u32,
        child: u32,
        now: u64,
    ) -> Result<OwnedPreparedFork, SharedMemoryError> {
        let prepared = self.prepare_fork(parent, child, now)?;
        Ok(OwnedPreparedFork {
            namespace: Arc::clone(self),
            plan: prepared.plan.clone(),
        })
    }

    pub fn fork(&self, parent: u32, child: u32, now: u64) -> Result<Vec<InheritedAttachment>, SharedMemoryError> {
        self.prepare_fork(parent, child, now)?.commit()
    }

    pub fn prepare_fork(&self, parent: u32, child: u32, now: u64) -> Result<PreparedMemoryFork<'_>, SharedMemoryError> {
        if parent == child {
            return Err(SharedMemoryError::InvalidArgument);
        }
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.attachments.values().any(|attachment| attachment.pid == child) {
            return Err(SharedMemoryError::InvalidArgument);
        }
        let inherited: Vec<_> = state
            .attachments
            .iter()
            .filter(|(_, attachment)| attachment.pid == parent)
            .map(|(token, attachment)| (*token, attachment.segment))
            .collect();
        if state.attachments.len().saturating_add(inherited.len()) > self.limits.attachments {
            return Err(SharedMemoryError::ResourceLimit);
        }
        let expected_next = state.next_attachment;
        let mut next = expected_next;
        let mut attachments = Vec::with_capacity(inherited.len());
        for (parent_token, segment) in inherited {
            let child_token = next;
            next = next.checked_add(1).ok_or(SharedMemoryError::ResourceLimit)?;
            let metadata = Self::segment(&state, segment)?.metadata;
            let backing = SharedBackingRef {
                object: metadata.backing,
                offset: 0,
                length: Self::page_extent(metadata.size)?,
                write_shared: true,
            };
            attachments.push(PlannedAttachment {
                parent: parent_token,
                child: child_token,
                segment,
                backing,
            });
        }
        Ok(PreparedMemoryFork {
            namespace: self,
            plan: ForkPlan {
                parent,
                child,
                now,
                expected_next,
                next,
                attachments,
            },
        })
    }
}
