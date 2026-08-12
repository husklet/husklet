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

#[cfg(test)]
mod tests {
    #[test]
    fn availability_matches_the_compiled_source_manifest() {
        assert!(super::supported("linux", "aarch64"));
        assert!(super::supported("macos", "aarch64"));
        assert!(super::production_default("linux", "aarch64"));
        assert!(!super::production_default("macos", "aarch64"));
        for (target_os, target_arch) in [("linux", "x86_64"), ("macos", "x86_64")] {
            assert!(!super::supported(target_os, target_arch));
        }
    }

    #[test]
    fn macos_host_closure_is_owned_by_the_retained_manifest() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../runtime/native/retained");
        let manifest = std::fs::read_to_string(root.join("RUNTIME_SOURCES.manifest")).unwrap();
        for source in [
            "include/hl/macos.h",
            "src/host/macos/directory.c",
            "src/host/macos/host.c",
            "src/host/macos/probe.h",
            "src/host/macos/process.c",
            "src/host/macos/range.c",
            "src/host/macos/system.c",
        ] {
            assert!(
                manifest.lines().any(|entry| entry == source),
                "missing retained source {source}"
            );
            assert!(root.join(source).is_file(), "missing retained file {source}");
        }
    }
}
