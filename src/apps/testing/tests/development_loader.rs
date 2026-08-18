//! Verifies that downstream Cargo development executables find the private native engine themselves.

#[cfg(all(unix, debug_assertions))]
#[test]
fn downstream_executable_starts_without_loader_environment() {
    const CHILD: &str = "HL_NATIVE_DEVELOPMENT_LOADER_CHILD";
    assert!(hl_native::artifact_smoke(), "native engine ABI probe failed");
    if std::env::var_os(CHILD).is_some() {
        // Reaching the test body proves that the operating-system loader found
        // the native engine before Rust process initialization began.
        return;
    }

    let status = std::process::Command::new(std::env::current_exe().expect("current test executable"))
        .args([
            "--exact",
            "downstream_executable_starts_without_loader_environment",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env_remove("LD_LIBRARY_PATH")
        .env_remove("DYLD_LIBRARY_PATH")
        .env_remove("DYLD_FALLBACK_LIBRARY_PATH")
        .status()
        .expect("restart downstream development executable");

    assert!(status.success(), "downstream development executable failed to restart: {status}");
}

#[cfg(not(all(unix, debug_assertions)))]
#[test]
fn downstream_executable_starts_without_loader_environment() {}
