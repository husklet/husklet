use super::*;

#[tokio::test]
async fn start_restart_and_exec_resolve_managed_volume_names_at_launch() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_millis(100);
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    let volume = containers
        .volumes()
        .create(VolumeSpec::new("runtime-data"))
        .await
        .unwrap();
    containers
        .create(spec("volume-owner").mount(Mount::volume("runtime-data", "/data", Access::ReadWrite)))
        .await
        .unwrap();

    containers.start("volume-owner").await.unwrap();
    containers.wait("volume-owner").await.unwrap();
    containers.start("volume-owner").await.unwrap();
    let execution = containers
        .executions()
        .create("volume-owner", ExecSpec::new(Process::new("/bin/read-data")))
        .await
        .unwrap();
    let mut session = containers.executions().start(&execution.id).await.unwrap();
    while session.next().await.unwrap().is_some() {}
    containers.wait("volume-owner").await.unwrap();

    let launches = runtime.mounts.lock().unwrap().clone();
    assert_eq!(launches.len(), 3);
    for mounts in launches {
        assert!(mounts.contains(&(
            volume.path.clone(),
            std::path::PathBuf::from("/data"),
            Access::ReadWrite,
        )));
        for target in ["/etc/hosts", "/etc/resolv.conf", "/etc/hostname"] {
            assert!(
                mounts
                    .iter()
                    .any(|mount| { mount.1 == std::path::Path::new(target) && mount.2 == Access::ReadWrite })
            );
        }
    }
}
