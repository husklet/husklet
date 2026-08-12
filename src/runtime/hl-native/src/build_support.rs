//! Cargo build-script policy shared with its unit tests.

use std::fmt::Write;

pub(crate) const BUILD_POLICY_INPUTS: &[&str] =
    &["build.rs", "src/artifact.rs", "src/build_support.rs", "src/platform.rs"];

pub(crate) const COMPILER_ENVIRONMENT_INPUTS: &[&str] =
    &["AR", "ARFLAGS", "CC", "CFLAGS", "CPPFLAGS", "CRATE_CC_NO_DEFAULTS"];

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

pub(crate) fn darwin_export_list(symbols: &[&str]) -> String {
    symbols_with_affixes(symbols, "", "_", "\n")
}

pub(crate) fn windows_export_definition(symbols: &[&str]) -> String {
    symbols_with_affixes(symbols, "EXPORTS\n", "  ", "\n")
}

pub(crate) fn linux_export_map(symbols: &[&str]) -> String {
    symbols_with_affixes(symbols, "HL_NATIVE_1 {\n  global:\n", "    ", ";\n") + "  local: *;\n};\n"
}

fn symbols_with_affixes(symbols: &[&str], header: &str, prefix: &str, suffix: &str) -> String {
    let mut manifest = String::from(header);
    for symbol in symbols {
        write!(manifest, "{prefix}{symbol}{suffix}").expect("writing to a String cannot fail");
    }
    manifest
}

#[cfg(test)]
mod tests {
    #[test]
    fn compiler_environment_inputs_are_complete_and_stably_ordered() {
        assert_eq!(
            super::COMPILER_ENVIRONMENT_INPUTS,
            &["AR", "ARFLAGS", "CC", "CFLAGS", "CPPFLAGS", "CRATE_CC_NO_DEFAULTS"]
        );
    }

    #[test]
    fn build_policy_modules_are_complete_and_stably_ordered() {
        assert_eq!(
            super::BUILD_POLICY_INPUTS,
            &["build.rs", "src/artifact.rs", "src/build_support.rs", "src/platform.rs"]
        );
        for path in super::BUILD_POLICY_INPUTS {
            assert!(
                std::path::Path::new(path).is_file(),
                "missing build policy input {path}"
            );
        }
    }

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

    #[test]
    fn export_manifests_apply_each_platforms_linker_grammar() {
        let symbols = ["hl_engine_abi", "hl_engine_version"];
        assert_eq!(
            super::darwin_export_list(&symbols),
            "_hl_engine_abi\n_hl_engine_version\n"
        );
        assert_eq!(
            super::windows_export_definition(&symbols),
            "EXPORTS\n  hl_engine_abi\n  hl_engine_version\n"
        );
        assert_eq!(
            super::linux_export_map(&symbols),
            "HL_NATIVE_1 {\n  global:\n    hl_engine_abi;\n    hl_engine_version;\n  local: *;\n};\n"
        );
    }
}
