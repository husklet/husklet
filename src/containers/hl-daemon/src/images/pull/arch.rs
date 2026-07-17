//! docker `--platform` ↔ hl-arch mapping: hl-images target arch ↔ runtime [`Guest`] personality,
//! image-config arch detection, docker arch labels, and the pull arch-preference list.
use super::*;
use hl_jit::Guest;

/// Map a hl-images (runtime-agnostic) target arch onto the runtime's guest personality.
pub(crate) fn guest_of(a: hl_images::Arch) -> Guest {
    match a {
        hl_images::Arch::LinuxAarch64 => Guest::LinuxAarch64,
        hl_images::Arch::LinuxX86_64 => Guest::LinuxX86_64,
    }
}

/// The image config's declared guest arch, if recognizable (hl-images detection mapped to a `Guest`).
pub(crate) fn manifest_arch(config: &Value) -> Option<Guest> {
    hl_images::arch_from_config(config).map(guest_of)
}

/// docker arch label for a guest target.
pub(crate) fn docker_arch(g: Guest) -> &'static str {
    if g.arch() == "x86_64" {
        "amd64"
    } else {
        "arm64"
    }
}

/// A docker `--platform` value mapped to hl's arch label, if recognized. OCI platform strings are
/// `os/arch[/variant]`; hl only runs Linux guests, so an explicit OS prefix MUST be `linux` — a
/// non-Linux request (`windows/amd64`, `nonsense/amd64`) is rejected (`None`) rather than silently
/// mapped to the matching arch. Accepted forms: a bare `<arch>` (no OS prefix), `linux/<arch>`, or
/// `linux/<arch>/<variant>` (a trailing variant like `.../arm64/v8` must not hide the arch).
pub(crate) fn platform_arch(platform: Option<&str>) -> Option<&'static str> {
    let p = platform?;
    if p.is_empty() {
        return None;
    }
    let segs: Vec<&str> = p.split('/').collect();
    // With an OS prefix present (2+ segments), the OS must be linux; the arch is then in the remaining
    // segments. A single bare segment is treated as the arch itself (no OS to validate).
    let arch_segs: &[&str] = if segs.len() == 1 {
        &segs[..]
    } else {
        if segs[0] != "linux" {
            return None;
        }
        &segs[1..]
    };
    arch_segs.iter().find_map(|seg| match *seg {
        "amd64" | "x86_64" => Some("amd64"),
        "arm64" | "aarch64" => Some("arm64"),
        _ => None,
    })
}

/// Preferred arch list when pulling for a given platform: the requested one, else native-arm64 first.
pub(crate) fn platform_archs(platform: Option<&str>) -> Vec<&'static str> {
    match platform_arch(platform) {
        Some(a) => vec![a],
        None => vec!["arm64", "amd64"],
    }
}

#[cfg(test)]
mod arch_map_tests {
    //! Characterization tests: lock the *current* behavior of the `--platform` ↔ arch mappers.
    //! These assert what the code actually does today, not what docker conventions might imply.
    use super::*;

    // ---- guest_of: hl-images Arch -> runtime Guest personality ----
    #[test]
    fn guest_of_maps_each_arch_to_matching_guest() {
        assert_eq!(guest_of(hl_images::Arch::LinuxAarch64), Guest::LinuxAarch64);
        assert_eq!(guest_of(hl_images::Arch::LinuxX86_64), Guest::LinuxX86_64);
    }

    // ---- docker_arch: Guest -> docker arch label ----
    #[test]
    fn docker_arch_x86_is_amd64_everything_else_arm64() {
        assert_eq!(docker_arch(Guest::LinuxX86_64), "amd64");
        assert_eq!(docker_arch(Guest::LinuxAarch64), "arm64");
    }

    // ---- docker_arch ∘ guest_of round-trip for both linux arches ----
    #[test]
    fn docker_arch_guest_of_roundtrip() {
        assert_eq!(docker_arch(guest_of(hl_images::Arch::LinuxX86_64)), "amd64");
        assert_eq!(
            docker_arch(guest_of(hl_images::Arch::LinuxAarch64)),
            "arm64"
        );
    }

    // ---- platform_arch: docker --platform string -> hl arch label ----
    #[test]
    fn platform_arch_amd64_aliases() {
        assert_eq!(platform_arch(Some("linux/amd64")), Some("amd64"));
        assert_eq!(platform_arch(Some("amd64")), Some("amd64"));
        assert_eq!(platform_arch(Some("x86_64")), Some("amd64"));
        assert_eq!(platform_arch(Some("linux/x86_64")), Some("amd64"));
    }

    #[test]
    fn platform_arch_arm64_aliases() {
        assert_eq!(platform_arch(Some("linux/arm64")), Some("arm64"));
        assert_eq!(platform_arch(Some("arm64")), Some("arm64"));
        assert_eq!(platform_arch(Some("aarch64")), Some("arm64"));
        assert_eq!(platform_arch(Some("linux/aarch64")), Some("arm64"));
    }

    #[test]
    fn platform_arch_none_and_garbage_yield_none() {
        assert_eq!(platform_arch(None), None);
        assert_eq!(platform_arch(Some("")), None);
        assert_eq!(platform_arch(Some("windows/riscv")), None);
        assert_eq!(platform_arch(Some("banana")), None);
        // 32-bit arm (`arm`/`v7`) is not a supported hl arch -> None.
        assert_eq!(platform_arch(Some("linux/arm/v7")), None);
    }

    // Finding 19: a non-Linux OS prefix must be REJECTED, not silently mapped to the matching arch.
    // hl only runs Linux guests, so `windows/amd64` / `nonsense/amd64` are None; only `linux/<arch>`
    // (or a bare `<arch>`) is accepted.
    #[test]
    fn platform_arch_rejects_non_linux_os_prefix() {
        // Non-linux OS prefixes are rejected even though the arch token is recognized.
        assert_eq!(platform_arch(Some("windows/amd64")), None);
        assert_eq!(platform_arch(Some("nonsense/amd64")), None);
        assert_eq!(platform_arch(Some("darwin/arm64")), None);
        // The linux OS prefix (and a bare arch) are accepted.
        assert_eq!(platform_arch(Some("linux/amd64")), Some("amd64"));
        assert_eq!(platform_arch(Some("linux/arm64")), Some("arm64"));
        assert_eq!(platform_arch(Some("amd64")), Some("amd64"));
        // A linux prefix with a trailing variant still resolves the arch.
        assert_eq!(platform_arch(Some("linux/arm64/v8")), Some("arm64"));
    }

    #[test]
    fn platform_arch_honors_oci_variant_suffix() {
        // OCI `os/arch/variant`: the arch is a MIDDLE segment, not the last. A trailing variant must
        // not hide it (regression guard for the old rsplit-last-segment bug).
        assert_eq!(platform_arch(Some("linux/arm64/v8")), Some("arm64"));
        assert_eq!(platform_arch(Some("linux/amd64/v3")), Some("amd64"));
    }

    // ---- platform_archs: preference list for a pull ----
    #[test]
    fn platform_archs_requested_platform_is_singleton() {
        assert_eq!(platform_archs(Some("linux/amd64")), vec!["amd64"]);
        assert_eq!(platform_archs(Some("linux/arm64")), vec!["arm64"]);
        assert_eq!(platform_archs(Some("aarch64")), vec!["arm64"]);
    }

    #[test]
    fn platform_archs_default_prefers_arm64_then_amd64() {
        // None (no --platform) and unrecognized platforms both fall back to native-arm64 first.
        assert_eq!(platform_archs(None), vec!["arm64", "amd64"]);
        assert_eq!(platform_archs(Some("garbage")), vec!["arm64", "amd64"]);
        // A recognized variant-suffixed platform is honored as a singleton, not defaulted.
        assert_eq!(platform_archs(Some("linux/arm64/v8")), vec!["arm64"]);
    }
}
