//! Cargo build-script policy shared with its unit tests.

pub(crate) const WINDOWS_SYSTEM_LIBRARIES: &[&str] = &[
    "kernel32",
    "ntdll",
    "advapi32",
    "bcrypt",
    "ws2_32",
    "synchronization",
    "userenv",
];

pub(crate) fn source_matches(target_os: &str, source: &str) -> bool {
    if target_os == "windows"
        && matches!(
            source,
            "src/native/host/child.c"
                | "src/native/host/fork_wire.c"
                | "src/native/host/private.c"
                | "src/native/host/resolve.c"
        )
    {
        return false;
    }
    if source.starts_with("src/native/toolchain/msvc-posix/") {
        return target_os == "windows";
    }
    let Some(host_relative) = source.strip_prefix("src/native/host/") else {
        return true;
    };
    let Some((platform, _)) = host_relative.split_once('/') else {
        return true;
    };
    !matches!(platform, "linux" | "macos" | "windows") || platform == target_os
}

pub(crate) fn static_archive_filename(target_os: &str, name: &str) -> String {
    if target_os == "windows" {
        format!("{name}.lib")
    } else {
        format!("lib{name}.a")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn platform_source_closures_do_not_mix_host_implementations() {
        for target in ["linux", "macos", "windows"] {
            for platform in ["linux", "macos", "windows"] {
                let source = format!("src/native/host/{platform}/host.c");
                assert_eq!(super::source_matches(target, &source), target == platform);
            }
            assert!(super::source_matches(target, "src/native/engine/runtime.c"));
            assert!(super::source_matches(target, "src/native/host/sync.c"));
            assert_eq!(
                super::source_matches(target, "src/native/toolchain/msvc-posix/compatibility.c"),
                target == "windows"
            );
        }
    }

    #[test]
    fn windows_link_inputs_use_target_spelling() {
        assert_eq!(super::static_archive_filename("windows", "hl_engine"), "hl_engine.lib");
        assert_eq!(super::static_archive_filename("linux", "hl_engine"), "libhl_engine.a");
        assert!(super::WINDOWS_SYSTEM_LIBRARIES.contains(&"ws2_32"));
        assert!(super::WINDOWS_SYSTEM_LIBRARIES.contains(&"ntdll"));
    }
}
