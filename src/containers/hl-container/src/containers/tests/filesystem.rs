use super::*;

#[tokio::test]
async fn file_records_survive_reopen_and_are_removed_atomically() {
    let temporary = tempfile::tempdir().unwrap();
    let config = Config::new(temporary.path());
    let repository = Arc::new(Disk::open(config.root.clone()).await.unwrap());
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_millis(40);
    let first = test_containers(repository, Arc::new(runtime)).await.unwrap();
    let created = first
        .create(spec("durable").restart(RestartPolicy::UnlessStopped))
        .await
        .unwrap();
    first.start("durable").await.unwrap();
    first.signal("durable", Signal::Terminate).await.unwrap();
    assert_eq!(first.wait("durable").await.unwrap(), ExitStatus::Code(0));
    drop(first);
    let reopened = test_containers(
        Arc::new(Disk::open(config.root).await.unwrap()),
        Arc::new(FakeRuntime::new(ExitStatus::Code(0))),
    )
    .await
    .unwrap();
    let durable = reopened.inspect("durable").await.unwrap();
    assert_eq!(durable.id, created.id);
    assert_eq!(durable.generation, 1);
    assert!(durable.restart.manually_stopped);
    assert_eq!(
        reopened.logs("durable").await.unwrap(),
        crate::Logs {
            stdout: b"fake-out\n".to_vec(),
            stderr: b"fake-err\n".to_vec()
        }
    );
    reopened.remove("durable").await.unwrap();
    assert!(reopened.list().await.unwrap().is_empty());
}
