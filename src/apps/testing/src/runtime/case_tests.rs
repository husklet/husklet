use super::{Options, WorkKey, apps, plan, plan_for_host, require_planned, validate_case_ids, workspace};
use crate::runtime::definition::{App, EngineHost};
use crate::suite::Target;
use clap::Parser;
use std::{collections::BTreeSet, fs};

#[derive(Parser)]
struct RuntimeCli {
    #[command(flatten)]
    options: Options,
}

fn options(arguments: &[&str]) -> Options {
    RuntimeCli::try_parse_from(std::iter::once("runtime").chain(arguments.iter().copied()))
        .unwrap()
        .options
}

fn app() -> App {
    let directory = tempfile::tempdir().unwrap();
    let definition = directory.path().join("test.yaml");
    fs::write(directory.path().join("exact.c"), "exact").unwrap();
    fs::write(directory.path().join("excluded.c"), "excluded").unwrap();
    fs::write(directory.path().join("inactive.c"), "inactive").unwrap();
    let golden = directory.path().join("golden");
    fs::create_dir(&golden).unwrap();
    for output in ["exact.out", "excluded.out", "inactive.out"] {
        fs::write(golden.join(output), []).unwrap();
    }
    fs::write(
        &definition,
        r"targets: [arm64, amd64]
image: alpine
execution: {}
build: { compiler: { arm64: arm-cc, amd64: amd-cc }, flags: [] }
cases:
  - { id: runtime/exact, build: { source: exact.c, output: exact, flags: [] }, artifact: { destination: /opt/exact }, status: active, compat: { class: compatibility }, targets: [arm64], run: [], expect: { exit: 0, stdout: golden/exact.out } }
  - id: runtime/host-excluded
    build: { source: excluded.c, output: excluded, flags: [] }
    artifact: { destination: /opt/excluded }
    targets: [arm64]
    status: !host-excluded
      hosts: [macos]
      reason: retained macOS exclusion
      evidence: tests/runtime/example/EVIDENCE.md
    compat: { class: compatibility }
    run: []
    expect: { exit: 0, stdout: golden/excluded.out }
  - id: runtime/inactive
    build: { source: inactive.c, output: inactive, flags: [] }
    artifact: { destination: /opt/inactive }
    status: !broken
      reason: retained incompatibility
      evidence: tests/runtime/example/EVIDENCE.md
    compat: { class: compatibility }
    run: []
    expect: { exit: 0, stdout: golden/inactive.out }
",
    )
    .unwrap();
    App::load(directory.path(), &definition).unwrap()
}

#[test]
fn case_selection_parses_with_optional_app_and_isa() {
    let options = options(&["example", "--case", "runtime/exact", "--isa", "arm64"]);
    assert_eq!(options.app.as_deref(), Some("example"));
    assert_eq!(options.case.as_deref(), Some("runtime/exact"));
    assert_eq!(options.target, Some(Target::Arm64));
}

#[test]
fn plan_matches_only_the_complete_case_id() {
    let exact = options(&["--case", "runtime/exact", "--isa", "arm64"]);
    let planned = plan(vec![app()], &exact);
    assert!(planned.matched_case);
    assert_eq!(planned.work.len(), 1);
    assert_eq!(planned.work[0].key.id, "runtime/exact");

    let substring = options(&["--case", "runtime/ex"]);
    let error = require_planned(plan(vec![app()], &substring), substring.case.as_deref())
        .err()
        .expect("substring case selection unexpectedly produced work");
    assert_eq!(error.to_string(), "no runtime case exactly matched --case runtime/ex");
}

#[test]
fn an_inactive_only_selection_is_recorded_rather_than_rejected() {
    let options = options(&["--case", "runtime/inactive", "--isa", "arm64"]);
    let planned = require_planned(plan(vec![app()], &options), options.case.as_deref()).unwrap();
    assert!(planned.work.is_empty());
    assert_eq!(planned.skipped.len(), 1);
    assert_eq!(planned.skipped[0].key.id, "runtime/inactive");
    assert_eq!(planned.skipped[0].status, super::ledger::NOT_RUN);
    assert!(planned.skipped[0].diagnostic.contains("retained incompatibility"));
}

#[test]
fn host_exclusion_uses_the_injected_engine_host() {
    let options = options(&["--case", "runtime/host-excluded", "--isa", "arm64"]);
    for host in [EngineHost::Linux, EngineHost::Windows] {
        let planned = plan_for_host(vec![app()], &options, host);
        assert!(planned.matched_case);
        assert_eq!(planned.work.len(), 1);
        assert_eq!(planned.work[0].key.id, "runtime/host-excluded");
    }

    let excluded = plan_for_host(vec![app()], &options, EngineHost::Macos);
    assert!(excluded.matched_case);
    let excluded = require_planned(excluded, options.case.as_deref()).unwrap();
    assert!(excluded.work.is_empty());
    assert_eq!(excluded.skipped.len(), 1);
    assert_eq!(excluded.skipped[0].key.id, "runtime/host-excluded");
}

#[test]
fn duplicate_full_ids_are_rejected_before_planning() {
    let error = validate_case_ids(&[app(), app()]).unwrap_err();
    assert_eq!(error.to_string(), "runtime case ID is duplicated: runtime/exact");
}

#[test]
fn repository_yaml_inventory_is_fully_discovered_and_planned() {
    // This is current-manifest integrity, not a parity claim for retired TSV rows;
    // tests/runtime/LEGACY_PARITY.md remains the source audit for migration gaps.
    let options = options(&[]);
    let apps = apps(&options).unwrap();
    validate_case_ids(&apps).unwrap();

    let root = workspace().unwrap().join("tests/runtime");
    let manifest_apps = fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.join("test.yaml").is_file())
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect::<BTreeSet<_>>();
    let loaded_apps = apps.iter().map(|app| app.name.clone()).collect::<BTreeSet<_>>();
    assert_eq!(loaded_apps, manifest_apps);

    let mut declared = BTreeSet::new();
    let mut active = BTreeSet::new();
    let mut inactive = BTreeSet::new();
    for case in apps.iter().flat_map(|app| &app.cases) {
        for target in &case.targets {
            let key = WorkKey {
                id: case.id.clone(),
                target: *target,
            };
            assert!(declared.insert(key.clone()), "duplicate declared runtime work key");
            if case.inactive(EngineHost::current()).is_none() {
                active.insert(key);
            } else {
                inactive.insert(key);
            }
        }
    }
    assert!(active.is_disjoint(&inactive));
    assert_eq!(declared, active.union(&inactive).cloned().collect());

    let planned = plan(apps, &options);
    let scheduled = planned.work.into_iter().map(|work| work.key).collect::<BTreeSet<_>>();
    let recorded = planned.skipped.into_iter().map(|row| row.key).collect::<BTreeSet<_>>();
    assert_eq!(scheduled, active);
    assert_eq!(recorded, inactive);
    assert_eq!(scheduled.union(&recorded).cloned().collect::<BTreeSet<_>>(), declared);
}
