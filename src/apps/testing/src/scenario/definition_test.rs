use super::{Class, Resource, Scenario, ScenarioAction};
use std::{collections::BTreeSet, fs, path::Path};

fn load(document: &str) -> Result<Scenario, Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let directory = root.path().join("sample");
    fs::create_dir_all(directory.join("golden"))?;
    fs::write(directory.join("golden/contains.txt"), "marker")?;
    fs::write(directory.join("golden/exact.txt"), "exact\n")?;
    fs::write(directory.join("payload.txt"), "payload")?;
    let definition = directory.join("test.yaml");
    fs::write(&definition, document)?;
    Scenario::load(&directory, &definition)
}

#[test]
fn legacy_run_is_a_single_typed_argv_action() {
    let scenario = load(
        r#"cases:
  - id: sample/legacy
    image: alpine:3.20
    run: { program: /bin/echo, arguments: [marker] }
    expect: { stdout_contains: golden/contains.txt }
"#,
    )
    .unwrap();
    let case = &scenario.cases[0];
    assert!(matches!(&case.actions[..], [ScenarioAction::Argv(argv)] if argv == &["/bin/echo", "marker"]));
    assert_eq!(case.targets.len(), 2);
}

#[test]
fn rich_contract_preserves_bounds_and_order() {
    let scenario = load(
        r#"cases:
  - id: sample/rich
    image: alpine:3.20
    class: long
    targets: [arm64]
    xfail: [arm64]
    resources: [network, process_heavy]
    environment: { TERM: xterm }
    fixtures: [{ source: payload.txt, destination: /data/payload.txt }]
    actions:
      - shell: { script: "echo marker" }
      - argv: { argv: [/bin/echo, exact] }
      - host: { script: "true" }
      - api: { operation: inspect }
    readiness: { startup: "daemon --fork", probe: "daemon ping", attempts: 3, delay_ms: 10, logs: [/tmp/daemon.log] }
    timeout: 7
    expect: { exit: 0, stdout_contains: [golden/contains.txt], stdout_exact: golden/exact.txt }
"#,
    )
    .unwrap();
    let case = &scenario.cases[0];
    assert_eq!(case.class, Class::Long);
    assert_eq!(case.resources, [Resource::Network, Resource::ProcessHeavy]);
    assert_eq!(case.actions.len(), 4);
    assert_eq!(case.timeout, 7);
    assert_eq!(case.readiness.as_ref().unwrap().attempts, 3);
}

#[test]
fn entrypoint_action_is_distinct_from_empty_argv() {
    let entrypoint = load(
        "cases:\n  - id: sample/entrypoint\n    image: alpine\n    actions: [{ entrypoint: {} }]\n    expect: { stdout_contains: golden/contains.txt }\n",
    )
    .unwrap();
    assert!(matches!(entrypoint.cases[0].actions[0], ScenarioAction::Entrypoint));
    assert!(
        load("cases:\n  - id: sample/empty\n    image: alpine\n    actions: [{ argv: { argv: [] } }]\n    expect: { stdout_contains: golden/contains.txt }\n").is_err()
    );
}

#[test]
fn invalid_cross_target_and_path_contracts_are_rejected() {
    for document in [
        "cases:\n  - id: sample/xfail\n    image: alpine\n    targets: [arm64]\n    xfail: [amd64]\n    actions: [{ shell: { script: true } }]\n    expect: { stdout_contains: golden/contains.txt }\n",
        "cases:\n  - id: sample/path\n    image: alpine\n    fixtures: [{ source: ../payload, destination: /payload }]\n    actions: [{ shell: { script: true } }]\n    expect: { stdout_contains: golden/contains.txt }\n",
        "cases:\n  - id: sample/both\n    image: alpine\n    run: { program: /bin/true }\n    actions: [{ shell: { script: true } }]\n    expect: { stdout_contains: golden/contains.txt }\n",
        "cases:\n  - id: sample/resources\n    image: alpine\n    resources: [network, network]\n    actions: [{ shell: { script: true } }]\n    expect: { stdout_contains: golden/contains.txt }\n",
        "cases:\n  - id: sample/fixtures\n    image: alpine\n    fixtures: [{ source: payload.txt, destination: /payload }, { source: payload.txt, destination: /payload }]\n    actions: [{ shell: { script: true } }]\n    expect: { stdout_contains: golden/contains.txt }\n",
    ] {
        assert!(load(document).is_err(), "accepted invalid document: {document}");
    }
    let oversized = "x".repeat(super::MAX_TEXT + 1);
    assert!(load(&format!("cases:\n  - id: sample/bounded\n    image: alpine\n    environment: {{ VALUE: {oversized:?} }}\n    actions: [{{ shell: {{ script: true }} }}]\n    expect: {{ stdout_contains: golden/contains.txt }}\n")).is_err());
}

#[test]
fn dotnet_folder_preserves_the_orphan_contract_ids() {
    let mut root = Path::new(env!("CARGO_MANIFEST_DIR"));
    while !root.join("tests/scenarios/languages/test.yaml").is_file() {
        root = root.parent().expect("workspace root contains dotnet scenario");
    }
    let directory = root.join("tests/scenarios/languages");
    let scenario = Scenario::load(&directory, &directory.join("test.yaml")).unwrap();
    let ids = scenario
        .cases
        .iter()
        .map(|case| case.id.as_str())
        .filter(|id| id.starts_with("languages/dotnet-"))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        ids,
        BTreeSet::from([
            "languages/dotnet-fib-sdk9",
            "languages/dotnet-runtime-info-8",
            "languages/dotnet-sum-sdk8",
            "languages/dotnet-version-sdk8"
        ])
    );
}

#[test]
fn every_repository_definition_loads_with_globally_unique_ids() {
    let mut root = Path::new(env!("CARGO_MANIFEST_DIR"));
    while !root.join("tests/scenarios").is_dir() {
        root = root.parent().expect("workspace root contains scenario definitions");
    }
    let scenario_root = root.join("tests/scenarios");
    let mut directories = fs::read_dir(&scenario_root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir() && path.join("test.yaml").is_file())
        .collect::<Vec<_>>();
    directories.sort();
    assert!(!directories.is_empty());

    let mut ids = BTreeSet::new();
    for directory in directories {
        let definition = directory.join("test.yaml");
        let scenario =
            Scenario::load(&directory, &definition).unwrap_or_else(|error| panic!("{}: {error}", definition.display()));
        for case in scenario.cases {
            assert!(
                ids.insert(case.id.clone()),
                "duplicate repository scenario id {}",
                case.id
            );
        }
    }
}
