//! One process-local, bounded record of successful workspace lifecycle changes.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use hl_extension::{WorkspaceLifecycleAction, WorkspaceLifecycleChange};

const LIMIT: usize = 256;

#[derive(Default)]
struct Lifecycle {
    revision: u64,
    changes: VecDeque<WorkspaceLifecycleChange>,
}

fn lifecycle() -> &'static Mutex<Lifecycle> {
    static LIFECYCLE: OnceLock<Mutex<Lifecycle>> = OnceLock::new();
    LIFECYCLE.get_or_init(|| Mutex::new(Lifecycle::default()))
}

pub(crate) fn revision() -> u64 {
    lifecycle()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .revision
}

pub(crate) fn since(revision: u64) -> Vec<WorkspaceLifecycleChange> {
    let mut changes: Vec<_> = lifecycle()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .changes
        .iter()
        .filter(|change| change.revision > revision)
        .cloned()
        .collect();
    if let Some(first) = changes.first_mut() {
        first.coalesced = first
            .coalesced
            .saturating_add(first.revision.saturating_sub(revision).saturating_sub(1));
    }
    changes
}

pub(crate) fn changed(workspace: &str, action: WorkspaceLifecycleAction) {
    let mut lifecycle = lifecycle()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    lifecycle.revision = lifecycle.revision.saturating_add(1).max(1);
    let revision = lifecycle.revision;
    lifecycle.changes.push_back(WorkspaceLifecycleChange {
        workspace: workspace.to_owned(),
        action,
        revision,
        coalesced: 0,
    });
    if lifecycle.changes.len() > LIMIT {
        lifecycle.changes.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ledger_is_bounded_and_reports_discarded_revisions() {
        let before = revision();
        for index in 0..258 {
            changed(&format!("overflow-{index}"), WorkspaceLifecycleAction::Update);
        }
        let bounded = since(before);
        assert_eq!(bounded.len(), LIMIT);
        assert_eq!(bounded[0].coalesced, 2);
        assert_eq!(revision(), bounded.last().expect("last change").revision);
    }
}
