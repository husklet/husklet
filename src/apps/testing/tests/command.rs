use std::process::Command;

#[test]
fn installed_runner_rejects_a_vacuous_scenario_selection() {
    let output = Command::new(env!("CARGO_BIN_EXE_testing"))
        .args(["scenarios", "permissions", "--class", "long", "--list"])
        .output()
        .expect("run installed testing command");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty(), "vacuous selection emitted rows");
    let stderr = String::from_utf8(output.stderr).expect("diagnostic is UTF-8");
    assert!(
        stderr.contains("scenario selection produced no executable case/target/sample rows"),
        "unexpected diagnostic: {stderr}"
    );
}
