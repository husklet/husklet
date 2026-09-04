//! Process-local invalidation for durable extension rosters.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, PoisonError};

use crate::config::WorkspaceConfig;

static REVISIONS: OnceLock<Mutex<BTreeMap<PathBuf, u64>>> = OnceLock::new();

#[must_use]
pub fn inventory_revision(workspace: &WorkspaceConfig) -> u64 {
    *revisions()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&key(workspace))
        .unwrap_or(&0)
}

pub(crate) fn publish_inventory_change(workspace: &WorkspaceConfig) {
    let mut revisions = revisions().lock().unwrap_or_else(PoisonError::into_inner);
    let revision = revisions.entry(key(workspace)).or_default();
    *revision = revision.saturating_add(1);
}

fn revisions() -> &'static Mutex<BTreeMap<PathBuf, u64>> {
    REVISIONS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn key(workspace: &WorkspaceConfig) -> PathBuf {
    workspace.storage_dir(&crate::paths::hl_root())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_workspace_mutation_invalidates_only_its_own_roster() {
        let root = tempfile::tempdir().unwrap();
        let mut first = WorkspaceConfig::new("first", "image", hl_ws::Arch::Amd64);
        first.ws.storage = Some(root.path().join("first"));
        let mut second = WorkspaceConfig::new("second", "image", hl_ws::Arch::Amd64);
        second.ws.storage = Some(root.path().join("second"));

        let before = inventory_revision(&first);
        publish_inventory_change(&first);
        assert_eq!(inventory_revision(&first), before + 1);
        assert_eq!(inventory_revision(&second), 0);
    }
}
