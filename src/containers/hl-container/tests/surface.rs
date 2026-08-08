use hl_container::{Config, ContainerSpec, ContainerState, Containers, Isolation, Persistence, Process, Sandbox};

#[test]
fn general_linux_workloads_default_to_sentry_routing() {
    assert_eq!(
        Isolation::default(),
        Isolation {
            sandbox: Sandbox::SentryOnly,
            read_only_root: false,
            network_isolated: false,
            seccomp_baseline: hl_container::SeccompBaseline::Container,
        }
    );
}

#[tokio::test]
async fn public_headless_surface_creates_inspects_and_removes() {
    let state = tempfile::tempdir().unwrap();
    let containers = Containers::builder(Config::new(state.path()).persistence(Persistence::Memory))
        .build()
        .await
        .unwrap();

    let created = containers
        .create(ContainerSpec::from_directory("/rootfs", Process::new("/bin/true")).name("surface"))
        .await
        .unwrap();

    assert_eq!(created.state, ContainerState::Created);
    assert_eq!(containers.inspect("surface").await.unwrap(), created);
    assert_eq!(containers.list().await.unwrap(), vec![created.clone()]);
    assert_eq!(containers.remove("surface").await.unwrap(), created);
}
