//! Cargo build-script policy shared with its unit tests.

pub(crate) const BUILD_POLICY_INPUTS: &[&str] = &[
    "build.rs",
    "src/artifact.rs",
    "src/build_support.rs",
    "src/inventory/mod.rs",
    "src/inventory/sources.rs",
    "src/native_build.rs",
    "src/platform.rs",
];

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
            &[
                "build.rs",
                "src/artifact.rs",
                "src/build_support.rs",
                "src/inventory/mod.rs",
                "src/inventory/sources.rs",
                "src/native_build.rs",
                "src/platform.rs",
            ]
        );
        for path in super::BUILD_POLICY_INPUTS {
            assert!(
                std::path::Path::new(path).is_file(),
                "missing build policy input {path}"
            );
        }

        let listed = super::BUILD_POLICY_INPUTS
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        let entry = std::fs::read_to_string("build.rs").expect("read Cargo build entry point");
        assert!(
            entry.contains("include!(\"src/native_build.rs\")"),
            "build.rs must delegate to the owned native build module"
        );
        for module in ["artifact", "build_support", "inventory", "platform"] {
            let source = format!("mod {module};");
            assert!(
                std::fs::read_to_string("src/native_build.rs")
                    .expect("read native build module")
                    .contains(&source),
                "native build module no longer owns {module}"
            );
            let file = format!("src/{module}.rs");
            let directory = format!("src/{module}/mod.rs");
            let resolved = if std::path::Path::new(&file).is_file() {
                file
            } else {
                directory
            };
            assert!(
                listed.contains(resolved.as_str()),
                "build module {resolved} is absent from rerun inventory"
            );
        }
        assert!(
            listed.contains("src/inventory/sources.rs"),
            "nested inventory source discovery is absent from rerun inventory"
        );
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
    fn target_checkpoint_restore_discards_host_transient_cpu_state_on_both_isas() {
        let contracts = [
            (
                "aarch64",
                &[
                    "(c)->vdirty = 0",
                    "(c)->fault_addr = 0",
                    "(c)->bus_ea = 0",
                    "(c)->bus_filter = 0",
                    "(c)->bus_force = 0",
                    "G_SOFT_STATE_RESET(c)",
                ][..],
            ),
            (
                "x86_64",
                &[
                    "(c)->vdirty = 0",
                    "(c)->fault_addr = 0",
                    "(c)->bus_ea = 0",
                    "(c)->bus_filter = 0",
                    "(c)->bus_force = 0",
                    "G_SOFT_TLB_CLEAR(c)",
                ][..],
            ),
        ];
        for (target, required) in contracts {
            let path = format!("src/native/engine/target/{target}.c");
            let source = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path}: {error}"));
            let (_, sanitize) = source
                .split_once("#define G_CKPT_CPU_SANITIZE(c)")
                .unwrap_or_else(|| panic!("{path} has no checkpoint CPU sanitizer"));
            let (sanitize, _) = sanitize
                .split_once("} while (0)")
                .unwrap_or_else(|| panic!("{path} checkpoint CPU sanitizer is unterminated"));
            for statement in required {
                assert!(
                    sanitize.contains(statement),
                    "{path} restore keeps host-transient CPU state: {statement}"
                );
            }
        }
    }

    #[test]
    fn windows_link_inputs_use_target_spelling() {
        assert!(super::WINDOWS_SYSTEM_LIBRARIES.contains(&"ws2_32"));
        assert!(super::WINDOWS_SYSTEM_LIBRARIES.contains(&"ntdll"));
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
