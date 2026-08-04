use super::{Disk, Execs, Logs as _};
use crate::{
    ContainerId, Error, Exec, ExecSpec, ExecState, ExitStatus, JournalId, Process, Stream,
};
use std::io::Write as _;

#[tokio::test]
async fn journal_preserves_order_and_cursor_across_reopen() {
    let root = tempfile::tempdir().unwrap();
    let id = JournalId::container(ContainerId::new());
    let disk = Disk::open(root.path().to_owned()).await.unwrap();
    let first = disk.append(&id, Stream::Stdout, b"first").await.unwrap();
    let second = disk.append(&id, Stream::Stderr, b"second").await.unwrap();
    assert_eq!(first.sequence, 1);
    assert_eq!(second.sequence, 2);
    assert_eq!(disk.after(&id, 0, 1).await.unwrap(), vec![first]);
    drop(disk);

    let reopened = Disk::open(root.path().to_owned()).await.unwrap();
    assert_eq!(reopened.cursor(&id).await.unwrap(), 2);
    assert_eq!(reopened.after(&id, 1, 8).await.unwrap(), vec![second]);
    let logs = reopened.read(&id).await.unwrap();
    assert_eq!(logs.stdout, b"first");
    assert_eq!(logs.stderr, b"second");
}

#[tokio::test]
async fn journal_rejects_truncated_records_on_reopen() {
    let root = tempfile::tempdir().unwrap();
    let id = JournalId::container(ContainerId::new());
    let disk = Disk::open(root.path().to_owned()).await.unwrap();
    disk.append(&id, Stream::Stdout, b"complete").await.unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(disk.log_path(&id))
        .unwrap()
        .write_all(&[1, 2, 3])
        .unwrap();
    drop(disk);
    assert!(matches!(
        Disk::open(root.path().to_owned()).await,
        Err(Error::Corrupt(_))
    ));
}

#[tokio::test]
async fn exec_inspect_and_state_survive_reopen() {
    let root = tempfile::tempdir().unwrap();
    let parent = ContainerId::new();
    let mut exec = Exec::new(
        parent,
        ExecSpec::new(Process::new("/bin/echo").args(["ok"])),
    );
    let disk = Disk::open(root.path().to_owned()).await.unwrap();
    disk.insert(&exec).await.unwrap();
    exec.state = ExecState::Running {
        process_id: 73,
        started_at_ms: 100,
    };
    disk.replace(&exec).await.unwrap();
    exec.state = ExecState::Exited {
        result: ExitStatus::Code(0),
        finished_at_ms: 120,
        process_id: Some(73),
    };
    disk.replace(&exec).await.unwrap();
    drop(disk);

    let reopened = Disk::open(root.path().to_owned()).await.unwrap();
    assert_eq!(reopened.get(&exec.id).await.unwrap(), Some(exec.clone()));
    assert_eq!(reopened.list().await.unwrap(), vec![exec]);
}

#[tokio::test]
async fn exec_parent_cleanup_only_removes_matching_records() {
    let root = tempfile::tempdir().unwrap();
    let removed_parent = ContainerId::new();
    let kept_parent = ContainerId::new();
    let removed = Exec::new(
        removed_parent.clone(),
        ExecSpec::new(Process::new("/bin/one")),
    );
    let kept = Exec::new(kept_parent, ExecSpec::new(Process::new("/bin/two")));
    let disk = Disk::open(root.path().to_owned()).await.unwrap();
    disk.insert(&removed).await.unwrap();
    disk.insert(&kept).await.unwrap();

    disk.remove_parent(&removed_parent).await.unwrap();
    drop(disk);
    let reopened = Disk::open(root.path().to_owned()).await.unwrap();
    assert_eq!(reopened.get(&removed.id).await.unwrap(), None);
    assert_eq!(reopened.list().await.unwrap(), vec![kept]);
}

#[tokio::test]
async fn exec_remove_is_durable() {
    let root = tempfile::tempdir().unwrap();
    let exec = Exec::new(ContainerId::new(), ExecSpec::new(Process::new("/bin/true")));
    let disk = Disk::open(root.path().to_owned()).await.unwrap();
    disk.insert(&exec).await.unwrap();
    Execs::remove(&disk, &exec.id).await.unwrap();
    drop(disk);

    let reopened = Disk::open(root.path().to_owned()).await.unwrap();
    assert_eq!(reopened.get(&exec.id).await.unwrap(), None);
    assert!(matches!(
        Execs::remove(&reopened, &exec.id).await,
        Err(Error::NotFound(_))
    ));
}

#[tokio::test]
async fn exec_records_reject_unknown_versions_and_clean_temporary_files() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("state/execs");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("abandoned.tmp"), b"partial").unwrap();
    let exec = Exec::new(ContainerId::new(), ExecSpec::new(Process::new("/bin/true")));
    std::fs::write(
        directory.join(format!("{}.json", exec.id)),
        serde_json::json!({ "version": 999, "exec": exec }).to_string(),
    )
    .unwrap();

    let disk = Disk::open(root.path().to_owned()).await.unwrap();
    assert!(!directory.join("abandoned.tmp").exists());
    assert!(matches!(disk.list().await, Err(Error::Corrupt(_))));
}
