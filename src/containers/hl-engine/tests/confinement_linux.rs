#![cfg(target_os = "linux")]

#[test]
fn escape_denial() {
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_hl-confinement-child"))
        .status()
        .unwrap();
    assert!(status.success());
}
