//! Adding, modifying, and deleting the watches an epoll set holds.
use crate::epoll::{Epoll, EpollError, EpollInterest, EpollWatchKey, TargetObserver, Watch};
use hl_descriptor::{OperationLease, Readiness};
use std::sync::Arc;
impl Epoll {
    pub fn add(
        &self,
        target: OperationLease,
        interests: EpollInterest,
        data: u64,
    ) -> Result<EpollWatchKey, EpollError> {
        if !interests.valid() {
            return Err(EpollError::InvalidArgument);
        }
        self.prune_retired();
        let target = target.into_durable();
        let key = EpollWatchKey::from_lease(&target);
        {
            let state = self.inner.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            Self::validate_active(&state)?;
            if state.watches.iter().any(|watch| watch.key == key) {
                return Err(EpollError::AlreadyExists);
            }
            if state.watches.len() == self.inner.watch_limit {
                return Err(EpollError::ResourceLimit);
            }
        }
        let token = {
            let mut state = self.inner.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let token = state.next_token;
            state.next_token = state.next_token.wrapping_add(1).max(1);
            token
        };
        let observer = Arc::new(TargetObserver {
            epoll: Arc::downgrade(&self.inner),
            token,
        });
        let subscription = target.subscribe_readiness(observer)?;
        let mut state = self.inner.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::validate_active(&state)?;
        if state.watches.iter().any(|watch| watch.key == key) {
            drop(state);
            subscription.quiesce();
            return Err(EpollError::AlreadyExists);
        }
        if state.watches.len() == self.inner.watch_limit {
            drop(state);
            subscription.quiesce();
            return Err(EpollError::ResourceLimit);
        }
        let index = state.watches.len();
        state.watches.push(Watch {
            key,
            target,
            interests,
            data,
            observed: Readiness::default(),
            disabled: false,
            token,
            revision: 1,
            readiness_sequence: 0,
            queued: false,
            subscription,
        });
        state.token_index.insert(token, index);
        Self::wake(&self.inner, &mut state);
        drop(state);
        Self::refresh_token(&self.inner, token);
        Ok(key)
    }

    pub fn modify(&self, target: &OperationLease, interests: EpollInterest, data: u64) -> Result<(), EpollError> {
        if !interests.valid() {
            return Err(EpollError::InvalidArgument);
        }
        let key = EpollWatchKey::from_lease(target);
        let mut state = self.inner.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::validate_active(&state)?;
        let index = state
            .watches
            .iter()
            .position(|watch| watch.key == key)
            .ok_or(EpollError::NotFound)?;
        let token = state.watches[index].token;
        if interests.exclusive() || state.watches[index].interests.exclusive() {
            return Err(EpollError::InvalidArgument);
        }
        if state.watches[index].queued {
            state.ready.retain(|queued| *queued != token);
        }
        let watch = &mut state.watches[index];
        watch.interests = interests;
        watch.data = data;
        watch.revision = watch.revision.wrapping_add(1).max(1);
        watch.observed = Readiness::default();
        watch.disabled = false;
        watch.queued = false;
        Self::wake(&self.inner, &mut state);
        drop(state);
        Self::refresh_token(&self.inner, token);
        Ok(())
    }

    pub fn delete(&self, target: &OperationLease) -> Result<(), EpollError> {
        let key = EpollWatchKey::from_lease(target);
        let removed = {
            let mut state = self.inner.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            Self::validate_active(&state)?;
            let index = state
                .watches
                .iter()
                .position(|watch| watch.key == key)
                .ok_or(EpollError::NotFound)?;
            let removed = state.watches.remove(index);
            state.token_index.remove(&removed.token);
            Self::reindex_from(&mut state, index);
            state.ready.retain(|token| *token != removed.token);
            Self::wake(&self.inner, &mut state);
            removed
        };
        removed.subscription.quiesce();
        Ok(())
    }
}
