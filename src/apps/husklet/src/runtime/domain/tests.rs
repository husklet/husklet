use super::*;

#[test]
fn signatures_are_unambiguous_and_ignore_terminal_presentation() {
    let mut first = WorkspaceConfig::new("demo", "ubuntu:latest", Arch::Arm64);
    first.env.push(("AB".into(), "C".into()));
    let mut second = WorkspaceConfig::new("demo", "ubuntu:latest", Arch::Arm64);
    second.env.push(("A".into(), "BC".into()));
    assert_ne!(
        Configuration::new(&first).signature(),
        Configuration::new(&second).signature()
    );
    let signature = Configuration::new(&first).signature();
    first.terminal.font_size = Some(18);
    assert_eq!(signature, Configuration::new(&first).signature());
    assert_eq!(signature.len(), 64);
    assert!(!signature.contains("ubuntu"));
    assert!(!signature.contains("AB"));
}

#[test]
fn workspace_domain_uses_workspace_storage() {
    let root = tempfile::tempdir().unwrap();
    let mut workspace = WorkspaceConfig::new("demo", "ubuntu", Arch::Arm64);
    workspace.storage = Some(root.path().join("custom"));
    assert_eq!(
        Domain::new(&workspace).socket(),
        root.path().join("custom/runtime/domain.sock")
    );
}

#[test]
fn domain_protocol_rejects_missing_and_incompatible_publications() {
    let root = tempfile::tempdir().unwrap();
    let protocol = PublishedProtocol::new(root.path());
    assert!(!protocol.compatible().unwrap());
    std::fs::write(&protocol.path, "obsolete\n").unwrap();
    assert!(!protocol.compatible().unwrap());
    protocol.publish().unwrap();
    assert!(protocol.compatible().unwrap());
    protocol.remove().unwrap();
    assert!(!protocol.path.exists());
}

#[test]
fn published_configuration_is_secret_free_and_detects_changes() {
    let root = tempfile::tempdir().unwrap();
    let publication = PublishedConfiguration::new(root.path());
    let mut workspace = WorkspaceConfig::new("demo", "ubuntu", Arch::Arm64);
    workspace.env.push(("TOKEN".into(), "secret-value".into()));
    publication.publish(&workspace).unwrap();
    publication.validate(&workspace).unwrap();
    let digest = std::fs::read_to_string(&publication.path).unwrap();
    assert_eq!(digest.len(), 64);
    assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert!(!digest.contains("secret-value"));
    workspace.env.push(("MODE".into(), "changed".into()));
    let error = publication.validate(&workspace).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    publication.remove().unwrap();
    assert!(!publication.path.exists());
}

#[test]
fn live_domain_without_configuration_identity_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let publication = PublishedConfiguration::new(root.path());
    let workspace = WorkspaceConfig::new("demo", "ubuntu", Arch::Arm64);
    let error = publication.validate(&workspace).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert!(error
        .to_string()
        .contains("no verifiable configuration identity"));
}

#[test]
fn domain_startup_reports_an_exited_worker_without_waiting_for_timeout() {
    let root = tempfile::tempdir().unwrap();
    let mut workspace = WorkspaceConfig::new("demo", "ubuntu", Arch::Arm64);
    workspace.storage = Some(root.path().to_owned());
    let domain = Domain::new(&workspace);
    let child = std::process::Command::new("/usr/bin/false")
        .spawn()
        .unwrap();
    let started = std::time::Instant::now();
    let error = domain
        .wait_for_start(child, std::time::Duration::from_secs(10))
        .unwrap_err();
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert!(error.to_string().contains("exited before publishing"));
    assert!(error.to_string().contains("domain.log"));
}

#[tokio::test]
async fn migration_removes_only_inactive_pre_domain_terminals() {
    let root = tempfile::tempdir().unwrap();
    let containers = Containers::builder(Config::new(root.path()))
        .build()
        .await
        .unwrap();
    containers
        .create(
            ContainerSpec::from_directory("/", hl_container::Process::new("/bin/true"))
                .name("legacy-terminal"),
        )
        .await
        .unwrap();
    containers
        .create(
            ContainerSpec::from_directory("/", hl_container::Process::new("/bin/true"))
                .name(CONTAINER)
                .label(SIGNATURE, "current"),
        )
        .await
        .unwrap();
    Runtime::remove_legacy_terminals(&containers).await.unwrap();
    assert!(matches!(
        containers.inspect("legacy-terminal").await,
        Err(hl_container::Error::NotFound(_))
    ));
    assert_eq!(
        containers
            .inspect(CONTAINER)
            .await
            .unwrap()
            .spec
            .name
            .as_deref(),
        Some(CONTAINER)
    );
}
