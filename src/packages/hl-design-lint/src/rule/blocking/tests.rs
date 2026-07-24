use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{rule::Rule, AsyncBlocking, Workspace};

fn findings(source: &str) -> Vec<crate::Finding> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "hl-async-lint-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    let path: PathBuf = root.join("src/lib.rs");
    fs::write(&path, source).unwrap();
    let values = AsyncBlocking
        .check(&Workspace::load([path]).unwrap())
        .unwrap();
    fs::remove_dir_all(root).unwrap();
    values
}

#[test]
fn detects_qualified_and_aliased_blocking_operations_only_in_async_scopes() {
    let values = findings(
        r#"
use std::{fs as disk, thread::sleep as pause};
use std::process::Command as HostCommand;

async fn bad() {
    pause(std::time::Duration::ZERO);
    let _ = disk::read("x");
    let mut command = HostCommand::new("git");
    let _ = command.output();
    let _ = disk::File::open("x");
    let _ = disk::OpenOptions::new().read(true).open("x");
}
fn synchronous() {
    pause(std::time::Duration::ZERO);
    let _ = disk::read("x");
}
"#,
    );
    assert_eq!(values.len(), 5);
    assert!(values.iter().any(|value| value.message.contains("sleep")));
    assert!(values
        .iter()
        .any(|value| value.message.contains("filesystem")));
    assert!(values.iter().any(|value| value.message.contains("process")));
}

#[test]
fn ignores_construction_async_apis_tests_and_blocking_boundaries() {
    let values = findings(
        r#"
async fn safe() {
    let _ = std::process::Command::new("git");
    let _ = tokio::process::Command::new("git").output().await;
    let _ = tokio::fs::read("x").await;
    tokio::task::spawn_blocking(|| {
        std::thread::sleep(std::time::Duration::ZERO);
        let _ = std::fs::read("x");
        let _ = std::process::Command::new("git").output();
    }).await;
}
#[cfg(test)]
async fn fixture() { let _ = std::fs::read("x"); }
"#,
    );
    assert!(values.is_empty(), "{values:#?}");
}

#[test]
fn diagnoses_only_proven_blocking_locks_and_runtime_methods() {
    let values = findings(
        r#"
use std::sync::Mutex as StdMutex;
use tokio::sync::{mpsc::Receiver, Mutex as AsyncMutex};

async fn locks(
    std_lock: &StdMutex<u8>,
    async_lock: &AsyncMutex<u8>,
    channel: &mut Receiver<u8>,
) {
    let _ = std_lock.lock();
    let _ = async_lock.lock().await;
    let _ = channel.blocking_recv();
}
"#,
    );
    assert_eq!(values.len(), 2);
    assert!(values.iter().any(|value| value.subject == "lock"));
    assert!(values.iter().any(|value| value.subject == "blocking_recv"));
}

#[test]
fn reports_live_synchronous_guard_across_await_but_not_dropped_guard() {
    let values = findings(
        r#"
use std::sync::Mutex;
async fn guards(lock: &Mutex<Vec<u8>>) {
    {
        let guard = lock.lock().unwrap();
        ready().await;
        consume(&guard);
    }
    {
        let guard = lock.lock().unwrap();
        drop(guard);
        ready().await;
    }
}
"#,
    );
    assert_eq!(
        values
            .iter()
            .filter(|value| value.message.contains("held across await"))
            .count(),
        1
    );
}

#[test]
fn async_closures_are_checked_without_guessing_from_method_names() {
    let values = findings(
        r#"
struct Builder;
impl Builder {
    fn blocking_recv(&self) {}
    fn lock(&self) {}
    fn output(&self) {}
}
fn closures(builder: Builder) {
    let _future = async || {
        std::thread::sleep(std::time::Duration::ZERO);
        builder.blocking_recv();
        builder.lock();
        builder.output();
    };
}
"#,
    );
    assert_eq!(values.len(), 1);
    assert!(values[0].message.contains("sleep"));
}
