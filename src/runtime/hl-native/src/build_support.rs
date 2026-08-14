//! Cargo build-script policy shared with its unit tests.

use std::fmt::Write;

pub(crate) const BUILD_POLICY_INPUTS: &[&str] =
    &["build.rs", "src/artifact.rs", "src/build_support.rs", "src/platform.rs"];

pub(crate) const COMPILER_ENVIRONMENT_INPUTS: &[&str] =
    &["AR", "ARFLAGS", "CC", "CFLAGS", "CPPFLAGS", "CRATE_CC_NO_DEFAULTS"];
pub(crate) const LINKER_ENVIRONMENT_INPUTS: &[&str] = &["CROSS_COMPILE", "RUSTC_LINKER", "RUSTC_WRAPPER"];
const TARGET_SCOPED_COMPILER_ENVIRONMENT_INPUTS: &[&str] = &["AR", "ARFLAGS", "CC", "CFLAGS", "CPPFLAGS"];

pub(crate) fn target_compiler_environment_inputs(target: &str) -> Vec<String> {
    let normalized = target.replace(['-', '.'], "_");
    TARGET_SCOPED_COMPILER_ENVIRONMENT_INPUTS
        .iter()
        .flat_map(|input| {
            [
                format!("{input}_{target}"),
                format!("{input}_{normalized}"),
                format!("HOST_{input}"),
                format!("TARGET_{input}"),
            ]
        })
        .collect()
}

pub(crate) const WINDOWS_SYSTEM_LIBRARIES: &[&str] = &[
    "kernel32",
    "ntdll",
    "advapi32",
    "bcrypt",
    "ws2_32",
    "synchronization",
    "userenv",
];

pub(crate) fn export_symbols(manifest: &str) -> Vec<&str> {
    let symbols = manifest.lines().filter(|line| !line.is_empty()).collect::<Vec<_>>();
    assert!(!symbols.is_empty(), "native bridge export manifest is empty");
    assert!(
        symbols.windows(2).all(|pair| pair[0] < pair[1]),
        "native bridge exports must be unique and sorted"
    );
    assert!(
        symbols.iter().all(|symbol| symbol.starts_with("hl_")
            && symbol.bytes().all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())),
        "native bridge export manifest contains an invalid C identifier"
    );
    symbols
}

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

pub(crate) fn static_archive_filename(target_os: &str, target_env: &str, name: &str) -> String {
    if target_os == "windows" && target_env == "msvc" {
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
    fn linker_environment_inputs_are_complete_and_stably_ordered() {
        assert_eq!(
            super::LINKER_ENVIRONMENT_INPUTS,
            &["CROSS_COMPILE", "RUSTC_LINKER", "RUSTC_WRAPPER"]
        );
    }

    #[test]
    fn target_compiler_environment_inputs_cover_cc_precedence_forms() {
        let inputs = super::target_compiler_environment_inputs("aarch64-unknown-linux.gnu");
        assert_eq!(
            &inputs[..4],
            &[
                "AR_aarch64-unknown-linux.gnu",
                "AR_aarch64_unknown_linux_gnu",
                "HOST_AR",
                "TARGET_AR",
            ]
        );
        assert_eq!(inputs.len(), super::TARGET_SCOPED_COMPILER_ENVIRONMENT_INPUTS.len() * 4);
        assert_eq!(
            &inputs[inputs.len() - 4..],
            &[
                "CPPFLAGS_aarch64-unknown-linux.gnu",
                "CPPFLAGS_aarch64_unknown_linux_gnu",
                "HOST_CPPFLAGS",
                "TARGET_CPPFLAGS",
            ]
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
    fn target_exit_paths_publish_matching_dispatch_diagnostics() {
        let schema = "[prof] dispatcher crossings=%llu translations=%llu";
        for target in ["aarch64", "x86_64"] {
            let path = format!("src/native/engine/target/{target}.c");
            let source = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path}: {error}"));
            assert_eq!(source.matches(schema).count(), 1, "{path} diagnostic schema drifted");
            assert!(
                source.contains("(unsigned long long)g_dispatch_profile.crossings"),
                "{path} omits dispatcher crossings"
            );
            assert!(
                source.contains("(unsigned long long)g_dispatch_profile.translations"),
                "{path} omits translation count"
            );
        }
    }

    #[test]
    fn target_checkpoint_layout_is_deterministic_on_both_isas() {
        let placement = "g_pcache || hl_option_get(\"HL_CHECKPOINT\")";
        for target in ["aarch64", "x86_64"] {
            let path = format!("src/native/engine/target/{target}.c");
            let source = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path}: {error}"));
            assert_eq!(
                source.matches(placement).count(),
                2,
                "{path} must select deterministic main and interpreter placement for checkpoint capture"
            );
        }
    }

    #[test]
    fn windows_link_inputs_use_target_spelling() {
        assert_eq!(
            super::static_archive_filename("windows", "msvc", "hl_engine"),
            "hl_engine.lib"
        );
        assert_eq!(
            super::static_archive_filename("windows", "gnu", "hl_engine"),
            "libhl_engine.a"
        );
        assert_eq!(
            super::static_archive_filename("linux", "gnu", "hl_engine"),
            "libhl_engine.a"
        );
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

    #[test]
    fn bridge_export_manifest_is_sorted_and_contains_checkpoint_configuration() {
        let manifest = include_str!("native/bridge/exports.txt");
        let symbols = super::export_symbols(manifest);
        assert!(symbols.contains(&"hl_c_backend_checkpoint_configure"));
        assert_eq!(symbols.len(), 22);
    }

    #[test]
    fn macos_fork_prerequisites_are_completed_in_the_parent() {
        let lifecycle = std::fs::read_to_string("src/native/engine/lifecycle.c").unwrap();
        let prepare = lifecycle
            .find("hl_linux_dns_prepare();")
            .expect("production lifecycle must prewarm the macOS resolver");
        let spawn = lifecycle[prepare..]
            .find("spawn_cloned")
            .map(|offset| prepare + offset)
            .expect("production lifecycle must spawn the guest");
        assert!(prepare < spawn, "resolver prewarm moved behind the production fork");

        let locks = std::fs::read_to_string("src/native/linux_abi/syscall/emulation_state.c").unwrap();
        let apple = locks
            .split("#if defined(__APPLE__)")
            .nth(1)
            .expect("record-lock initialization needs a macOS arm");
        assert!(apple.contains("MAP_SHARED | MAP_ANON"));
        assert!(locks.contains("/husklet-poslk-v1-%lu"));
    }
}
