#![cfg(target_os = "linux")]

mod engine;
mod guest;

use std::process::Command;

fn displaced_et_exec(isa: &str, engine_name: &str) {
    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("guest");
    guest::displaced_et_exec(isa, &executable);
    let engine = engine::EngineBinaryPaths::required().named(engine_name);
    let ordinary = Command::new(&engine)
        .args(["--guest-isa", isa])
        .arg(&executable)
        .env_remove("HL_TEST_FORCE_DISPLACED_ET_EXEC")
        .env_remove("HL_AUTHORITY_FD")
        .env_remove("HL_AUTHORITY_HEALTH_FD")
        .output()
        .unwrap();
    assert!(ordinary.status.success(), "{isa} ordinary ET_EXEC failed");
    assert_eq!(ordinary.stdout, b"displaced-et-exec-ok\n");
    assert!(!String::from_utf8_lossy(&ordinary.stderr).contains("hl-test-displaced-et-exec:"));
    let output = Command::new(engine)
        .args(["--guest-isa", isa])
        .arg(&executable)
        .env("HL_TEST_FORCE_DISPLACED_ET_EXEC", "1")
        .env_remove("HL_AUTHORITY_FD")
        .env_remove("HL_AUTHORITY_HEALTH_FD")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{isa} displaced ET_EXEC failed: {:?}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
        output.status.code()
    );
    assert_eq!(stdout, "displaced-et-exec-ok\n");
    let placement = stderr
        .lines()
        .find(|line| line.starts_with("hl-test-displaced-et-exec: "))
        .unwrap_or_else(|| panic!("{isa} did not prove displaced storage:\n{stderr}"));
    eprintln!("{isa} {placement}");
    assert_eq!(placement, "hl-test-displaced-et-exec: displaced");
}

#[test]
fn x86_64() {
    displaced_et_exec("x86_64", "hl-x86_64");
}

#[test]
fn aarch64() {
    displaced_et_exec("aarch64", "hl-aarch64");
}
