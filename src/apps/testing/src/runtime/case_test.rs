use super::{Options, Schedule, WorkKey, apps, require_planned, validate_case_ids, workspace};
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
    assert_eq!(options.selection.case.as_deref(), Some("runtime/exact"));
    assert_eq!(options.selection.target, Some(Target::Arm64));
}

#[test]
fn plan_matches_only_the_complete_case_id() {
    let exact = options(&["--case", "runtime/exact", "--isa", "arm64"]);
    let planned = Schedule::plan(vec![app()], &exact);
    assert!(planned.matched_case);
    assert_eq!(planned.work.len(), 1);
    assert_eq!(planned.work[0].key.id, "runtime/exact");

    let substring = options(&["--case", "runtime/ex"]);
    let error = require_planned(
        Schedule::plan(vec![app()], &substring),
        substring.selection.case.as_deref(),
    )
    .err()
    .expect("substring case selection unexpectedly produced work");
    assert_eq!(error.to_string(), "no runtime case exactly matched --case runtime/ex");
}

#[test]
fn an_inactive_only_selection_is_recorded_rather_than_rejected() {
    let options = options(&["--case", "runtime/inactive", "--isa", "arm64"]);
    let planned = require_planned(Schedule::plan(vec![app()], &options), options.selection.case.as_deref()).unwrap();
    assert!(planned.work.is_empty());
    assert_eq!(planned.skipped.len(), 1);
    assert_eq!(planned.skipped[0].attempt.key.id, "runtime/inactive");
    assert_eq!(planned.skipped[0].attempt.status, super::ledger::NOT_RUN);
    assert!(planned.skipped[0].diagnostic.contains("retained incompatibility"));
}

#[test]
fn host_exclusion_uses_the_injected_engine_host() {
    let options = options(&["--case", "runtime/host-excluded", "--isa", "arm64"]);
    for host in [EngineHost::Linux, EngineHost::Windows] {
        let planned = Schedule::for_host(vec![app()], &options, host);
        assert!(planned.matched_case);
        assert_eq!(planned.work.len(), 1);
        assert_eq!(planned.work[0].key.id, "runtime/host-excluded");
    }

    let excluded = Schedule::for_host(vec![app()], &options, EngineHost::Macos);
    assert!(excluded.matched_case);
    let excluded = require_planned(excluded, options.selection.case.as_deref()).unwrap();
    assert!(excluded.work.is_empty());
    assert_eq!(excluded.skipped.len(), 1);
    assert_eq!(excluded.skipped[0].attempt.key.id, "runtime/host-excluded");
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

    let planned = Schedule::plan(apps, &options);
    let scheduled = planned.work.into_iter().map(|work| work.key).collect::<BTreeSet<_>>();
    let recorded = planned
        .skipped
        .into_iter()
        .map(|row| row.attempt.key)
        .collect::<BTreeSet<_>>();
    assert_eq!(scheduled, active);
    assert_eq!(recorded, inactive);
    assert_eq!(scheduled.union(&recorded).cloned().collect::<BTreeSet<_>>(), declared);
}

#[test]
fn x86_tso_compatibility_and_full_stress_are_both_owned() {
    let apps = apps(&options(&[])).unwrap();
    let cases = apps
        .iter()
        .flat_map(|app| &app.cases)
        .map(|case| (case.id.as_str(), case))
        .collect::<std::collections::BTreeMap<_, _>>();

    for (bounded, bounded_rounds, soak, soak_rounds) in [
        (
            "runtime/ipc/tso-unaligned",
            "2000",
            "runtime/ipc/tso-unaligned-soak",
            "100000",
        ),
        (
            "runtime/ipc/tso-simd-mp",
            "10000",
            "runtime/ipc/tso-simd-mp-soak",
            "400000",
        ),
    ] {
        let bounded = cases.get(bounded).unwrap();
        assert_eq!(bounded.arguments, [bounded_rounds]);
        assert!(bounded.soak.is_none());

        let extended = cases.get(soak).unwrap();
        assert_eq!(extended.arguments, [soak_rounds]);
        assert_eq!(extended.soak.as_ref().unwrap().duration().as_secs(), 900);
    }
}

/// Builds a one-case app whose manifest header and expectation are supplied, so floor inheritance
/// can be exercised without a real toolchain.
fn floored(header: &str, expect: &str) -> Result<App, crate::suite::Error> {
    let directory = tempfile::tempdir().unwrap();
    let definition = directory.path().join("test.yaml");
    fs::write(directory.path().join("only.c"), "only").unwrap();
    fs::create_dir(directory.path().join("golden")).unwrap();
    fs::write(directory.path().join("golden/only.out"), []).unwrap();
    fs::write(
        &definition,
        format!(
            "targets: [arm64]\nimage: alpine\n{header}\n\
             build: {{ compiler: {{ arm64: arm-cc, amd64: amd-cc }}, flags: [] }}\ncases:\n  \
             - {{ id: runtime/only, build: {{ source: only.c, output: only, flags: [] }}, \
             artifact: {{ destination: /opt/only }}, status: active, \
             compat: {{ class: compatibility }}, run: [], \
             expect: {{ exit: 0, stdout: golden/only.out{expect} }} }}\n"
        ),
    )
    .unwrap();
    App::load(directory.path(), &definition)
}

const NATIVE: &str = "execution: { native: true, diagnostics: true }\n\
                      diagnostics-floor: [{ counter: crossings, greater-than: 0 }]";

#[test]
fn a_case_without_its_own_assertions_inherits_the_app_floor() {
    assert_eq!(floored(NATIVE, "").unwrap().cases[0].diagnostics.len(), 1);
}

#[test]
fn an_explicit_empty_list_opts_a_case_out_of_the_floor() {
    assert!(
        floored(NATIVE, ", diagnostics: []").unwrap().cases[0]
            .diagnostics
            .is_empty()
    );
}

#[test]
fn a_case_list_replaces_the_floor_rather_than_adding_to_it() {
    let app = floored(NATIVE, ", diagnostics: [{ counter: translations, greater-than: 4 }]").unwrap();
    assert_eq!(app.cases[0].diagnostics.len(), 1);
}

#[test]
fn a_floor_on_an_app_that_never_emits_counters_is_a_load_error() {
    let error = floored(
        "execution: {}\ndiagnostics-floor: [{ counter: crossings, greater-than: 0 }]",
        "",
    )
    .err()
    .expect("a floor without native diagnostics unexpectedly loaded");
    assert!(error.to_string().contains("diagnostics-floor"), "{error}");
}

#[test]
fn cwd_backend_controls_compile_the_identical_fixture() {
    const NATIVE: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tests/runtime/filesystem/source/cwd_relative_resolution.c"
    ));
    const INTERPRETED: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tests/runtime/cwd-relative-interpreter/source/cwd_relative_resolution.c"
    ));
    assert_eq!(NATIVE, INTERPRETED, "native and interpreter controls drifted");
}
