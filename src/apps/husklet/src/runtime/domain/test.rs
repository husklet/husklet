use super::*;
use hl_ws::Arch;

#[test]
fn signatures_are_unambiguous_and_ignore_terminal_presentation() {
    let mut first = WorkspaceConfig::new("demo", "ubuntu:latest", Arch::Arm64);
    first.env.push(("AB".into(), "C".into()));
    let mut second = WorkspaceConfig::new("demo", "ubuntu:latest", Arch::Arm64);
    second.env.push(("A".into(), "BC".into()));
    assert_ne!(
        Configuration::new(&first).signature_for("runtime-a"),
        Configuration::new(&second).signature_for("runtime-a")
    );
    let signature = Configuration::new(&first).signature_for("runtime-a");
    first.terminal.font_size = Some(18);
    assert_eq!(signature, Configuration::new(&first).signature_for("runtime-a"));
    assert_ne!(signature, Configuration::new(&first).signature_for("runtime-b"));
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
fn domain_protocol_separates_an_absent_publication_from_a_wrong_one() {
    let root = tempfile::tempdir().unwrap();
    let protocol = PublishedProtocol::new(root.path());
    assert_eq!(protocol.state().unwrap(), Publication::Unpublished);
    std::fs::write(&protocol.path, "obsolete\n").unwrap();
    assert_eq!(protocol.state().unwrap(), Publication::Mismatched("obsolete".into()));
    protocol.publish().unwrap();
    assert_eq!(protocol.state().unwrap(), Publication::Compatible);
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
    assert!(error.to_string().contains("no verifiable configuration identity"));
}

/// A workspace whose runtime directory exists, with a bound socket standing in for a live domain.
fn live_domain(root: &std::path::Path) -> (WorkspaceConfig, Domain, std::os::unix::net::UnixListener) {
    let mut workspace = WorkspaceConfig::new("demo", "ubuntu", Arch::Arm64);
    workspace.storage = Some(root.to_owned());
    let domain = Domain::new(&workspace);
    std::fs::create_dir_all(&domain.directory).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(domain.socket()).unwrap();
    (workspace, domain, listener)
}

fn inode(path: &std::path::Path) -> u64 {
    std::os::unix::fs::MetadataExt::ino(&std::fs::symlink_metadata(path).unwrap())
}

#[test]
fn a_live_compatible_domain_is_served_and_never_replaced() {
    let root = tempfile::tempdir().unwrap();
    let (workspace, domain, _listener) = live_domain(root.path());
    PublishedProtocol::new(&domain.directory).publish().unwrap();
    PublishedConfiguration::new(&domain.directory)
        .publish(&workspace)
        .unwrap();
    let original = inode(&domain.socket());

    assert!(matches!(domain.decide(&workspace).unwrap(), Decision::Serve));
    // The whole start, not just the decision: a healthy domain is neither unlinked nor respawned,
    // even while it holds the lease it holds for its entire life.
    let _lease = Lease::acquire(&domain.directory.join("domain.lock")).unwrap();
    assert_eq!(domain.ensure(&workspace).unwrap(), domain.socket());
    assert_eq!(inode(&domain.socket()), original);
}

#[test]
fn a_live_domain_with_changed_settings_is_replaced() {
    let root = tempfile::tempdir().unwrap();
    let (mut workspace, domain, _listener) = live_domain(root.path());
    PublishedProtocol::new(&domain.directory).publish().unwrap();
    PublishedConfiguration::new(&domain.directory)
        .publish(&workspace)
        .unwrap();
    workspace.env.push(("MODE".into(), "changed".into()));

    let Decision::Replace(_, reason) = domain.decide(&workspace).unwrap() else {
        panic!("a domain with a stale configuration must be replaced");
    };
    assert!(reason.contains("configuration"));
}

#[test]
fn a_domain_that_has_not_published_yet_is_awaited_rather_than_judged_incompatible() {
    let root = tempfile::tempdir().unwrap();
    let (workspace, domain, _listener) = live_domain(root.path());
    PublishedConfiguration::new(&domain.directory)
        .publish(&workspace)
        .unwrap();
    let directory = domain.directory.clone();
    let publishing = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        PublishedProtocol::new(&directory).publish().unwrap();
    });

    assert!(matches!(domain.decide(&workspace).unwrap(), Decision::Serve));

    publishing.join().unwrap();
    assert!(domain.socket().exists());
}

#[test]
fn a_replacement_names_the_domain_it_replaces_and_why() {
    let root = tempfile::tempdir().unwrap();
    let (workspace, domain, _listener) = live_domain(root.path());
    std::fs::write(PublishedProtocol::new(&domain.directory).path, "1\n").unwrap();

    let Decision::Replace(_, reason) = domain.decide(&workspace).unwrap() else {
        panic!("a domain speaking another protocol must be replaced");
    };
    assert!(reason.contains("protocol 1"));
    assert!(reason.contains(PROTOCOL));
    // Deciding to replace does not itself unlink anything; the stop comes first.
    assert!(domain.socket().exists());
}

#[test]
fn deciding_to_start_leaves_a_stale_socket_for_the_lease_to_clear() {
    let root = tempfile::tempdir().unwrap();
    let (workspace, domain, listener) = live_domain(root.path());
    drop(listener);

    let Decision::Start(reason) = domain.decide(&workspace).unwrap() else {
        panic!("an unreachable socket has no live owner");
    };
    assert!(reason.contains("no live domain"));
    // Unlinking happens only after the domain lease is provably free, never on the decision.
    assert!(domain.socket().exists());
}

#[test]
fn a_socket_is_never_unlinked_while_a_domain_still_holds_the_lease() {
    let root = tempfile::tempdir().unwrap();
    let (workspace, domain, listener) = live_domain(root.path());
    // The hazard: the socket is unreachable but its owner is alive and finishing up. This is the
    // shape that produced a bare "no such file" against a domain nobody reported dead.
    drop(listener);
    let _lease = Lease::acquire(&domain.directory.join("domain.lock")).unwrap();

    let error = domain
        .reserve(&workspace, std::time::Duration::from_millis(200))
        .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(error.to_string().contains("demo"));
    assert!(error.to_string().contains("domain.lock"));
    assert!(domain.socket().exists());
}

#[test]
fn a_socket_no_domain_owns_is_cleared_for_the_replacement() {
    let root = tempfile::tempdir().unwrap();
    let (workspace, domain, listener) = live_domain(root.path());
    drop(listener);

    domain
        .reserve(&workspace, std::time::Duration::from_millis(200))
        .unwrap();

    assert!(!domain.socket().exists());
}

#[test]
fn a_restart_marks_its_boundary_in_the_appended_domain_log() {
    let root = tempfile::tempdir().unwrap();
    let workspace = WorkspaceConfig::new("demo", "ubuntu", Arch::Arm64);
    let path = root.path().join("domain.log");
    std::fs::write(&path, "previous domain output\n").unwrap();
    let log = std::fs::OpenOptions::new().append(true).open(&path).unwrap();

    Domain::mark_restart(&log, &workspace, "stale socket").unwrap();

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("previous domain output\n"));
    assert!(text.contains("starting workspace 'demo' execution domain"));
    assert!(text.contains("stale socket"));
}

#[test]
fn domain_startup_reports_an_exited_worker_without_waiting_for_timeout() {
    let root = tempfile::tempdir().unwrap();
    let mut workspace = WorkspaceConfig::new("demo", "ubuntu", Arch::Arm64);
    workspace.storage = Some(root.path().to_owned());
    let domain = Domain::new(&workspace);
    let child = std::process::Command::new("/usr/bin/false").spawn().unwrap();
    let started = std::time::Instant::now();
    let error = domain
        .wait_for_start(child, std::time::Duration::from_secs(10))
        .unwrap_err();
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert!(error.to_string().contains("exited before publishing"));
    assert!(error.to_string().contains("domain.log"));
}

#[test]
fn restore_summary_is_consumed_once_and_names_each_failure() {
    let root = tempfile::tempdir().unwrap();
    let mut workspace = WorkspaceConfig::new("demo", "ubuntu", Arch::Arm64);
    workspace.storage = Some(root.path().to_owned());
    let summary = RestoreSummary::new(&workspace);

    summary
        .publish(&[
            "terminal pane-1: restored process cannot be attached".into(),
            "container database: checkpoint object is incomplete".into(),
        ])
        .unwrap();

    let text = summary.take().unwrap().unwrap();
    assert!(text.contains("workspace restored with 2 failure(s)"));
    assert!(text.contains("terminal pane-1: restored process cannot be attached"));
    assert!(text.contains("container database: checkpoint object is incomplete"));
    assert_eq!(summary.take().unwrap(), None);
}

#[test]
fn successful_restore_removes_an_obsolete_failure_summary() {
    let root = tempfile::tempdir().unwrap();
    let mut workspace = WorkspaceConfig::new("demo", "ubuntu", Arch::Arm64);
    workspace.storage = Some(root.path().to_owned());
    let summary = RestoreSummary::new(&workspace);

    summary.publish(&["old failure".into()]).unwrap();
    summary.publish(&[]).unwrap();

    assert_eq!(summary.take().unwrap(), None);
}
