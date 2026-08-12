//! Build-time availability for the integrated retained-C production backend.

/// Returns whether the retained source manifest currently supplies every
/// host-service, host-emitter, and guest-target translation unit required by
/// this product target.
pub fn supported(target_os: &str, target_arch: &str) -> bool {
    matches!(target_os, "linux" | "macos") && target_arch == "aarch64"
}

/// Whether the integrated C backend is selected when a launch does not name one.
///
/// Availability is the capability boundary. A supported product must not require
/// an otherwise-redundant backend selector merely because it runs on macOS.
pub fn production_default(target_os: &str, target_arch: &str) -> bool {
    supported(target_os, target_arch)
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_supported_product_defaults_to_c() {
        for target_os in ["linux", "macos"] {
            assert!(super::supported(target_os, "aarch64"));
            assert!(super::production_default(target_os, "aarch64"));
            assert!(!super::supported(target_os, "x86_64"));
            assert!(!super::production_default(target_os, "x86_64"));
        }
    }
}
