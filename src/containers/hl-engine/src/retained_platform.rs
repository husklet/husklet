//! Build-time availability for the integrated retained-C production backend.

/// Returns whether the retained source manifest currently supplies every
/// host-service, host-emitter, and guest-target translation unit required by
/// this product target.
pub fn supported(target_os: &str, target_arch: &str) -> bool {
    matches!(target_os, "linux" | "macos") && target_arch == "aarch64"
}

/// Whether the retained backend is selected when a launch does not name one.
pub fn production_default(target_os: &str, target_arch: &str) -> bool {
    target_os == "linux" && target_arch == "aarch64"
}
