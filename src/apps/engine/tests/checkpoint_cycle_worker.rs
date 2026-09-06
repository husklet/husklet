#![cfg(target_os = "linux")]

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
compile_error!("checkpoint-cycle integration test supports only aarch64 and x86_64");

use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn worker() -> &'static str {
    #[cfg(target_arch = "aarch64")]
    {
        env!("CARGO_BIN_EXE_hl-aarch64")
    }
    #[cfg(target_arch = "x86_64")]
    {
        env!("CARGO_BIN_EXE_hl-x86_64")
    }
}

fn guest_compiler() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64-linux-gnu-gcc"
    } else {
        "x86_64-linux-gnu-gcc"
    }
}

fn compile_probe(source: &Path, probe: &Path) {
    let status = Command::new(guest_compiler())
        .args(["-static", "-O2", "-o"])
        .arg(probe)
        .arg(source)
        .status()
        .expect("compile checkpoint-cycle probe with the pinned guest toolchain");
    assert!(status.success(), "checkpoint-cycle probe compilation failed: {status}");
}

fn guest_isa() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    }
}

fn translated_backend_shape(stderr: &str) -> std::collections::BTreeMap<&str, u64> {
    let records = stderr
        .lines()
        .filter_map(|line| line.strip_prefix("[diag] backend-shape "))
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 1, "expected one backend-shape receipt: {stderr}");
    records[0]
        .split_whitespace()
        .map(|field| {
            let (name, value) = field
                .split_once('=')
                .unwrap_or_else(|| panic!("malformed backend-shape field {field:?}"));
            let value = value
                .parse::<u64>()
                .unwrap_or_else(|_| panic!("non-numeric backend-shape field {field:?}"));
            (name, value)
        })
        .collect()
}

#[test]
fn production_worker_captures_kills_restores_and_continues() {
    let fixture = tempfile::tempdir().unwrap();
    let rootfs = fixture.path().join("rootfs");
    let control = rootfs.join("run/checkpoint");
    std::fs::create_dir_all(rootfs.join("bin")).unwrap();
    std::fs::create_dir_all(&control).unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tests/runtime/checkpoint-translated/daily_dev.c");
    let probe = rootfs.join("bin/checkpoint-cycle-probe");
    compile_probe(&source, &probe);

    let output = Command::new(worker())
        .args([
            "--guest-isa",
            guest_isa(),
            "--native-supervised=on",
            "--loader-receipt",
            "--rootfs",
            rootfs.to_str().unwrap(),
            "--checkpoint-cycle",
            "/run/checkpoint",
            "bin/checkpoint-cycle-probe",
        ])
        .output()
        .expect("run production checkpoint-cycle worker");
    assert!(
        output.status.success(),
        "worker failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    let records = stderr
        .lines()
        .filter_map(|line| line.strip_prefix("[hl-checkpoint-cycle]\t"))
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 1, "{stderr}");
    assert_eq!(stderr.matches("[hl-loader]\t").count(), 1, "{stderr}");
    let receipt: Value = serde_json::from_str(records[0]).unwrap();
    assert_eq!(receipt["schema"], "husklet-checkpoint-cycle-v1");
    assert_eq!(receipt["guest_isa"], guest_isa());
    assert_eq!(receipt["backend"], "native");
    for leg in ["captured", "original_reaped", "restored_killed", "restored_continued"] {
        assert_eq!(receipt[leg], true, "missing lifecycle leg {leg}: {receipt}");
    }
    assert_eq!(receipt["final_guest_status"], 0);
    assert!(receipt["member_count"].as_u64().is_some_and(|count| count > 0));
}

#[test]
fn production_translated_worker_captures_kills_restores_and_continues() {
    let fixture = tempfile::tempdir().unwrap();
    let rootfs = fixture.path().join("rootfs");
    let control = rootfs.join("run/checkpoint");
    std::fs::create_dir_all(rootfs.join("bin")).unwrap();
    std::fs::create_dir_all(&control).unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tests/runtime/checkpoint-translated/daily_dev.c");
    let probe = rootfs.join("bin/checkpoint-cycle-probe");
    compile_probe(&source, &probe);
    let output = Command::new(worker())
        .args([
            "--guest-isa",
            guest_isa(),
            "--translit",
            "--diagnostics",
            "--loader-receipt",
            "--rootfs",
            rootfs.to_str().unwrap(),
            "--checkpoint-cycle",
            "/run/checkpoint",
            "bin/checkpoint-cycle-probe",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "translated worker failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    let receipt = stderr
        .lines()
        .filter_map(|line| line.strip_prefix("[hl-checkpoint-cycle]\t"))
        .collect::<Vec<_>>();
    assert_eq!(receipt.len(), 1, "{stderr}");
    let receipt: Value = serde_json::from_str(receipt[0]).unwrap();
    assert_eq!(receipt["backend"], "translated");
    assert_eq!(receipt["kill_exit_kind"], "Signal");
    assert_eq!(receipt["kill_guest_status"], 9);
    let shape = translated_backend_shape(&stderr);
    assert_eq!(shape.get("translation_codegen_available"), Some(&1), "{shape:?}");
    assert!(
        shape.get("translated_entries").is_some_and(|entries| *entries > 0),
        "{shape:?}"
    );
}

#[cfg(feature = "native-test-hooks")]
#[test]
fn failure_after_start_stops_and_reaps_the_probe() {
    let fixture = tempfile::tempdir().unwrap();
    let rootfs = fixture.path().join("rootfs");
    let control = rootfs.join("run/checkpoint");
    std::fs::create_dir_all(rootfs.join("bin")).unwrap();
    std::fs::create_dir_all(&control).unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tests/runtime/checkpoint-translated/daily_dev.c");
    let probe = rootfs.join("bin/checkpoint-cycle-probe");
    compile_probe(&source, &probe);
    let output = Command::new(worker())
        .env("HL_CHECKPOINT_CYCLE_TEST_FAIL_AFTER_START", "1")
        .args([
            "--guest-isa",
            guest_isa(),
            "--native-supervised=on",
            "--rootfs",
            rootfs.to_str().unwrap(),
            "--checkpoint-cycle",
            "/run/checkpoint",
            "bin/checkpoint-cycle-probe",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("injected checkpoint-cycle failure after start"));
    let transcript = std::fs::read_to_string(control.join("output")).unwrap();
    let pid = transcript
        .lines()
        .find_map(|line| line.strip_prefix("READY leader="))
        .and_then(|tail| tail.split_whitespace().next())
        .unwrap();
    assert!(
        !Path::new("/proc").join(pid).exists(),
        "probe pid {pid} survived worker refusal"
    );
}
