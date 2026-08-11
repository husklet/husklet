use std::fs;
#[cfg(all(target_os = "linux", target_arch = "aarch64", feature = "c-execution"))]
use std::process::Command;

const RETIRED_PATHS: [&str; 5] = [
    "include/hl/config.h",
    "src/core/cli.c",
    "src/core/config.c",
    "src/core/launch.c",
    "src/core/target/run.c",
];

const RETIRED_CONFIG_TOKENS: [&str; 8] = [
    "hl_launch_config",
    "hl_launch_result",
    "HL_CONFIG_MAGIC",
    "HL_CONFIG_ABI",
    "HL_CONFIG_NETWORK_",
    "HL_CONFIG_SANDBOX",
    "HL_CONFIG_UNTRUSTED",
    "HL_LAUNCH_RESULT",
];

const RETAINED_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../native/c/engine/retained/");

const RETIRED_SYMBOLS: [&str; 5] = [
    "hl_engine_entry",
    "hl_cli_route_parse",
    "hl_run_config_file_with",
    "hl_native_engine_run",
    "hl_launch_config_validate",
];

#[test]
fn product_manifest_excludes_the_retired_standalone_path() {
    let manifest = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../native/c/engine/retained/COMPILED_TUS.tsv"
    ))
    .unwrap();
    assert!(
        manifest.contains("src/core/engine.c"),
        "manifest fixture lost the product engine"
    );
    for path in RETIRED_PATHS {
        assert!(
            !std::path::Path::new(RETAINED_ROOT).join(path).exists(),
            "retired standalone configuration path returned to the retained tree: {path}"
        );
        assert!(
            !manifest.contains(path),
            "retired standalone configuration path returned to product build: {path}"
        );
    }
    let closure = fs::read_to_string(format!("{RETAINED_ROOT}RUNTIME_SOURCES.manifest")).unwrap();
    for path in closure.lines().filter(|path| !path.is_empty()) {
        let source = fs::read_to_string(format!("{RETAINED_ROOT}{path}"))
            .unwrap_or_else(|error| panic!("read retained closure path {path}: {error}"));
        for token in RETIRED_CONFIG_TOKENS {
            assert!(
                !source.contains(token),
                "retired launch configuration token {token} returned in {path}"
            );
        }
    }
    let target = manifest
        .lines()
        .find(|line| line.starts_with("target_unity_direct\t"))
        .expect("target unity translation unit");
    assert!(
        target
            .split('\t')
            .nth(3)
            .unwrap_or_default()
            .split(';')
            .any(|value| value == "HL_ENGINE_NO_STANDALONE=1"),
        "target unity must compile out hl_engine_entry"
    );
}

#[test]
#[cfg(all(target_os = "linux", target_arch = "aarch64", feature = "c-execution"))]
fn linked_test_binary_excludes_retired_standalone_symbols() {
    // Referencing the library keeps its whole-archive native link directives in
    // this integration test, making the symbol assertion non-vacuous.
    drop(hl_engine::options::Options::default());
    let executable = std::env::current_exe().unwrap();
    let output = Command::new("nm").arg(&executable).output().expect("run nm");
    assert!(output.status.success(), "nm failed for {}", executable.display());
    let output = String::from_utf8(output.stdout).expect("nm output is UTF-8");
    let symbols = output
        .lines()
        .filter_map(|line| line.split_whitespace().last())
        .collect::<std::collections::HashSet<_>>();
    assert!(
        symbols.contains("hl_engine_run"),
        "retained engine symbols were not linked"
    );
    for symbol in RETIRED_SYMBOLS {
        assert!(
            !symbols.contains(symbol),
            "retired standalone symbol returned to linked product: {symbol}"
        );
    }
}
