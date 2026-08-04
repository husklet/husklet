use super::{Options, plan, require_work, validate_case_ids};
use crate::runtime::definition::App;
use crate::suite::Target;
use clap::Parser;
use std::fs;

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
    fs::write(
        &definition,
        r#"targets: [arm64, amd64]
image: alpine
execution: {}
build: { compiler: { arm64: arm-cc, amd64: amd-cc }, flags: [] }
cases:
  - { id: runtime/exact, build: { source: exact.c, output: exact, flags: [] }, artifact: { destination: /opt/exact }, status: active, compat: { class: compatibility }, targets: [arm64], run: [], expect: { exit: 0, stdout: golden/exact.out } }
  - id: runtime/inactive
    build: { source: inactive.c, output: inactive, flags: [] }
    artifact: { destination: /opt/inactive }
    status: !broken
      reason: retained incompatibility
      evidence: tests/runtime/example/EVIDENCE.md
    compat: { class: compatibility }
    run: []
    expect: { exit: 0, stdout: golden/inactive.out }
"#,
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
    let Err(error) = require_work(plan(vec![app()], &substring), substring.case.as_deref()) else {
        panic!("substring case selection unexpectedly produced work");
    };
    assert_eq!(error.to_string(), "no runtime case exactly matched --case runtime/ex");
}

#[test]
fn inactive_exact_match_is_distinct_from_no_match() {
    let options = options(&["--case", "runtime/inactive", "--isa", "arm64"]);
    let Err(error) = require_work(plan(vec![app()], &options), options.case.as_deref()) else {
        panic!("inactive case selection unexpectedly produced work");
    };
    assert_eq!(
        error.to_string(),
        "runtime case runtime/inactive matched but has no active work for the selected target(s)"
    );
}

#[test]
fn duplicate_full_ids_are_rejected_before_planning() {
    let error = validate_case_ids(&[app(), app()]).unwrap_err();
    assert_eq!(error.to_string(), "runtime case ID is duplicated: runtime/exact");
}
