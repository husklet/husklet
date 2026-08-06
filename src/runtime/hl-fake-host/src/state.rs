use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Condvar, Mutex};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ResourceKind {
    File,
    Directory,
    Mapping,
    Process,
    Socket,
    Transport,
    Pin,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Fault {
    Interrupted,
    WouldBlock,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FakeHostError {
    Fault(Fault),
    InvalidResource,
    Closed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Call {
    pub sequence: usize,
    pub capability: &'static str,
    pub operation: &'static str,
    pub resource: u64,
    pub requested: usize,
    pub completed: usize,
    pub fault: Option<Fault>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResourceCounts(BTreeMap<ResourceKind, usize>);

impl ResourceCounts {
    #[must_use]
    pub fn get(&self, kind: ResourceKind) -> usize {
        self.0.get(&kind).copied().unwrap_or(0)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.values().all(|count| *count == 0)
    }
}

struct State {
    next_resource: u64,
    calls: Vec<Call>,
    fault_at: BTreeMap<usize, Fault>,
    resources: BTreeMap<ResourceKind, BTreeSet<u64>>,
    barriers: BTreeMap<String, bool>,
}

struct HostContext {
    identity: u64,
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Clone)]
pub struct FakeHost {
    shared: Arc<HostContext>,
}

impl std::fmt::Debug for FakeHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FakeHost")
            .field("identity", &self.identity())
            .finish_non_exhaustive()
    }
}

impl FakeHost {
    #[must_use]
    pub fn new(identity: u64) -> Self {
        Self {
            shared: Arc::new(HostContext {
                identity,
                state: Mutex::new(State {
                    next_resource: 1,
                    calls: Vec::new(),
                    fault_at: BTreeMap::new(),
                    resources: BTreeMap::new(),
                    barriers: BTreeMap::new(),
                }),
                changed: Condvar::new(),
            }),
        }
    }

    #[must_use]
    pub fn identity(&self) -> u64 {
        self.shared.identity
    }

    pub fn fail_at(&self, sequence: usize, fault: Fault) {
        self.lock().fault_at.insert(sequence, fault);
    }

    #[must_use]
    pub fn transcript(&self) -> Vec<Call> {
        self.lock().calls.clone()
    }

    /// Stable, pointer-free rows suitable for C/Rust differential fixtures.
    #[must_use]
    pub fn differential_transcript(&self) -> Vec<String> {
        self.transcript()
            .into_iter()
            .map(|call| {
                format!(
                    "{}\t{}\t{}\t{}\t{}\t{}\t{:?}",
                    call.sequence,
                    call.capability,
                    call.operation,
                    call.resource,
                    call.requested,
                    call.completed,
                    call.fault
                )
            })
            .collect()
    }

    #[must_use]
    pub fn resources(&self) -> ResourceCounts {
        ResourceCounts(
            self.lock()
                .resources
                .iter()
                .map(|(kind, resources)| (*kind, resources.len()))
                .collect(),
        )
    }

    pub fn release_barrier(&self, name: impl Into<String>) {
        self.lock().barriers.insert(name.into(), true);
        self.shared.changed.notify_all();
    }

    pub fn wait_barrier(&self, name: &str) {
        let state = self.lock();
        drop(
            self.shared
                .changed
                .wait_while(state, |state| !state.barriers.get(name).copied().unwrap_or(false))
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
    }

    pub fn allocate(&self, capability: &'static str, kind: ResourceKind) -> Result<u64, FakeHostError> {
        let resource = {
            let mut state = self.lock();
            let resource = state.next_resource;
            state.next_resource = state
                .next_resource
                .checked_add(1)
                .ok_or(FakeHostError::InvalidResource)?;
            resource
        };
        self.call(capability, "open", resource, 0, 0)?;
        self.lock().resources.entry(kind).or_default().insert(resource);
        Ok(resource)
    }

    pub fn release(&self, capability: &'static str, kind: ResourceKind, resource: u64) -> Result<(), FakeHostError> {
        self.call(capability, "close", resource, 0, 0)?;
        let mut state = self.lock();
        if resource == 0 || !state.resources.entry(kind).or_default().remove(&resource) {
            return Err(FakeHostError::InvalidResource);
        }
        Ok(())
    }

    pub fn record(
        &self,
        capability: &'static str,
        operation: &'static str,
        resource: u64,
        requested: usize,
        completed: usize,
    ) -> Result<(), FakeHostError> {
        self.call(capability, operation, resource, requested, completed)
    }

    fn call(
        &self,
        capability: &'static str,
        operation: &'static str,
        resource: u64,
        requested: usize,
        completed: usize,
    ) -> Result<(), FakeHostError> {
        let mut state = self.lock();
        let sequence = state.calls.len() + 1;
        let fault = state.fault_at.remove(&sequence);
        state.calls.push(Call {
            sequence,
            capability,
            operation,
            resource,
            requested,
            completed: if fault.is_some() { 0 } else { completed },
            fault,
        });
        fault.map_or(Ok(()), |fault| Err(FakeHostError::Fault(fault)))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.shared.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
