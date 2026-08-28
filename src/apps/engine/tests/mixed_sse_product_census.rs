#![cfg(all(not(feature = "native-test-hooks"), target_os = "linux", target_arch = "x86_64"))]

use std::{collections::BTreeMap, path::Path, process::Command};

const PREFIX: &str = "[diag] backend-shape ";
const FIELDS: [&str; 5] = [
    "version",
    "available",
    "mixed_sse_executed",
    "mixed_sse_executed_transitions",
    "mixed_sse_disabled_boundaries",
];

fn census(stderr: &str) -> Result<BTreeMap<&str, u64>, String> {
    let records = stderr
        .lines()
        .filter_map(|line| line.strip_prefix(PREFIX))
        .collect::<Vec<_>>();
    let [record] = records.as_slice() else {
        return Err(format!(
            "production mixed-SSE census appeared {} times, expected once",
            records.len()
        ));
    };
    let mut fields = BTreeMap::new();
    for token in record.split_whitespace() {
        let Some((name, value)) = token.split_once('=') else {
            return Err(format!("production mixed-SSE census has malformed token {token:?}"));
        };
        if !FIELDS.contains(&name) {
            return Err(format!("production mixed-SSE census has extra field {name:?}"));
        }
        let value = value
            .parse::<u64>()
            .map_err(|_| format!("production mixed-SSE census field {name:?} is not decimal"))?;
        if fields.insert(name, value).is_some() {
            return Err(format!("production mixed-SSE census duplicates field {name:?}"));
        }
    }
    for name in FIELDS {
        if !fields.contains_key(name) {
            return Err(format!("production mixed-SSE census omits field {name:?}"));
        }
    }
    if fields["version"] != 2 || fields["available"] != 1 {
        return Err("production mixed-SSE census is unavailable or has the wrong version".into());
    }
    if fields["mixed_sse_executed_transitions"] < fields["mixed_sse_executed"]
        || (fields["mixed_sse_executed"] == 0 && fields["mixed_sse_executed_transitions"] != 0)
        || (fields["mixed_sse_executed"] != 0 && fields["mixed_sse_disabled_boundaries"] != 0)
    {
        return Err("production mixed-SSE census does not reconcile".into());
    }
    Ok(fields)
}

fn build_fixture(root: &Path) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mixed_sse_child.c");
    let destination = root.join("bin/mixed-sse-child");
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    let status = Command::new("x86_64-linux-gnu-gcc")
        .args(["-O2", "-static-pie"])
        .arg(source)
        .arg("-o")
        .arg(destination)
        .status()
        .expect("compile child-only mixed-SSE fixture");
    assert!(status.success(), "fixture compiler exited {status}");
}

fn build_map_failure_injection(root: &Path) -> std::path::PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fail_shared_mmap.c");
    let output = root.join("fail-shared-mmap.so");
    let status = Command::new("cc")
        .args(["-shared", "-fPIC", "-O2"])
        .arg(source)
        .args(["-ldl", "-o"])
        .arg(&output)
        .status()
        .expect("compile shared-map failure injection");
    assert!(status.success(), "failure injection compiler exited {status}");
    output
}

fn run(root: &Path, mode: &str) -> std::process::Output {
    let option = format!("--translit-mixed-sse={mode}");
    Command::new(env!("CARGO_BIN_EXE_hl-x86_64"))
        .args([
            "--diagnostics",
            "--translit",
            option.as_str(),
            "--rootfs",
            root.to_str().unwrap(),
            "bin/mixed-sse-child",
        ])
        .output()
        .expect("run production no-hooks worker")
}

#[test]
fn nohooks_product_aggregates_child_only_mixed_execution_after_reap() {
    let root = tempfile::tempdir().unwrap();
    build_fixture(root.path());
    let enabled = run(root.path(), "on");
    let disabled = run(root.path(), "off");
    assert!(enabled.status.success(), "{}", String::from_utf8_lossy(&enabled.stderr));
    assert!(
        disabled.status.success(),
        "{}",
        String::from_utf8_lossy(&disabled.stderr)
    );
    assert_eq!(enabled.stdout, b"mixed-child=1\n");
    assert_eq!(disabled.stdout, enabled.stdout);

    let enabled_stderr = String::from_utf8(enabled.stderr).unwrap();
    let disabled_stderr = String::from_utf8(disabled.stderr).unwrap();
    let enabled = census(&enabled_stderr).unwrap_or_else(|error| panic!("{error}:\n{enabled_stderr}"));
    let disabled = census(&disabled_stderr).unwrap_or_else(|error| panic!("{error}:\n{disabled_stderr}"));
    assert!(enabled["mixed_sse_executed"] > 0, "{enabled:?}");
    assert!(enabled["mixed_sse_executed_transitions"] >= enabled["mixed_sse_executed"]);
    assert_eq!(enabled["mixed_sse_disabled_boundaries"], 0);
    assert_eq!(disabled["mixed_sse_executed"], 0);
    assert_eq!(disabled["mixed_sse_executed_transitions"], 0);
    assert!(disabled["mixed_sse_disabled_boundaries"] > 0, "{disabled:?}");
}

#[test]
fn product_census_parser_rejects_cardinality_and_coordinated_token_changes() {
    let valid = "[diag] backend-shape version=2 available=1 mixed_sse_executed=3 \
                 mixed_sse_executed_transitions=7 mixed_sse_disabled_boundaries=0\n";
    census(valid).unwrap();
    let invalid = [
        String::new(),
        format!("{valid}{valid}"),
        valid.replace(" mixed_sse_executed=3", ""),
        valid.replace("mixed_sse_executed=3", "mixed_sse_executed=three"),
        valid.replace("mixed_sse_executed=3", "mixed_sse_executed=3 mixed_sse_executed=4"),
        valid.replace("available=1", "available=1 extra=0"),
        valid.replace("available=1", "available=0"),
    ];
    for invalid in invalid {
        assert!(census(&invalid).is_err(), "accepted invalid census: {invalid}");
    }
}

#[test]
fn diagnostic_mapping_failure_is_a_launch_refusal_not_a_zero_census() {
    let root = tempfile::tempdir().unwrap();
    build_fixture(root.path());
    let injection = build_map_failure_injection(root.path());
    let output = Command::new(env!("CARGO_BIN_EXE_hl-x86_64"))
        .args([
            "--diagnostics",
            "--translit",
            "--translit-mixed-sse=on",
            "--rootfs",
            root.path().to_str().unwrap(),
            "bin/mixed-sse-child",
        ])
        .env("LD_PRELOAD", injection)
        .env("HL_TEST_FAIL_SHARED_ANON", "1")
        .output()
        .expect("run launch with injected shared-map failure");
    assert!(
        !output.status.success(),
        "injected mapping failure unexpectedly launched"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("engine refused this launch"), "{stderr}");
    assert_eq!(
        stderr.lines().filter(|line| line.starts_with(PREFIX)).count(),
        0,
        "{stderr}"
    );
}
