use super::*;
use hl_ws::Arch;

#[test]
fn invalid_persisted_mount_is_rejected_before_domain_identity_decision() {
    let mut workspace = WorkspaceConfig::new("invalid-mount", "ubuntu:latest", Arch::Arm64);
    workspace.mounts.push(hl_ws::Mount {
        host: "/host".into(),
        container: "/guest/./alias".into(),
        ro: false,
    });

    let domain = Domain::new(&workspace);
    let Err(error) = domain.decide(&workspace) else {
        panic!("invalid persisted mount reached domain identity decision");
    };
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(error.to_string().contains("must be absolute and normalized"));
}

#[test]
fn duplicate_persisted_mount_is_rejected_before_domain_identity_decision() {
    let mut workspace = WorkspaceConfig::new("duplicate-mount", "ubuntu:latest", Arch::Arm64);
    for host in ["/first", "/second"] {
        workspace.mounts.push(hl_ws::Mount {
            host: host.into(),
            container: "/guest".into(),
            ro: false,
        });
    }

    let domain = Domain::new(&workspace);
    let Err(error) = domain.decide(&workspace) else {
        panic!("duplicate persisted mount reached domain identity decision");
    };
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(error.to_string().contains("duplicate mount target"));
}

#[test]
fn signatures_are_unambiguous_and_ignore_terminal_presentation() {
    let mut first = WorkspaceConfig::new("demo", "ubuntu:latest", Arch::Arm64);
    first.env.push(("AB".into(), "C".into()));
    let mut second = WorkspaceConfig::new("demo", "ubuntu:latest", Arch::Arm64);
    second.env.push(("A".into(), "BC".into()));
    assert_ne!(
        Configuration::new(&first).signature_for("runtime-a").unwrap(),
        Configuration::new(&second).signature_for("runtime-a").unwrap()
    );
    let signature = Configuration::new(&first).signature_for("runtime-a").unwrap();
    first.terminal.font_size = Some(18);
    assert_eq!(
        signature,
        Configuration::new(&first).signature_for("runtime-a").unwrap()
    );
    assert_ne!(
        signature,
        Configuration::new(&first).signature_for("runtime-b").unwrap()
    );
    assert_eq!(signature.len(), 64);
    assert!(!signature.contains("ubuntu"));
    assert!(!signature.contains("AB"));
}

#[test]
fn container_configuration_identity_ignores_runtime_build_identity() {
    let workspace = WorkspaceConfig::new("demo", "ubuntu:latest", Arch::Arm64);
    let configuration = Configuration::new(&workspace);

    assert_ne!(
        configuration.signature_for("runtime-a").unwrap(),
        configuration.signature_for("runtime-b").unwrap()
    );
    assert_eq!(
        configuration.identity_signature().unwrap(),
        configuration.identity_signature().unwrap()
    );
}

#[test]
fn legacy_container_compatibility_checks_rootfs_owning_launch_fields() {
    let mut workspace = WorkspaceConfig::new("demo", "ubuntu:latest", Arch::Arm64);
    workspace.cpus = Some(3);
    workspace.mounts.push(hl_ws::Mount {
        host: "/host".into(),
        container: "/guest".into(),
        ro: true,
    });
    let configuration = Configuration::new(&workspace);
    let mut spec = hl_container::ContainerSpec::from_directory("/tmp/root", hl_container::Process::new("/bin/sh"));
    spec.image = Some(workspace.image.parse().unwrap());
    let spec = configuration.container(spec, "legacy".into(), "configuration".into(), "runtime".into());
    assert!(configuration.legacy_container_compatible(&spec).unwrap());

    let mut incompatible = spec;
    incompatible.resources.cpu_count = 4;
    assert!(!configuration.legacy_container_compatible(&incompatible).unwrap());
}

#[test]
fn terminal_capability_defaults_remain_workspace_overridable() {
    let mut workspace = WorkspaceConfig::new("demo", "ubuntu:latest", Arch::Arm64);
    let defaults = Configuration::new(&workspace).environment();
    assert_eq!(defaults.get("TERM").map(String::as_str), Some("xterm-256color"));
    assert_eq!(defaults.get("COLORTERM").map(String::as_str), Some("truecolor"));

    workspace.env.push(("TERM".into(), "screen".into()));
    workspace.env.push(("COLORTERM".into(), "24bit".into()));
    let overridden = Configuration::new(&workspace).environment();
    assert_eq!(overridden.get("TERM").map(String::as_str), Some("screen"));
    assert_eq!(overridden.get("COLORTERM").map(String::as_str), Some("24bit"));
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
    assert!(publication.matches(&workspace).unwrap());
    let digest = std::fs::read_to_string(&publication.path).unwrap();
    assert_eq!(digest.len(), 64);
    assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert!(!digest.contains("secret-value"));
    workspace.env.push(("MODE".into(), "changed".into()));
    assert!(!publication.matches(&workspace).unwrap());
    publication.remove().unwrap();
    assert!(!publication.path.exists());
}

#[test]
fn live_domain_without_configuration_identity_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let publication = PublishedConfiguration::new(root.path());
    let workspace = WorkspaceConfig::new("demo", "ubuntu", Arch::Arm64);
    let error = publication.matches(&workspace).unwrap_err();
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
    assert!(domain.socket().exists(), "decision must not unlink the live domain");
}

#[test]
fn a_live_compatible_domain_without_configuration_identity_is_rejected_not_replaced() {
    let root = tempfile::tempdir().unwrap();
    let (workspace, domain, _listener) = live_domain(root.path());
    PublishedProtocol::new(&domain.directory).publish().unwrap();

    let Err(error) = domain.decide(&workspace) else {
        panic!("a live domain without authenticated configuration must not be used or replaced");
    };

    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert!(error.to_string().contains("no verifiable configuration identity"));
    assert!(domain.socket().exists());
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
fn failed_continue_still_closes_attachments_and_waits_for_the_domain() {
    let attachments = std::cell::Cell::new(false);
    let waited = std::cell::Cell::new(false);
    let error = Domain::handover_with(
        || Ok(()),
        || Err(io::Error::other("checkpoint rejected")),
        || {
            attachments.set(true);
            Ok(())
        },
        || {
            waited.set(true);
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "checkpoint rejected");
    assert!(
        attachments.get(),
        "a failed continue stranded its pane launcher workers"
    );
    assert!(waited.get(), "a failed continue skipped the domain lease wait");
}

#[test]
fn handover_holds_startup_ownership_until_attachments_and_domain_are_closed() {
    let root = tempfile::tempdir().unwrap();
    let startup = root.path().join("startup.lock");
    let domain = root.path().join("domain.lock");
    let owner = Lease::acquire(&domain).unwrap();
    let (attachments_closed, closed) = std::sync::mpsc::channel();
    let (launcher_exited, exit_confirmed) = std::sync::mpsc::channel();
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    let startup_in_thread = startup.clone();
    let domain_in_thread = domain.clone();
    let closing = std::thread::spawn(move || {
        let result = Domain::handover_with(
            || Lease::acquire_wait(&startup_in_thread, std::time::Duration::from_secs(1)),
            || Ok(()),
            || {
                attachments_closed.send(()).unwrap();
                exit_confirmed.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
                Ok(())
            },
            || Lease::wait_available(&domain_in_thread, std::time::Duration::from_secs(1)),
        );
        finished_tx.send(()).unwrap();
        result
    });

    closed.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
    assert!(
        finished_rx.try_recv().is_err(),
        "handover returned while attachment cleanup was waiting for launcher exit"
    );
    let Err(error) = Lease::acquire(&startup) else {
        panic!("a concurrent startup acquired the handover lease");
    };
    assert!(
        matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::AlreadyExists),
        "unexpected startup-lock contention error: {error}"
    );
    launcher_exited.send(()).unwrap();
    assert!(
        finished_rx.try_recv().is_err(),
        "handover returned after launcher exit but while the domain lease was held"
    );
    drop(owner);
    finished_rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
    closing.join().unwrap().unwrap();
    drop(Lease::acquire(&startup).unwrap());
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

    let text = summary.read().unwrap().unwrap();
    assert!(text.contains("2 programs could not be resumed as live processes"));
    assert!(text.contains("terminal pane-1: restored process cannot be attached"));
    assert!(text.contains("container database: checkpoint object is incomplete"));
    summary.clear().unwrap();
    assert_eq!(summary.read().unwrap(), None);
}

#[test]
fn successful_restore_removes_an_obsolete_failure_summary() {
    let root = tempfile::tempdir().unwrap();
    let mut workspace = WorkspaceConfig::new("demo", "ubuntu", Arch::Arm64);
    workspace.storage = Some(root.path().to_owned());
    let summary = RestoreSummary::new(&workspace);

    summary.publish(&["old failure".into()]).unwrap();
    summary.publish(&[]).unwrap();

    assert_eq!(summary.read().unwrap(), None);
}

#[tokio::test]
async fn kill_cleanup_attempts_process_reap_after_every_earlier_failure() {
    let calls = std::sync::Mutex::new(Vec::new());
    let outcome = Domain::stop_kill_with(
        || {
            calls.lock().unwrap().push("docker");
            Err(io::Error::other("daemon stuck"))
        },
        || {
            calls.lock().unwrap().push("attachments");
            Err(io::Error::other("launcher unavailable"))
        },
        async {
            calls.lock().unwrap().push("processes");
            Err(io::Error::other("guest reap timed out"))
        },
    )
    .await
    .unwrap_err()
    .to_string();

    assert_eq!(*calls.lock().unwrap(), ["docker", "attachments", "processes"]);
    assert!(outcome.contains("Docker service cleanup: daemon stuck"));
    assert!(outcome.contains("terminal attachment cleanup: launcher unavailable"));
    assert!(outcome.contains("workspace process cleanup: guest reap timed out"));
}

#[tokio::test]
async fn auxiliary_restore_failure_does_not_abort_later_restore_or_domain_serving() {
    use std::cell::RefCell;
    use std::rc::Rc;

    let events = Rc::new(RefCell::new(Vec::new()));
    let starts = Rc::clone(&events);
    let executions = Rc::clone(&events);
    let serving = Rc::clone(&events);

    // This mirrors the `?` boundary in `Domain::serve`: an independent process
    // failure is diagnostic data, so only a repository-level error may prevent
    // the daemon from being constructed and served.
    let startup: Result<Vec<String>, &'static str> = async {
        let failures = Runtime::restore_independently(
            [
                ("broken-id".into(), "broken-cache".into()),
                ("healthy-id".into(), "healthy-database".into()),
            ],
            move |id| {
                let starts = Rc::clone(&starts);
                async move {
                    starts.borrow_mut().push(format!("container {id}"));
                    if id == "broken-id" {
                        Err("missing optional volume")
                    } else {
                        Ok(())
                    }
                }
            },
            move || {
                let executions = Rc::clone(&executions);
                async move {
                    executions.borrow_mut().push("executions".into());
                    Ok(vec![("pane-2", "terminal endpoint unavailable")])
                }
            },
            |_id| async { Ok(()) },
        )
        .await?;
        serving.borrow_mut().push("serve".into());
        Ok(failures)
    }
    .await;

    assert_eq!(
        startup.unwrap(),
        [
            "broken-cache: missing optional volume",
            "execution pane-2: terminal endpoint unavailable",
        ]
    );
    assert_eq!(
        events.borrow().as_slice(),
        ["container broken-id", "container healthy-id", "executions", "serve"]
    );
}

#[test]
fn docker_warning_survives_relaunch_until_the_ui_acknowledges_it() {
    let root = tempfile::tempdir().unwrap();
    let mut workspace = WorkspaceConfig::new("demo", "ubuntu", Arch::Arm64);
    workspace.storage = Some(root.path().join("x".repeat(192)));
    workspace.docker_sock = true;
    let daemon = crate::runtime::resources::Daemon::new(&workspace);
    let preparation = daemon.prepare_checkpoint().unwrap();
    let warning = preparation.warning().unwrap().to_owned();
    drop(preparation);

    Domain::publish_restore_summary(&workspace, &mut Vec::new()).unwrap();
    assert!(RestoreSummary::new(&workspace)
        .read()
        .unwrap()
        .unwrap()
        .contains(&warning));
    assert_eq!(daemon.checkpoint_warning().unwrap().as_deref(), Some(warning.as_str()));

    // A second domain launch before any terminal/UI consumer must republish, not acknowledge, it.
    Domain::publish_restore_summary(&workspace, &mut Vec::new()).unwrap();
    assert_eq!(daemon.checkpoint_warning().unwrap().as_deref(), Some(warning.as_str()));

    let delivered = Domain::take_restore_summary(&workspace).unwrap().unwrap();
    assert!(delivered.contains(&warning));
    assert_eq!(daemon.checkpoint_warning().unwrap(), None);
    assert_eq!(Domain::take_restore_summary(&workspace).unwrap(), None);
}

#[test]
fn restore_notice_marks_every_line_as_husklet_output_and_shortens_the_typed_reason() {
    let root = tempfile::tempdir().unwrap();
    let mut workspace = WorkspaceConfig::new("demo", "ubuntu", Arch::Arm64);
    workspace.storage = Some(root.path().to_owned());
    let summary = RestoreSummary::new(&workspace);

    summary
        .publish(&[
            "execution 95032ffd73334c859c8bd2f1292dd438: exec 95032ffd73334c859c8bd2f1292dd438 \
             cannot be reattached after a whole-image restore: the restored member is a forked \
             child of the container's engine process"
                .into(),
        ])
        .unwrap();
    let text = summary.read().unwrap().unwrap();

    // Every line is attributed to Husklet, so it can never read as something the guest printed.
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        assert!(
            line.starts_with(crate::runtime::domain::RESTORE_NOTICE_PREFIX),
            "unmarked notice line: {line:?}"
        );
    }
    // One short line: a short id used once, the refusal's first clause, no engine internals.
    assert!(
        text.contains("\u{2022} execution 95032ffd: cannot be reattached after a whole-image restore\n"),
        "{text}"
    );
    assert_eq!(text.matches("95032ffd").count(), 1, "{text}");
    assert!(
        !text.contains("forked child of the container's engine process"),
        "{text}"
    );
    // Preserved scrollback is explained rather than presented as a failure report.
    assert!(text.contains("preserved history"));
    assert!(text.contains("could not be resumed as a live process"));
    assert!(!text.contains("failure(s)"), "alarming wording survived: {text}");
}

#[test]
fn restore_notice_counts_more_than_one_unresumable_process() {
    let root = tempfile::tempdir().unwrap();
    let mut workspace = WorkspaceConfig::new("demo", "ubuntu", Arch::Arm64);
    workspace.storage = Some(root.path().to_owned());
    let summary = RestoreSummary::new(&workspace);

    summary
        .publish(&["execution a: gone".into(), "execution b: gone".into()])
        .unwrap();
    let text = summary.read().unwrap().unwrap();

    assert!(text.contains("2 programs could not be resumed"), "{text}");
}

/// The user's reported journey, at the record layer: one workspace closed with Continue later and
/// reopened, over and over. Each reopen refuses to reattach the pane's execution, and each reopen
/// then creates a fresh pane execution which is checkpointed by the next close.
///
/// Every cycle must report exactly the one program it could not resume. Before the refused record
/// was discarded, cycle N reported N of them, because nothing in the workspace's life removes a
/// `Created` execution that owns a checkpoint.
#[tokio::test]
async fn repeated_continue_later_cycles_report_one_refusal_each_and_never_accumulate() {
    use std::cell::RefCell;
    use std::rc::Rc;

    // The durable execution records of one workspace, keyed by id, surviving every cycle.
    let records: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let mut reported = Vec::new();

    for cycle in 1..=10 {
        // The pane execution checkpointed by the previous close is the record awaiting restore.
        let awaiting = records.borrow().clone();
        let discarded = Rc::clone(&records);
        let failures: Vec<String> = Runtime::restore_independently(
            std::iter::empty::<(String, String)>(),
            |_id: String| async { Ok::<(), String>(()) },
            move || async move {
                Ok(awaiting
                    .into_iter()
                    .map(|id| (id, "the restored member exposes no live handle".to_owned()))
                    .collect::<Vec<_>>())
            },
            move |id: String| {
                let discarded = Rc::clone(&discarded);
                async move {
                    discarded.borrow_mut().retain(|existing| existing != &id);
                    Ok::<(), String>(())
                }
            },
        )
        .await
        .expect("restore reports independent failures rather than aborting");
        reported.push(failures.len());
        // Reopening the workspace opens one pane, whose execution the next close checkpoints.
        records.borrow_mut().push(format!("pane-{cycle}"));
    }

    assert_eq!(
        reported,
        [0, 1, 1, 1, 1, 1, 1, 1, 1, 1],
        "each reopen must report only the execution that this cycle could not resume"
    );
    assert_eq!(
        records.borrow().len(),
        1,
        "a refused execution record must not survive the restore that reported it"
    );
}

/// A refusal is the line the reader acts on; a failure to discard the refused record must not
/// replace it, duplicate it, or abort the restore of anything else.
#[tokio::test]
async fn a_discard_failure_neither_hides_nor_duplicates_the_refusal_it_follows() {
    let failures: Vec<String> = Runtime::restore_independently(
        std::iter::empty::<(String, String)>(),
        |_id: String| async { Ok::<(), String>(()) },
        || async { Ok(vec![("pane-1".to_owned(), "no live handle".to_owned())]) },
        |_id: String| async { Err::<(), String>("record storage is read-only".to_owned()) },
    )
    .await
    .expect("a discard failure is diagnostic, not a repository error");

    assert_eq!(failures, ["execution pane-1: no live handle"]);
}
