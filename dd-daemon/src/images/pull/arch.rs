//! docker `--platform` ↔ dd-arch mapping: dd-images target arch ↔ runtime [`Guest`] personality,
//! image-config arch detection, docker arch labels, and the pull arch-preference list.
use super::*;
use ddjit::Guest;

/// Map a dd-images (runtime-agnostic) target arch onto the runtime's guest personality.
pub(crate) fn guest_of(a: dd_images::Arch) -> Guest {
    match a {
        dd_images::Arch::LinuxAarch64 => Guest::LinuxAarch64,
        dd_images::Arch::LinuxX86_64 => Guest::LinuxX86_64,
        dd_images::Arch::DarwinAarch64 => Guest::DarwinAarch64,
    }
}

/// The image config's declared guest arch, if recognizable (dd-images detection mapped to a `Guest`).
pub(crate) fn manifest_arch(config: &Value) -> Option<Guest> {
    dd_images::arch_from_config(config).map(guest_of)
}

/// docker arch label for a guest target.
pub(crate) fn docker_arch(g: Guest) -> &'static str {
    if g.arch() == "x86_64" {
        "amd64"
    } else {
        "arm64"
    }
}

/// A docker `--platform` value ("linux/amd64", "arm64", …) mapped to dd's arch label, if recognized.
pub(crate) fn platform_arch(platform: Option<&str>) -> Option<&'static str> {
    match platform?.rsplit('/').next().unwrap_or("") {
        "amd64" | "x86_64" => Some("amd64"),
        "arm64" | "aarch64" => Some("arm64"),
        _ => None,
    }
}

/// Preferred arch list when pulling for a given platform: the requested one, else native-arm64 first.
pub(crate) fn platform_archs(platform: Option<&str>) -> Vec<&'static str> {
    match platform_arch(platform) {
        Some(a) => vec![a],
        None => vec!["arm64", "amd64"],
    }
}
