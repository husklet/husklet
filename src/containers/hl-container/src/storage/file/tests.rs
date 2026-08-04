use super::{Disk, Execs, Logs as _};
use crate::{ContainerId, Error, Exec, ExecSpec, ExecState, ExitStatus, JournalId, Process, Stream};
use std::io::Write as _;
use std::sync::{Arc, Barrier, mpsc};
use std::time::{Duration, Instant};

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
async fn journal_order() {
    let root = tempfile::tempdir().unwrap();
    let disk = Disk::open(root.path().to_owned()).await.unwrap();
    let id = JournalId::container(ContainerId::new());
    let barrier = Arc::new(Barrier::new(9));
    let mut workers = Vec::new();
    for byte in 0_u8..8 {
        let disk = disk.clone();
        let id = id.clone();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            disk.append_sync(&id, Stream::Stdout, vec![byte]).unwrap()
        }));
    }
    barrier.wait();
    let mut entries: Vec<_> = workers.into_iter().map(|worker| worker.join().unwrap()).collect();
    entries.sort_by_key(|entry| entry.sequence);
    assert_eq!(
        entries.iter().map(|entry| entry.sequence).collect::<Vec<_>>(),
        (1_u64..=8).collect::<Vec<_>>()
    );
    assert_eq!(disk.entries_sync(&id).unwrap(), entries);
}

#[tokio::test]
async fn journal_race() {
    let root = tempfile::tempdir().unwrap();
    let disk = Disk::open(root.path().to_owned()).await.unwrap();
    let mut ids = Vec::new();
    for byte in 0_u8..32 {
        let id = JournalId::container(ContainerId::new());
        ids.push(id.clone());
        let barrier = Arc::new(Barrier::new(3));
        let append = {
            let disk = disk.clone();
            let id = id.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                disk.append_sync(&id, Stream::Stdout, vec![byte])
            })
        };
        let remove = {
            let disk = disk.clone();
            let id = id.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                disk.remove_journal_sync(&id)
            })
        };
        barrier.wait();
        append.join().unwrap().unwrap();
        remove.join().unwrap().unwrap();
        let entries = disk.entries_sync(&id).unwrap();
        assert!(entries.is_empty() || entries.len() == 1);
        assert_eq!(disk.cursor_sync(&id).unwrap(), u64::try_from(entries.len()).unwrap());
    }
    drop(disk);
    let reopened = Disk::open(root.path().to_owned()).await.unwrap();
    for id in ids {
        let entries = reopened.entries_sync(&id).unwrap();
        assert_eq!(
            reopened.cursor_sync(&id).unwrap(),
            u64::try_from(entries.len()).unwrap()
        );
    }
}

#[tokio::test]
async fn striped_progress() {
    let root = tempfile::tempdir().unwrap();
    let disk = Disk::open(root.path().to_owned()).await.unwrap();
    let blocked = JournalId::container(ContainerId::new());
    let independent = loop {
        let candidate = JournalId::container(ContainerId::new());
        if Disk::journal_slot(&candidate) != Disk::journal_slot(&blocked) {
            break candidate;
        }
    };
    let stripe = disk.journal_lock(&blocked).unwrap();
    let (send, receive) = mpsc::channel();
    let worker = {
        let disk = disk.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            disk.append_sync(&independent, Stream::Stdout, b"independent".to_vec())
                .unwrap();
            send.send(started.elapsed()).unwrap();
        })
    };
    let elapsed = receive.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(elapsed < Duration::from_secs(1));
    worker.join().unwrap();

    let (send, receive) = mpsc::channel();
    let worker = {
        let disk = disk.clone();
        let blocked = blocked.clone();
        std::thread::spawn(move || {
            disk.append_sync(&blocked, Stream::Stdout, b"ordered".to_vec()).unwrap();
            send.send(()).unwrap();
        })
    };
    assert!(matches!(
        receive.recv_timeout(Duration::from_millis(50)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    drop(stripe);
    receive.recv_timeout(Duration::from_secs(1)).unwrap();
    worker.join().unwrap();
}

#[tokio::test]
async fn exec_inspect_and_state_survive_reopen() {
    let root = tempfile::tempdir().unwrap();
    let parent = ContainerId::new();
    let mut exec = Exec::new(parent, ExecSpec::new(Process::new("/bin/echo").args(["ok"])));
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
    let removed = Exec::new(removed_parent.clone(), ExecSpec::new(Process::new("/bin/one")));
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
