use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Mutex;

use hl_descriptor::{DescriptionIdentity, ObjectKind, OperationLease};
use hl_event::{Epoll, EpollError, EpollInterest, EpollWatchKey};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphError {
    InvalidArgument,
    Loop,
    ResourceLimit,
    Event(EpollError),
}

impl From<EpollError> for GraphError {
    fn from(error: EpollError) -> Self {
        Self::Event(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EdgeSnapshot {
    pub source: DescriptionIdentity,
    pub target: DescriptionIdentity,
    pub watches: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphSnapshot {
    pub node_limit: usize,
    pub edges: Vec<EdgeSnapshot>,
}

struct GraphState {
    edges: BTreeMap<DescriptionIdentity, BTreeMap<DescriptionIdentity, u32>>,
}

/// Composition-owned nested-epoll ownership graph.
pub struct OwnershipGraph {
    node_limit: usize,
    state: Mutex<GraphState>,
}

impl OwnershipGraph {
    pub fn new(node_limit: usize) -> Result<Self, GraphError> {
        if node_limit == 0 {
            return Err(GraphError::InvalidArgument);
        }
        Ok(Self {
            node_limit,
            state: Mutex::new(GraphState { edges: BTreeMap::new() }),
        })
    }

    pub fn add(
        &self,
        source: &OperationLease,
        epoll: &Epoll,
        target: OperationLease,
        interests: EpollInterest,
        data: u64,
    ) -> Result<EpollWatchKey, GraphError> {
        if source.object().kind() != ObjectKind::Poll {
            return Err(GraphError::InvalidArgument);
        }
        let source_id = source.description_identity();
        let target_id = target.description_identity();
        if source_id == target_id {
            return Err(GraphError::InvalidArgument);
        }
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let nested = target.object().kind() == ObjectKind::Poll;
        if nested {
            self.validate_edge(&state, source_id, target_id)?;
        }
        let key = epoll.add(target, interests, data)?;
        if nested {
            let count = state.edges.entry(source_id).or_default().entry(target_id).or_default();
            *count = count.checked_add(1).ok_or(GraphError::ResourceLimit)?;
        }
        Ok(key)
    }

    pub fn delete(&self, source: &OperationLease, epoll: &Epoll, target: &OperationLease) -> Result<(), GraphError> {
        let source_id = source.description_identity();
        let target_id = target.description_identity();
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        epoll.delete(target)?;
        if target.object().kind() == ObjectKind::Poll {
            Self::remove_edge(&mut state, source_id, target_id);
        }
        Ok(())
    }

    pub fn close(&self, identity: DescriptionIdentity) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.edges.remove(&identity);
        for targets in state.edges.values_mut() {
            targets.remove(&identity);
        }
        state.edges.retain(|_, targets| !targets.is_empty());
    }

    pub(crate) fn restore(&self, snapshot: &GraphSnapshot) {
        let mut edges = BTreeMap::new();
        for edge in &snapshot.edges {
            edges
                .entry(edge.source)
                .or_insert_with(BTreeMap::new)
                .insert(edge.target, edge.watches);
        }
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .edges = edges;
    }

    #[must_use]
    pub fn snapshot(&self) -> GraphSnapshot {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let edges = state
            .edges
            .iter()
            .flat_map(|(source, targets)| {
                targets.iter().map(|(target, watches)| EdgeSnapshot {
                    source: *source,
                    target: *target,
                    watches: *watches,
                })
            })
            .collect();
        GraphSnapshot {
            node_limit: self.node_limit,
            edges,
        }
    }

    fn validate_edge(
        &self,
        state: &GraphState,
        source: DescriptionIdentity,
        target: DescriptionIdentity,
    ) -> Result<(), GraphError> {
        let nodes = state
            .edges
            .iter()
            .flat_map(|(source, targets)| std::iter::once(*source).chain(targets.keys().copied()))
            .chain([source, target])
            .collect::<BTreeSet<_>>();
        if nodes.len() > self.node_limit {
            return Err(GraphError::ResourceLimit);
        }
        if Self::reaches(state, target, source, self.node_limit)? {
            return Err(GraphError::Loop);
        }
        Ok(())
    }

    fn reaches(
        state: &GraphState,
        start: DescriptionIdentity,
        sought: DescriptionIdentity,
        limit: usize,
    ) -> Result<bool, GraphError> {
        let mut queue = VecDeque::from([start]);
        let mut visited = BTreeSet::new();
        while let Some(node) = queue.pop_front() {
            if node == sought {
                return Ok(true);
            }
            if !visited.insert(node) {
                continue;
            }
            if visited.len() > limit {
                return Err(GraphError::ResourceLimit);
            }
            if let Some(targets) = state.edges.get(&node) {
                queue.extend(targets.keys().copied());
            }
        }
        Ok(false)
    }

    fn remove_edge(state: &mut GraphState, source: DescriptionIdentity, target: DescriptionIdentity) {
        let Some(targets) = state.edges.get_mut(&source) else {
            return;
        };
        let Some(count) = targets.get_mut(&target) else {
            return;
        };
        *count -= 1;
        if *count == 0 {
            targets.remove(&target);
        }
        if targets.is_empty() {
            state.edges.remove(&source);
        }
    }
}
