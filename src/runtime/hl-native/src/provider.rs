#![allow(unsafe_code)]

/// Exercises a deliberately retained native allocation so leak tooling can
/// prove that it observes the integrated C engine.
#[must_use]
pub fn leak_check_nonvacuity() -> i32 {
    // SAFETY: the symbol takes no arguments and owns its test allocation.
    unsafe { super::bindings::hl_c_backend_leak_check_nonvacuity() }
}

/// Creates and destroys one engine from the host's `/bin/true` image after relocation.
#[cfg(unix)]
#[doc(hidden)]
pub fn artifact_lifecycle_smoke() -> Result<(), String> {
    use std::os::fd::AsRawFd as _;

    let root = std::ffi::CString::new("/").map_err(|error| error.to_string())?;
    let executable = std::ffi::CString::new("/bin/true").map_err(|error| error.to_string())?;
    let standard = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
        .map_err(|error| error.to_string())?;
    let isa = match std::env::consts::ARCH {
        "aarch64" => 1,
        "x86_64" => 2,
        architecture => return Err(format!("unsupported native smoke architecture {architecture}")),
    };
    let config = super::EngineConfig {
        isa,
        rootfs: Some(&root),
        executable_host: Some(&executable),
        executable_fd: -1,
        option_names: &[],
        option_values: &[],
        standard_fds: [standard.as_raw_fd(); 3],
        provider_fd: -1,
    };
    // SAFETY: every borrowed string, slice, and descriptor remains live through create. Dropping
    // the resulting unique owner immediately exercises the relocated destroy boundary too.
    let engine = unsafe { super::Engine::create(config) }.map_err(|error| error.to_string())?;
    drop(engine);
    Ok(())
}
