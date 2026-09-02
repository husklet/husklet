//! A durable revision authority with a process-local, bounded change window.
//!
//! Only the monotonic revision is persisted. Changes remain a bounded invalidation
//! stream and are deliberately not replayed after a host restart.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use hl_extension::{WorkspaceLifecycleAction, WorkspaceLifecycleChange};
use fs2::FileExt as _;

const LIMIT: usize = 256;
const HEADER: &str = "husklet-workspace-lifecycle-v1\nrevision=";

struct Lifecycle {
    path: PathBuf,
    revision: u64,
    changes: VecDeque<WorkspaceLifecycleChange>,
}

impl Lifecycle {
    fn load(path: PathBuf) -> Self {
        let revision = match std::fs::read_to_string(&path) {
            Ok(text) => parse(&text).unwrap_or(u64::MAX),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(_) => u64::MAX,
        };
        Self { path, revision, changes: VecDeque::new() }
    }

    fn changed(&mut self, workspace: &str, action: WorkspaceLifecycleAction) -> bool {
        let lock_path = self.path.with_extension("revision.lock");
        let Ok(lock) = std::fs::OpenOptions::new().create(true).read(true).write(true).open(lock_path) else {
            return false;
        };
        if lock.lock_exclusive().is_err() {
            return false;
        }
        let persisted = match std::fs::read_to_string(&self.path) {
            Ok(text) => match parse(&text) {
                Some(revision) => revision,
                None => return false,
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && self.revision == 0 => 0,
            Err(_) => return false,
        };
        let Some(revision) = self.revision.max(persisted).checked_add(1) else { return false };
        let record = format!("{HEADER}{revision}\n");
        if hl_fs::File::from(self.path.clone()).replace(record.as_bytes()).is_err() {
            return false;
        }
        self.revision = revision;
        self.changes.push_back(WorkspaceLifecycleChange {
            workspace: workspace.to_owned(), action, revision, coalesced: 0,
        });
        if self.changes.len() > LIMIT {
            self.changes.pop_front();
        }
        true
    }

    fn since(&self, revision: u64) -> Vec<WorkspaceLifecycleChange> {
        let mut changes: Vec<_> = self.changes.iter()
            .filter(|change| change.revision > revision).cloned().collect();
        if let Some(first) = changes.first_mut() {
            first.coalesced = first.coalesced
                .saturating_add(first.revision.saturating_sub(revision).saturating_sub(1));
        }
        changes
    }
}

fn parse(text: &str) -> Option<u64> {
    text.strip_prefix(HEADER)?.strip_suffix('\n')?.parse().ok()
}

fn authority_path() -> PathBuf {
    #[cfg(test)]
    let root = std::env::temp_dir().join(format!("husklet-lifecycle-test-{}", std::process::id()));
    #[cfg(not(test))]
    let root = crate::paths::hl_root();
    let _ = std::fs::create_dir_all(&root);
    root.join("workspace-lifecycle.revision")
}

fn lifecycle() -> &'static Mutex<Lifecycle> {
    static LIFECYCLE: OnceLock<Mutex<Lifecycle>> = OnceLock::new();
    LIFECYCLE.get_or_init(|| Mutex::new(Lifecycle::load(authority_path())))
}

pub(crate) fn revision() -> u64 {
    lifecycle().lock().unwrap_or_else(std::sync::PoisonError::into_inner).revision
}

pub(crate) fn since(revision: u64) -> Vec<WorkspaceLifecycleChange> {
    lifecycle().lock().unwrap_or_else(std::sync::PoisonError::into_inner).since(revision)
}

pub(crate) fn changed(workspace: &str, action: WorkspaceLifecycleAction) {
    let published = lifecycle().lock().unwrap_or_else(std::sync::PoisonError::into_inner)
        .changed(workspace, action);
    if !published {
        hl_log::hl_error!(hl_log::tag::RUNTIME,
            "workspace lifecycle revision authority is unavailable; suppressing an ambiguous event");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use super::*;

    fn path(name: &str) -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().expect("temporary directory");
        let path = root.path().join(name);
        (root, path)
    }

    #[test]
    fn restart_keeps_revision_but_replays_no_events() {
        let (_root, path) = path("revision");
        let mut first = Lifecycle::load(path.clone());
        assert!(first.changed("one", WorkspaceLifecycleAction::Create));
        let revision = first.revision;
        let mut restarted = Lifecycle::load(path);
        assert_eq!(restarted.revision, revision);
        assert!(restarted.since(0).is_empty(), "the durable counter is not an audit log");
        assert!(restarted.changed("two", WorkspaceLifecycleAction::Start));
        assert_eq!(restarted.revision, revision + 1);
    }

    #[test]
    fn corrupt_authority_fails_closed_without_overwriting_or_emitting() {
        let (_root, path) = path("corrupt");
        std::fs::write(&path, b"revision=broken\n").expect("corruption fixture");
        let mut lifecycle = Lifecycle::load(path.clone());
        assert_eq!(lifecycle.revision, u64::MAX);
        assert!(!lifecycle.changed("unsafe", WorkspaceLifecycleAction::Remove));
        assert!(lifecycle.changes.is_empty());
        assert_eq!(std::fs::read(&path).expect("preserved"), b"revision=broken\n");
    }

    #[test]
    fn an_orphaned_temporary_write_cannot_replace_the_last_authority() {
        let (root, path) = path("revision");
        std::fs::write(&path, format!("{HEADER}41\n")).expect("authority");
        std::fs::write(root.path().join(".revision.replace-torn"), b"partial").expect("torn temporary");
        let lifecycle = Lifecycle::load(path);
        assert_eq!(lifecycle.revision, 41);
        assert!(lifecycle.changes.is_empty());
    }

    #[test]
    fn concurrent_publishers_serialize_unique_durable_revisions() {
        let (_root, path) = path("revision");
        let barrier = Arc::new(Barrier::new(9));
        let threads: Vec<_> = (0..8).map(|index| {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let mut lifecycle = Lifecycle::load(path);
                barrier.wait();
                assert!(lifecycle.changed(&format!("workspace-{index}"), WorkspaceLifecycleAction::Update));
                lifecycle.revision
            })
        }).collect();
        barrier.wait();
        let mut revisions: Vec<_> = threads.into_iter().map(|thread| thread.join().expect("publisher")).collect();
        revisions.sort_unstable();
        assert_eq!(revisions, (1..=8).collect::<Vec<_>>());
        assert_eq!(Lifecycle::load(path).revision, 8);
    }

    #[test]
    fn ledger_is_bounded_and_reports_discarded_revisions() {
        let (_root, path) = path("revision");
        let mut lifecycle = Lifecycle::load(path);
        let before = lifecycle.revision;
        for index in 0..258 {
            assert!(lifecycle.changed(&format!("overflow-{index}"), WorkspaceLifecycleAction::Update));
        }
        let bounded = lifecycle.since(before);
        assert_eq!(bounded.len(), LIMIT);
        assert_eq!(bounded[0].coalesced, 2);
        assert_eq!(lifecycle.revision, bounded.last().expect("last change").revision);
    }
}
