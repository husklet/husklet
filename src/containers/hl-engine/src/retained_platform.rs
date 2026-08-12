//! Build-time availability for the temporary retained-C production backend.

/// Returns whether the retained source manifest currently supplies every
/// host-service, host-emitter, and guest-target translation unit required by
/// this product target.
pub fn supported(target_os: &str, target_arch: &str) -> bool {
    target_os == "linux" && target_arch == "aarch64"
}

#[cfg(test)]
mod tests {
    #[test]
    fn availability_matches_the_compiled_source_manifest() {
        assert!(super::supported("linux", "aarch64"));
        for (target_os, target_arch) in [("linux", "x86_64"), ("macos", "aarch64"), ("macos", "x86_64")] {
            assert!(!super::supported(target_os, target_arch));
        }
    }
}
