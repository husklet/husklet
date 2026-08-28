#![cfg(all(not(feature = "native-test-hooks"), target_os = "linux", target_arch = "x86_64"))]

use std::{collections::BTreeMap, path::Path, process::Command};

const PREFIX: &str = "[diag] backend-shape ";
const FIELDS: [&str; 21] = [
    "version",
    "available",
    "mixed_sse_executed",
    "mixed_sse_executed_transitions",
    "mixed_sse_disabled_boundaries",
    "jcc_ibtc_enabled",
    "jcc_ibtc_emitted",
    "jcc_ibtc_hits",
    "jcc_ibtc_misses",
    "jcc_ibtc_irq",
    "jcc_ibtc_fills",
    "jcc_ibtc_suppressed",
    "jcc_ibtc_invalid_refusals",
    "direct_jmp_ibtc_enabled",
    "direct_jmp_ibtc_emitted",
    "direct_jmp_ibtc_hits",
    "direct_jmp_ibtc_misses",
    "direct_jmp_ibtc_irq",
    "direct_jmp_ibtc_fills",
    "direct_jmp_ibtc_suppressed",
    "direct_jmp_ibtc_invalid_refusals",
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
    if fields["version"] != 4 || fields["available"] != 1 {
        return Err("production mixed-SSE census is unavailable or has the wrong version".into());
    }
    if fields["mixed_sse_executed_transitions"] < fields["mixed_sse_executed"]
        || (fields["mixed_sse_executed"] == 0 && fields["mixed_sse_executed_transitions"] != 0)
        || (fields["mixed_sse_executed"] != 0 && fields["mixed_sse_disabled_boundaries"] != 0)
    {
        return Err("production mixed-SSE census does not reconcile".into());
    }
    let dispositions = fields["jcc_ibtc_fills"]
        .checked_add(fields["jcc_ibtc_suppressed"])
        .and_then(|value| value.checked_add(fields["jcc_ibtc_invalid_refusals"]));
    if dispositions != Some(fields["jcc_ibtc_misses"]) {
        return Err("production JCC IBTC miss dispositions do not reconcile".into());
    }
    if fields["jcc_ibtc_enabled"] > 1
        || (fields["jcc_ibtc_enabled"] == 0 && (fields["jcc_ibtc_hits"] != 0 || fields["jcc_ibtc_fills"] != 0))
        || (fields["jcc_ibtc_enabled"] == 1 && fields["jcc_ibtc_suppressed"] != 0)
    {
        return Err("production JCC IBTC polarity is inconsistent".into());
    }
    let dynamic = fields["jcc_ibtc_hits"]
        .checked_add(fields["jcc_ibtc_misses"])
        .and_then(|value| value.checked_add(fields["jcc_ibtc_irq"]));
    if dynamic.is_none() || (dynamic != Some(0) && fields["jcc_ibtc_emitted"] == 0) {
        return Err("production JCC IBTC execution has no emitted site".into());
    }
    let direct_dispositions = fields["direct_jmp_ibtc_fills"]
        .checked_add(fields["direct_jmp_ibtc_suppressed"])
        .and_then(|value| value.checked_add(fields["direct_jmp_ibtc_invalid_refusals"]));
    if direct_dispositions != Some(fields["direct_jmp_ibtc_misses"]) {
        return Err("production direct-JMP IBTC miss dispositions do not reconcile".into());
    }
    if fields["direct_jmp_ibtc_enabled"] > 1
        || (fields["direct_jmp_ibtc_enabled"] == 0
            && (fields["direct_jmp_ibtc_hits"] != 0 || fields["direct_jmp_ibtc_fills"] != 0))
        || (fields["direct_jmp_ibtc_enabled"] == 1 && fields["direct_jmp_ibtc_suppressed"] != 0)
    {
        return Err("production direct-JMP IBTC polarity is inconsistent".into());
    }
    let direct_dynamic = fields["direct_jmp_ibtc_hits"]
        .checked_add(fields["direct_jmp_ibtc_misses"])
        .and_then(|value| value.checked_add(fields["direct_jmp_ibtc_irq"]));
    if direct_dynamic.is_none() || (direct_dynamic != Some(0) && fields["direct_jmp_ibtc_emitted"] == 0) {
        return Err("production direct-JMP IBTC execution has no emitted site".into());
    }
    Ok(fields)
}

fn put16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn build_jcc_ibtc_fixture(root: &Path) {
    use std::os::unix::fs::PermissionsExt as _;

    let mut bytes = vec![0; 4096];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    put16(&mut bytes, 16, 2);
    put16(&mut bytes, 18, 0x3e);
    put32(&mut bytes, 20, 1);
    put64(&mut bytes, 24, 0x40_0100);
    put64(&mut bytes, 32, 64);
    put16(&mut bytes, 52, 64);
    put16(&mut bytes, 54, 56);
    put16(&mut bytes, 56, 1);
    put32(&mut bytes, 64, 1);
    put32(&mut bytes, 68, 5);
    put64(&mut bytes, 80, 0x40_0000);
    put64(&mut bytes, 88, 0x40_0000);
    put64(&mut bytes, 96, 4096);
    put64(&mut bytes, 104, 4096);
    put64(&mut bytes, 112, 4096);
    bytes[0x100..0x106].copy_from_slice(&[0x31, 0xc0, 0x74, 0x0c, 0x0f, 0x0b]);
    bytes[0x110..0x120].copy_from_slice(&[
        0xff, 0xc1, 0x83, 0xf9, 0x02, 0x7c, 0xe9, 0xb8, 0x3c, 0, 0, 0, 0x31, 0xff, 0x0f, 0x05,
    ]);
    let destination = root.join("bin/jcc-ibtc");
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    std::fs::write(&destination, bytes).unwrap();
    std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn build_direct_jmp_ibtc_fixture(root: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let mut bytes = vec![0; 0x3000];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    put16(&mut bytes, 16, 2);
    put16(&mut bytes, 18, 0x3e);
    put32(&mut bytes, 20, 1);
    put64(&mut bytes, 24, 0x40_1ff0);
    put64(&mut bytes, 32, 64);
    put16(&mut bytes, 52, 64);
    put16(&mut bytes, 54, 56);
    put16(&mut bytes, 56, 1);
    put32(&mut bytes, 64, 1);
    put32(&mut bytes, 68, 5);
    put64(&mut bytes, 80, 0x40_0000);
    put64(&mut bytes, 88, 0x40_0000);
    let image_len = bytes.len() as u64;
    put64(&mut bytes, 96, image_len);
    put64(&mut bytes, 104, image_len);
    put64(&mut bytes, 112, 0x1000);
    // Cross-page direct JMP executes twice: first miss publishes 0x402000, second execution hits it.
    bytes[0x1ff0..0x1ff6].copy_from_slice(&[0x31, 0xc9, 0xeb, 0x0c, 0x0f, 0x0b]);
    bytes[0x2000..0x2010].copy_from_slice(&[
        0xff, 0xc1, 0x83, 0xf9, 0x02, 0x7c, 0xeb, 0xb8, 0x3c, 0, 0, 0, 0x31, 0xff, 0x0f, 0x05,
    ]);
    let destination = root.join("bin/direct-jmp-ibtc");
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    std::fs::write(&destination, bytes).unwrap();
    std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn run_direct_jmp_ibtc(root: &Path, mode: &str) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hl-x86_64"));
    command.args(["--diagnostics", "--translit"]);
    if mode != "default" {
        command.arg(format!("--translit-direct-jmp-ibtc={mode}"));
    }
    command.args(["--rootfs", root.to_str().unwrap(), "bin/direct-jmp-ibtc"]);
    if mode == "on" {
        // The typed immutable launch option must shadow this contradictory
        // ambient fallback when explicitly enabling the feature.
        command.env("HL_TRANSLIT_DIRECT_JMP_IBTC_DISABLE", "1");
    }
    command.output().unwrap()
}

fn run_jcc_ibtc(root: &Path, mode: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_hl-x86_64"))
        .args([
            "--diagnostics",
            "--translit",
            &format!("--translit-jcc-ibtc={mode}"),
            "--rootfs",
            root.to_str().unwrap(),
            "bin/jcc-ibtc",
        ])
        // A typed launch option store must shadow, not import, this contradictory ambient value.
        .env("HL_TRANSLIT_JCC_IBTC_DISABLE", "1")
        .output()
        .expect("run production no-hooks JCC IBTC worker")
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

fn run_fatal_root(root: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_hl-x86_64"))
        .args([
            "--diagnostics",
            "--translit",
            "--translit-mixed-sse=on",
            "--rootfs",
            root.to_str().unwrap(),
            "bin/mixed-sse-child",
            "fatal-root",
        ])
        .output()
        .expect("run production no-hooks worker with fatal root")
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
    assert_eq!(enabled["jcc_ibtc_enabled"], 1);
    assert_eq!(disabled["jcc_ibtc_enabled"], 1);
    assert!(enabled["mixed_sse_executed"] > 0, "{enabled:?}");
    assert!(enabled["mixed_sse_executed_transitions"] >= enabled["mixed_sse_executed"]);
    assert_eq!(enabled["mixed_sse_disabled_boundaries"], 0);
    assert_eq!(disabled["mixed_sse_executed"], 0);
    assert_eq!(disabled["mixed_sse_executed_transitions"], 0);
    assert!(disabled["mixed_sse_disabled_boundaries"] > 0, "{disabled:?}");
}

#[test]
fn real_worker_cli_typed_jcc_ibtc_on_and_off_reach_product_v4() {
    let root = tempfile::tempdir().unwrap();
    build_jcc_ibtc_fixture(root.path());
    let on = run_jcc_ibtc(root.path(), "on");
    let off = run_jcc_ibtc(root.path(), "off");
    assert!(on.status.success(), "{}", String::from_utf8_lossy(&on.stderr));
    assert!(off.status.success(), "{}", String::from_utf8_lossy(&off.stderr));
    assert_eq!(on.stdout, off.stdout);

    let on_stderr = String::from_utf8(on.stderr).unwrap();
    let off_stderr = String::from_utf8(off.stderr).unwrap();
    let on = census(&on_stderr).unwrap_or_else(|error| panic!("{error}:\n{on_stderr}"));
    let off = census(&off_stderr).unwrap_or_else(|error| panic!("{error}:\n{off_stderr}"));
    assert_eq!(on["jcc_ibtc_enabled"], 1, "{on:?}");
    assert_eq!(on["jcc_ibtc_emitted"], 1, "{on:?}");
    assert_eq!(on["jcc_ibtc_hits"], 1, "{on:?}");
    assert_eq!(on["jcc_ibtc_misses"], 1, "{on:?}");
    assert_eq!(on["jcc_ibtc_fills"], 1, "{on:?}");
    assert_eq!(on["jcc_ibtc_suppressed"], 0, "{on:?}");
    assert_eq!(off["jcc_ibtc_enabled"], 0, "{off:?}");
    assert_eq!(off["jcc_ibtc_emitted"], 1, "{off:?}");
    assert_eq!(off["jcc_ibtc_hits"], 0, "{off:?}");
    assert_eq!(off["jcc_ibtc_misses"], 2, "{off:?}");
    assert_eq!(off["jcc_ibtc_fills"], 0, "{off:?}");
    assert_eq!(off["jcc_ibtc_suppressed"], 2, "{off:?}");
}

#[test]
fn real_worker_cli_cross_page_direct_jmp_ibtc_on_and_off_reach_product_v4() {
    let root = tempfile::tempdir().unwrap();
    build_direct_jmp_ibtc_fixture(root.path());
    let default = run_direct_jmp_ibtc(root.path(), "default");
    let on = run_direct_jmp_ibtc(root.path(), "on");
    let off = run_direct_jmp_ibtc(root.path(), "off");
    assert!(default.status.success(), "{}", String::from_utf8_lossy(&default.stderr));
    assert!(on.status.success(), "{}", String::from_utf8_lossy(&on.stderr));
    assert!(off.status.success(), "{}", String::from_utf8_lossy(&off.stderr));
    assert_eq!(on.stdout, off.stdout);
    assert_eq!(default.stdout, off.stdout);
    let default_stderr = String::from_utf8(default.stderr).unwrap();
    let on_stderr = String::from_utf8(on.stderr).unwrap();
    let off_stderr = String::from_utf8(off.stderr).unwrap();
    let on = census(&on_stderr).unwrap_or_else(|error| panic!("{error}:\n{on_stderr}"));
    let off = census(&off_stderr).unwrap_or_else(|error| panic!("{error}:\n{off_stderr}"));
    let default = census(&default_stderr).unwrap_or_else(|error| panic!("{error}:\n{default_stderr}"));
    assert_eq!(on["direct_jmp_ibtc_enabled"], 1, "{on:?}");
    assert!(on["direct_jmp_ibtc_emitted"] > 0, "{on:?}");
    assert_eq!(on["direct_jmp_ibtc_hits"], 1, "{on:?}");
    assert_eq!(on["direct_jmp_ibtc_misses"], 1, "{on:?}");
    assert_eq!(on["direct_jmp_ibtc_fills"], 1, "{on:?}");
    assert_eq!(on["direct_jmp_ibtc_suppressed"], 0, "{on:?}");
    assert_eq!(off["direct_jmp_ibtc_enabled"], 0, "{off:?}");
    assert_eq!(off["direct_jmp_ibtc_emitted"], 0, "{off:?}");
    assert_eq!(off["direct_jmp_ibtc_hits"], 0, "{off:?}");
    assert_eq!(off["direct_jmp_ibtc_misses"], 0, "{off:?}");
    assert_eq!(off["direct_jmp_ibtc_fills"], 0, "{off:?}");
    assert_eq!(off["direct_jmp_ibtc_suppressed"], 0, "{off:?}");
    for field in [
        "direct_jmp_ibtc_enabled",
        "direct_jmp_ibtc_emitted",
        "direct_jmp_ibtc_hits",
        "direct_jmp_ibtc_misses",
        "direct_jmp_ibtc_fills",
        "direct_jmp_ibtc_suppressed",
    ] {
        assert_eq!(default[field], 0, "{field}: {default:?}");
    }
}

#[test]
fn nohooks_parent_barrier_settles_a_child_that_outlives_a_fatal_root() {
    let root = tempfile::tempdir().unwrap();
    build_fixture(root.path());
    let output = run_fatal_root(root.path());
    assert!(!output.status.success(), "fatal-root fixture unexpectedly succeeded");
    assert_eq!(output.stdout, b"mixed-child=1\n");
    let stderr = String::from_utf8(output.stderr).unwrap();
    let report = census(&stderr).unwrap_or_else(|error| panic!("{error}:\n{stderr}"));
    assert!(report["mixed_sse_executed"] > 0, "{report:?}");
    assert!(report["mixed_sse_executed_transitions"] >= report["mixed_sse_executed"]);
    assert_eq!(report["mixed_sse_disabled_boundaries"], 0);
}

#[test]
fn product_census_parser_rejects_cardinality_and_coordinated_token_changes() {
    let valid = "[diag] backend-shape version=4 available=1 mixed_sse_executed=3 \
                 mixed_sse_executed_transitions=7 mixed_sse_disabled_boundaries=0 \
                 jcc_ibtc_enabled=1 jcc_ibtc_emitted=1 jcc_ibtc_hits=1 jcc_ibtc_misses=1 \
                 jcc_ibtc_irq=0 jcc_ibtc_fills=1 jcc_ibtc_suppressed=0 jcc_ibtc_invalid_refusals=0 \
                 direct_jmp_ibtc_enabled=1 direct_jmp_ibtc_emitted=1 direct_jmp_ibtc_hits=1 \
                 direct_jmp_ibtc_misses=1 direct_jmp_ibtc_irq=0 direct_jmp_ibtc_fills=1 \
                 direct_jmp_ibtc_suppressed=0 direct_jmp_ibtc_invalid_refusals=0\n";
    census(valid).unwrap();
    let invalid = [
        String::new(),
        format!("{valid}{valid}"),
        valid.replace(" mixed_sse_executed=3", ""),
        valid.replace("mixed_sse_executed=3", "mixed_sse_executed=three"),
        valid.replace("mixed_sse_executed=3", "mixed_sse_executed=3 mixed_sse_executed=4"),
        valid.replace("available=1", "available=1 extra=0"),
        valid.replace("available=1", "available=0"),
        valid.replace("jcc_ibtc_fills=1", "jcc_ibtc_fills=0"),
        valid.replace("jcc_ibtc_suppressed=0", "jcc_ibtc_suppressed=1"),
        valid.replace(" direct_jmp_ibtc_emitted=1", ""),
        valid.replace("direct_jmp_ibtc_hits=1", "direct_jmp_ibtc_hits=notdecimal"),
        valid.replace(
            "direct_jmp_ibtc_hits=1",
            "direct_jmp_ibtc_hits=1 direct_jmp_ibtc_hits=1",
        ),
        valid.replace("direct_jmp_ibtc_fills=1", "direct_jmp_ibtc_fills=0"),
        valid.replace("direct_jmp_ibtc_suppressed=0", "direct_jmp_ibtc_suppressed=1"),
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
