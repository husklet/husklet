use std::sync::Arc;

use hl_provider::{NamespaceError, ProviderRemoteRestore, ProviderResourceKey, RemoteId};

use super::registry::{Publication, Store};

pub(super) struct RemoteTransaction {
    store: Arc<Store>,
    expected: u64,
    previous: Arc<Publication>,
    replacement: Arc<Publication>,
    committed: Option<u64>,
    resumed: bool,
    rolled_back: bool,
}

impl RemoteTransaction {
    pub(super) fn new(
        store: Arc<Store>,
        expected: u64,
        previous: Arc<Publication>,
        replacement: Arc<Publication>,
    ) -> Self {
        Self {
            store,
            expected,
            previous,
            replacement,
            committed: None,
            resumed: false,
            rolled_back: false,
        }
    }
}

impl ProviderRemoteRestore for RemoteTransaction {
    fn remote(&mut self, key: ProviderResourceKey) -> Result<RemoteId, NamespaceError> {
        self.replacement
            .by_key
            .get(&key)
            .map(|resource| resource.remote)
            .ok_or(NamespaceError::InvalidSnapshot)
    }

    fn commit(&mut self) -> Result<(), NamespaceError> {
        let mut state = self.store.state.lock().map_err(|_| NamespaceError::InvalidSnapshot)?;
        if self.committed.is_some() || state.generation != self.expected {
            return Err(NamespaceError::InvalidSnapshot);
        }
        let generation = state.generation.checked_add(1).ok_or(NamespaceError::InvalidSnapshot)?;
        state.current = Arc::clone(&self.replacement);
        state.generation = generation;
        self.committed = Some(generation);
        Ok(())
    }

    fn rollback(&mut self) {
        if self.rolled_back {
            return;
        }
        if let Some(generation) = self.committed {
            let mut state = self.store.state.lock().unwrap_or_else(|error| error.into_inner());
            if state.generation == generation {
                state.current = Arc::clone(&self.previous);
                state.generation = state.generation.saturating_add(1);
            }
        }
        if !self.resumed {
            self.store.activity.thaw();
        }
        self.rolled_back = true;
    }

    fn resume(&mut self) -> Result<(), NamespaceError> {
        if self.committed.is_none() || self.resumed || self.rolled_back {
            return Err(NamespaceError::InvalidSnapshot);
        }
        self.store.activity.thaw();
        self.resumed = true;
        Ok(())
    }
}

impl Drop for RemoteTransaction {
    fn drop(&mut self) {
        if !self.resumed {
            self.rollback();
        }
    }
}
