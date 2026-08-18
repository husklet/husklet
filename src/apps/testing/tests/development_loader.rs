//! Verifies that downstream Cargo development executables find the private native engine themselves.

#[cfg(all(unix, debug_assertions))]
#[test]
fn downstream_executable_starts_without_loader_environment() {
    const CHILD: &str = "HL_NATIVE_DEVELOPMENT_LOADER_CHILD";
    assert!(hl_native::artifact_smoke(), "native engine ABI probe failed");
    let paths = hl_native::artifact_paths().expect("loaded native engine path");
    let loaded = paths.first().expect("native engine path entry");
    let loaded = loaded.canonicalize().expect("canonical native engine path");
    assert_eq!(
        loaded.file_name().and_then(|name| name.to_str()),
        Some(hl_native::artifact_filename())
    );
    assert!(
        paths
            .iter()
            .all(|path| path.canonicalize().is_ok_and(|path| path == loaded))
    );
    eprintln!("loaded native engine: {}", loaded.display());
    if std::env::var_os(CHILD).is_some() {
        // The explicit Rust loader crossed the real C ABI after every ambient
        // loader search variable had been removed from this fresh process.
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

    assert!(
        status.success(),
        "downstream development executable failed to restart: {status}"
    );
}

#[cfg(not(all(unix, debug_assertions)))]
#[test]
fn downstream_executable_starts_without_loader_environment() {}
