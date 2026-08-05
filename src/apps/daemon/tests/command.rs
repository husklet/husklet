use std::process::Command;

#[test]
fn installed_command_reports_help_without_constructing_the_runtime() {
    let output = Command::new(env!("CARGO_BIN_EXE_dockerd"))
        .arg("--help")
        .output()
        .expect("run dockerd help");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("help is UTF-8");
    assert!(stdout.contains("--data-root"));
    assert!(stdout.contains("--host"));
    assert!(stdout.contains("/var/lib/docker"));
    assert!(stdout.contains("unix:///var/run/docker.sock"));
}

#[test]
fn installed_command_reports_its_release_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_dockerd"))
        .arg("--version")
        .output()
        .expect("run dockerd version");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("version is UTF-8");
    assert_eq!(stdout.trim(), format!("dockerd {}", env!("CARGO_PKG_VERSION")));
}
