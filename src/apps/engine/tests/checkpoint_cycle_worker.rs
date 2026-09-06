#![cfg(target_os = "linux")]

use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn worker() -> &'static str {
    #[cfg(target_arch = "aarch64")]
    return env!("CARGO_BIN_EXE_hl-aarch64");
    #[cfg(target_arch = "x86_64")]
    return env!("CARGO_BIN_EXE_hl-x86_64");
    #[allow(unreachable_code)]
    panic!("checkpoint-cycle integration test supports only aarch64 and x86_64");
}

fn guest_isa() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    }
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
    let compile = Command::new("cc")
        .args(["-static", "-O2", "-o"])
        .arg(&probe)
        .arg(source)
        .status()
        .expect("compile checkpoint-cycle probe");
    assert!(
        compile.success(),
        "checkpoint-cycle probe compilation failed: {compile}"
    );

    let output = Command::new(worker())
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
