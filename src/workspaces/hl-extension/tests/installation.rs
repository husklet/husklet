//! The install lifecycle and the grant that survives across restarts. The
//! property under test throughout is that a grant only ever narrows on its own.

use hl_extension::{
    Capability, Consent, Disposition, ExtensionName, Grant, Installation, Manifest, Record, Stage, Summary, PROTOCOL,
};

fn name() -> ExtensionName {
    ExtensionName::new("containers").expect("name")
}

fn manifest(capabilities: &[Capability]) -> Manifest {
    let capabilities: Vec<String> = capabilities
        .iter()
        .map(|capability| format!("\"{}\"", capability.as_str()))
        .collect();
    let label = format!(
        "{{\"name\":\"containers\",\"display_name\":\"Containers\",\"version\":\"1.0.0\",\
          \"protocol\":{PROTOCOL},\"capabilities\":[{}]}}",
        capabilities.join(",")
    );
    Manifest::parse(&label, PROTOCOL).expect("manifest")
}

fn installed(requested: &[Capability], consented: &[Capability]) -> Installation {
    let mut installation = Installation::new();
    installation
        .install(
            &manifest(requested),
            "sha256:first",
            &Grant::new(consented.iter().copied()),
            1_000,
        )
        .expect("installed");
    installation
}

#[test]
fn an_install_records_the_intersection_rather_than_the_request() {
    let installation = installed(
        &[Capability::ContainerRead, Capability::ContainerControl],
        &[Capability::ContainerRead],
    );
    let record = installation.record(&name()).expect("recorded");

    assert!(record.granted.holds(Capability::ContainerRead));
    assert!(
        !record.granted.holds(Capability::ContainerControl),
        "a request is not consent"
    );
    assert_eq!(record.granted.len(), 1);
    assert_eq!(record.image_digest, "sha256:first");
    assert_eq!(record.installed_at, 1_000);
    assert_eq!(installation.stage(&name()), Stage::Standby, "install does not start it");
}

#[test]
fn consent_beyond_the_manifest_is_not_recorded() {
    let installation = installed(
        &[Capability::ContainerRead],
        &[Capability::ContainerRead, Capability::FilesystemWrite],
    );
    let record = installation.record(&name()).expect("recorded");

    assert!(
        !record.granted.holds(Capability::FilesystemWrite),
        "an undeclared capability cannot be consented into existence"
    );
}

#[test]
fn an_update_asking_for_more_keeps_the_old_grant_and_names_what_is_new() {
    let mut installation = installed(&[Capability::ContainerRead], &[Capability::ContainerRead]);
    installation.enable(&name()).expect("enabled");

    let update = manifest(&[
        Capability::ContainerRead,
        Capability::ContainerControl,
        Capability::FilesystemWrite,
    ]);
    let consent = installation
        .reinstall(&update, "sha256:second", 2_000)
        .expect("updated");

    assert_eq!(
        consent,
        Consent::Requirement {
            additional: vec![Capability::ContainerControl, Capability::FilesystemWrite],
        }
    );
    assert!(consent.is_requirement());

    let record = installation.record(&name()).expect("recorded");
    assert_eq!(record.granted, Grant::new([Capability::ContainerRead]));
    assert_eq!(record.image_digest, "sha256:second", "it runs the new image");
    assert_eq!(
        installation.stage(&name()),
        Stage::Duty,
        "it keeps running, narrowly, rather than stopping"
    );
}

#[test]
fn an_update_asking_for_less_narrows_without_prompting() {
    let mut installation = installed(
        &[Capability::ContainerRead, Capability::ContainerControl],
        &[Capability::ContainerRead, Capability::ContainerControl],
    );

    let update = manifest(&[Capability::ContainerRead]);
    let consent = installation
        .reinstall(&update, "sha256:second", 2_000)
        .expect("updated");

    assert_eq!(consent, Consent::Standing);
    let record = installation.record(&name()).expect("recorded");
    assert_eq!(record.granted, Grant::new([Capability::ContainerRead]));
    assert!(!record.granted.holds(Capability::ContainerControl));
}

#[test]
fn a_widened_grant_arrives_only_with_an_answered_prompt() {
    let mut installation = installed(&[Capability::ContainerRead], &[Capability::ContainerRead]);
    let update = manifest(&[Capability::ContainerRead, Capability::ContainerControl]);
    installation
        .reinstall(&update, "sha256:second", 2_000)
        .expect("updated");

    let record = installation
        .consent(&update, &Grant::new([Capability::ContainerControl]))
        .expect("consented");

    assert!(record.granted.holds(Capability::ContainerControl));
    assert!(
        record.granted.holds(Capability::ContainerRead),
        "the old grant survives"
    );
}

#[test]
fn an_update_to_an_absent_record_is_refused_rather_than_installing_one() {
    let mut installation = Installation::new();
    let update = manifest(&[Capability::ContainerRead]);

    assert!(installation.reinstall(&update, "sha256:second", 2_000).is_err());
    assert_eq!(installation.stage(&name()), Stage::Vacancy);
    assert!(installation.is_empty());
}

#[test]
fn an_enable_and_disable_round_trip_leaves_the_grant_and_digest_intact() {
    let mut installation = installed(
        &[Capability::ContainerRead, Capability::Interface],
        &[Capability::ContainerRead, Capability::Interface],
    );
    let before = installation.record(&name()).expect("recorded").clone();

    installation.enable(&name()).expect("enabled");
    assert_eq!(installation.stage(&name()), Stage::Duty);
    installation.disable(&name()).expect("disabled");
    assert_eq!(installation.stage(&name()), Stage::Standby);

    let after = installation.record(&name()).expect("recorded");
    assert_eq!(after.granted, before.granted);
    assert_eq!(after.image_digest, before.image_digest);
    assert_eq!(after.installed_at, before.installed_at);
}

#[test]
fn an_uninstall_forgets_the_record_and_a_later_install_starts_from_nothing() {
    let mut installation = installed(
        &[Capability::ContainerRead, Capability::ContainerControl],
        &[Capability::ContainerRead, Capability::ContainerControl],
    );

    let forgotten = installation.uninstall(&name()).expect("removed");
    assert!(forgotten.granted.holds(Capability::ContainerControl));
    assert_eq!(installation.stage(&name()), Stage::Vacancy);
    assert!(installation.record(&name()).is_none());
    assert!(installation.uninstall(&name()).is_none());

    installation
        .install(
            &manifest(&[Capability::ContainerRead, Capability::ContainerControl]),
            "sha256:third",
            &Grant::new([Capability::ContainerRead]),
            3_000,
        )
        .expect("installed");

    let record = installation.record(&name()).expect("recorded");
    assert!(
        !record.granted.holds(Capability::ContainerControl),
        "an uninstalled grant must not be resurrected"
    );
}

#[test]
fn restarts_under_the_limit_stay_on_duty_and_back_off() {
    let mut installation = installed(&[Capability::ContainerRead], &[Capability::ContainerRead]);
    installation.enable(&name()).expect("enabled");

    let mut previous = 0;
    for attempt in 1..Installation::ATTEMPT_LIMIT {
        let disposition = installation.restarted(&name(), i64::from(attempt)).expect("counted");
        let Disposition::Backoff {
            attempt: counted,
            delay_ms,
        } = disposition
        else {
            panic!("under the limit must not fault: {disposition:?}");
        };
        assert_eq!(counted, attempt);
        assert!(delay_ms >= previous && delay_ms <= Installation::BACKOFF_CAP_MS);
        previous = delay_ms;
        assert_eq!(installation.stage(&name()), Stage::Duty);
    }
}

#[test]
fn a_long_lived_run_starts_the_restart_count_over() {
    let mut installation = installed(&[Capability::ContainerRead], &[Capability::ContainerRead]);
    installation.enable(&name()).expect("enabled");

    for attempt in 1..Installation::ATTEMPT_LIMIT {
        installation.restarted(&name(), i64::from(attempt)).expect("counted");
    }
    let later = Installation::WINDOW_MS * 2;
    let disposition = installation.restarted(&name(), later).expect("counted");

    assert_eq!(
        disposition,
        Disposition::Backoff {
            attempt: 1,
            delay_ms: Installation::BACKOFF_BASE_MS
        },
        "a restart outside the window is judged on its own"
    );
    assert_eq!(installation.stage(&name()), Stage::Duty);
}

#[test]
fn restarts_over_the_limit_fault_and_stay_faulted_until_a_retry() {
    let mut installation = installed(&[Capability::ContainerRead], &[Capability::ContainerRead]);
    installation.enable(&name()).expect("enabled");

    for attempt in 1..Installation::ATTEMPT_LIMIT {
        installation.restarted(&name(), i64::from(attempt)).expect("counted");
    }
    let disposition = installation
        .restarted(&name(), i64::from(Installation::ATTEMPT_LIMIT))
        .expect("counted");
    assert_eq!(
        disposition,
        Disposition::Fault {
            restarts: Installation::ATTEMPT_LIMIT
        }
    );

    let stage = installation.stage(&name());
    assert_eq!(
        stage,
        Stage::Fault {
            restarts: Installation::ATTEMPT_LIMIT
        }
    );
    assert!(stage.is_fault());
    assert_ne!(stage, Stage::Standby, "a fault is never a plain disable");
    assert!(
        installation.record(&name()).expect("recorded").enabled,
        "the record still says the person wanted it running"
    );

    // Time passing is not consent to try again.
    let far_later = Installation::WINDOW_MS * 10;
    assert!(matches!(
        installation.restarted(&name(), far_later).expect("counted"),
        Disposition::Fault { .. }
    ));
    assert!(installation.stage(&name()).is_fault());

    installation.retry(&name()).expect("retried");
    assert_eq!(installation.stage(&name()), Stage::Duty);
    assert_eq!(
        installation.restarted(&name(), far_later + 1).expect("counted"),
        Disposition::Backoff {
            attempt: 1,
            delay_ms: Installation::BACKOFF_BASE_MS
        },
        "a retry restores the full attempt budget"
    );
}

#[test]
fn a_new_image_clears_the_restart_count() {
    let mut installation = installed(&[Capability::ContainerRead], &[Capability::ContainerRead]);
    installation.enable(&name()).expect("enabled");
    for attempt in 1..=Installation::ATTEMPT_LIMIT {
        installation.restarted(&name(), i64::from(attempt)).expect("counted");
    }
    assert!(installation.stage(&name()).is_fault());

    installation
        .reinstall(&manifest(&[Capability::ContainerRead]), "sha256:second", 5_000)
        .expect("updated");

    assert_eq!(
        installation.stage(&name()),
        Stage::Duty,
        "a different image has not failed yet"
    );
}

#[test]
fn a_record_round_trips_through_serde_unchanged() {
    let mut installation = installed(
        &[Capability::ContainerRead, Capability::Interface],
        &[Capability::ContainerRead, Capability::Interface],
    );
    installation.enable(&name()).expect("enabled");
    let record = installation.record(&name()).expect("recorded");

    let encoded = serde_json::to_string(record).expect("encoded");
    let decoded: Record = serde_json::from_str(&encoded).expect("decoded");

    assert_eq!(&decoded, record);
    assert_eq!(decoded.granted.len(), 2);
    assert!(decoded.enabled);
}

#[test]
fn the_consent_summary_names_execution_when_the_grant_permits_it() {
    for capability in [Capability::ContainerControl, Capability::TerminalControl] {
        let summary = Summary::of(&Grant::new([Capability::ContainerRead, capability]));
        assert!(summary.execution, "{capability:?} is execution inside the workspace");
        assert!(summary.mutations.contains(&capability));
        assert!(summary.to_string().contains(Summary::EXECUTION_NOTICE));
    }
}

#[test]
fn the_consent_summary_stays_quiet_about_execution_when_it_is_read_only() {
    let summary = Summary::of(&Grant::new([
        Capability::ContainerRead,
        Capability::WorkspaceRead,
        Capability::TerminalOutput,
    ]));

    assert!(!summary.execution);
    assert!(summary.mutations.is_empty());
    assert_eq!(summary.observations.len(), 3);
    assert!(!summary.to_string().contains(Summary::EXECUTION_NOTICE));
}

#[test]
fn an_operation_on_an_absent_record_is_refused() {
    let mut installation = Installation::new();

    assert!(installation.enable(&name()).is_err());
    assert!(installation.disable(&name()).is_err());
    assert!(installation.retry(&name()).is_err());
    assert!(installation.restarted(&name(), 0).is_err());
    assert!(installation.record(&name()).is_none());
}
