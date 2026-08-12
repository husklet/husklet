use super::{Class, Resource, Scenario, Step};
use crate::scenario::terminal::Step as TerminalStep;
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
        r"cases:
  - id: sample/legacy
    image: alpine:3.20
    run: { program: /bin/echo, arguments: [marker] }
    expect: { stdout_contains: golden/contains.txt }
",
    )
    .unwrap();
    let case = &scenario.cases[0];
    assert!(matches!(&case.actions[..], [Step::Argv(argv)] if argv == &["/bin/echo", "marker"]));
    assert_eq!(case.targets.len(), 2);
}

#[test]
fn empty_output_is_typed_and_exclusive() {
    let scenario = load(
        "cases:\n  - id: sample/quiet\n    image: alpine\n    actions: [{ shell: { script: true } }]\n    expect: { output_empty: true }\n",
    )
    .unwrap();
    assert!(scenario.cases[0].output_empty);
    assert!(
        load("cases:\n  - id: sample/conflict\n    image: alpine\n    actions: [{ shell: { script: true } }]\n    expect: { output_empty: true, stdout_exact: golden/exact.txt }\n").is_err()
    );
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
      - api: { operation: copy_to_container, source: payload.txt, destination: /tmp }
    readiness: { startup: "daemon --fork", probe: "daemon ping", attempts: 3, delay_ms: 10, logs: [/tmp/daemon.log] }
    timeout: 7
    warmups: 2
    repetitions: 3
    expect: { exit: 0, stdout_contains: [golden/contains.txt], stdout_exact: golden/exact.txt }
"#,
    )
    .unwrap();
    let case = &scenario.cases[0];
    assert_eq!(case.class, Class::Long);
    assert_eq!(case.resources, [Resource::Network, Resource::ProcessHeavy]);
    assert_eq!(case.actions.len(), 4);
    assert_eq!(case.timeout, 7);
    assert_eq!((case.warmups, case.repetitions), (2, 3));
    assert_eq!(case.readiness.as_ref().unwrap().attempts, 3);
}

#[test]
fn entrypoint_action_is_distinct_from_empty_argv() {
    let entrypoint = load(
        "cases:\n  - id: sample/entrypoint\n    image: alpine\n    actions: [{ entrypoint: {} }]\n    expect: { stdout_contains: golden/contains.txt }\n",
    )
    .unwrap();
    assert!(matches!(entrypoint.cases[0].actions[0], Step::Entrypoint));
    assert!(
        load("cases:\n  - id: sample/empty\n    image: alpine\n    actions: [{ argv: { argv: [] } }]\n    expect: { stdout_contains: golden/contains.txt }\n").is_err()
    );
}

#[test]
fn terminal_action_preserves_ordered_bounded_operations() {
    let scenario = load(
        r#"cases:
  - id: sample/terminal
    image: alpine
    resources: [pty]
    actions:
      - terminal:
          argv: [/bin/sh]
          rows: 30
          columns: 100
          steps:
            - await_output: { contains: ready, timeout_ms: 250 }
            - write: { text: "abc\r" }
            - resize: { rows: 40, columns: 120 }
            - reject_output: { text: "\u007f" }
            - close: {}
    expect: { stdout_contains: golden/contains.txt }
"#,
    )
    .unwrap();
    let Step::Terminal(action) = &scenario.cases[0].actions[0] else {
        panic!("terminal action was not retained");
    };
    assert_eq!((action.rows, action.columns), (30, 100));
    assert_eq!(
        action.steps,
        [
            TerminalStep::AwaitOutput {
                contains: b"ready".to_vec(),
                timeout_ms: 250,
            },
            TerminalStep::Write(b"abc\r".to_vec()),
            TerminalStep::Resize { rows: 40, columns: 120 },
            TerminalStep::RejectOutput(vec![0x7f]),
            TerminalStep::Close,
        ]
    );
}

#[test]
fn terminal_actions_require_pty_and_reject_invalid_lifecycle() {
    for document in [
        "cases:\n  - id: sample/no-pty\n    image: alpine\n    actions: [{ terminal: { argv: [/bin/sh], steps: [{ close: {} }] } }]\n    expect: { stdout_contains: golden/contains.txt }\n",
        "cases:\n  - id: sample/zero-size\n    image: alpine\n    resources: [pty]\n    actions: [{ terminal: { argv: [/bin/sh], rows: 0, steps: [{ close: {} }] } }]\n    expect: { stdout_contains: golden/contains.txt }\n",
        "cases:\n  - id: sample/write-after-close\n    image: alpine\n    resources: [pty]\n    actions: [{ terminal: { argv: [/bin/sh], steps: [{ close: {} }, { write: { text: late } }] } }]\n    expect: { stdout_contains: golden/contains.txt }\n",
        "cases:\n  - id: sample/unbounded-wait\n    image: alpine\n    resources: [pty]\n    actions: [{ terminal: { argv: [/bin/sh], steps: [{ await_output: { contains: x, timeout_ms: 60001 } }] } }]\n    expect: { stdout_contains: golden/contains.txt }\n",
    ] {
        assert!(load(document).is_err(), "accepted invalid document: {document}");
    }
}

#[test]
fn invalid_cross_target_and_path_contracts_are_rejected() {
    for document in [
        "cases:\n  - id: sample/xfail\n    image: alpine\n    targets: [arm64]\n    xfail: [amd64]\n    actions: [{ shell: { script: true } }]\n    expect: { stdout_contains: golden/contains.txt }\n",
        "cases:\n  - id: sample/path\n    image: alpine\n    fixtures: [{ source: ../payload, destination: /payload }]\n    actions: [{ shell: { script: true } }]\n    expect: { stdout_contains: golden/contains.txt }\n",
        "cases:\n  - id: sample/both\n    image: alpine\n    run: { program: /bin/true }\n    actions: [{ shell: { script: true } }]\n    expect: { stdout_contains: golden/contains.txt }\n",
        "cases:\n  - id: sample/resources\n    image: alpine\n    resources: [network, network]\n    actions: [{ shell: { script: true } }]\n    expect: { stdout_contains: golden/contains.txt }\n",
        "cases:\n  - id: sample/fixtures\n    image: alpine\n    fixtures: [{ source: payload.txt, destination: /payload }, { source: payload.txt, destination: /payload }]\n    actions: [{ shell: { script: true } }]\n    expect: { stdout_contains: golden/contains.txt }\n",
        "cases:\n  - id: sample/samples\n    image: alpine\n    warmups: 101\n    repetitions: 0\n    actions: [{ shell: { script: true } }]\n    expect: { stdout_contains: golden/contains.txt }\n",
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
fn compiled_language_stable_ids_are_folder_owned_once() {
    let mut root = Path::new(env!("CARGO_MANIFEST_DIR"));
    while !root.join("tests/scenarios/languages/test.yaml").is_file() {
        root = root.parent().expect("workspace root contains language scenarios");
    }
    let directory = root.join("tests/scenarios/languages");
    let scenario = Scenario::load(&directory, &directory.join("test.yaml")).unwrap();
    let expected = BTreeSet::from([
        "languages/go-fib-123-alpine",
        "languages/go-sum-122-alpine",
        "languages/go-sum-122-bookworm",
        "languages/go-version-122-alpine",
        "languages/java-fib-21",
        "languages/java-sum-17",
        "languages/java-sum-temurin21",
        "languages/java-sum-temurin21-alpine",
        "languages/java-version-temurin17",
        "languages/rust-fib-1-slim",
        "languages/rust-sum-1-alpine",
        "languages/rust-version-1-slim",
    ]);
    let actual = scenario
        .cases
        .iter()
        .map(|case| case.id.as_str())
        .filter(|id| expected.contains(id))
        .collect::<BTreeSet<_>>();
    assert_eq!(actual, expected);
}

#[test]
fn weird_expected_failures_are_target_exact() {
    let mut root = Path::new(env!("CARGO_MANIFEST_DIR"));
    while !root.join("tests/scenarios/weird/test.yaml").is_file() {
        root = root.parent().expect("workspace root contains weird scenarios");
    }
    let directory = root.join("tests/scenarios/weird");
    let scenario = Scenario::load(&directory, &directory.join("test.yaml")).unwrap();
    let expected = scenario
        .cases
        .iter()
        .filter(|case| !case.expected_failures.is_empty())
        .map(|case| {
            (
                case.id.as_str(),
                case.expected_failures
                    .iter()
                    .map(|target| target.name())
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(expected, [("weird/dotnet-ryujit", vec!["amd64"])]);
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

#[test]
fn copy_scenario_has_four_typed_api_cases() {
    let mut root = Path::new(env!("CARGO_MANIFEST_DIR"));
    while !root.join("tests/scenarios/copy/test.yaml").is_file() {
        root = root.parent().expect("workspace root contains copy scenario");
    }
    let directory = root.join("tests/scenarios/copy");
    let scenario = Scenario::load(&directory, &directory.join("test.yaml")).unwrap();
    assert_eq!(scenario.cases.len(), 4);
    assert!(scenario.cases.iter().all(|case| {
        case.id.starts_with("cpcmd/") && case.actions.iter().any(|action| matches!(action, Step::Api(_)))
    }));
}
