use std::fs;
use std::path::{Path, PathBuf};
#[cfg(hl_retained_c)]
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

const RETAINED_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../runtime/native/retained/");

const RETIRED_SYMBOLS: [&str; 5] = [
    "hl_engine_entry",
    "hl_cli_route_parse",
    "hl_run_config_file_with",
    "hl_native_engine_run",
    "hl_launch_config_validate",
];

#[test]
fn product_build_does_not_depend_on_sibling_engines() {
    for (name, source) in [
        ("build.rs", include_str!("../build.rs")),
        ("Cargo.toml", include_str!("../Cargo.toml")),
    ] {
        for sibling in ["../engine", "../engine_rust"] {
            assert!(
                !source.contains(sibling),
                "{name} must not read or link the sibling checkout {sibling}"
            );
        }
    }
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("hl-engine lives below src/containers")
        .to_owned()
}

fn walk(root: &Path, directory: &Path, output: &mut Vec<PathBuf>) {
    let mut entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
        .collect::<std::io::Result<Vec<_>>>()
        .unwrap_or_else(|error| panic!("enumerate {}: {error}", directory.display()));
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, output);
        } else {
            output.push(path.strip_prefix(root).expect("repository path").to_owned());
        }
    }
}

fn production_inputs(root: &Path) -> Vec<PathBuf> {
    let mut candidates = vec![PathBuf::from("Cargo.toml"), PathBuf::from("flake.nix")];
    walk(root, &root.join("src"), &mut candidates);
    let workflows = root.join(".github");
    if workflows.is_dir() {
        walk(root, &workflows, &mut candidates);
    }
    candidates
        .into_iter()
        .filter(|path| {
            let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
            let extension = path
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or_default();
            name == "Cargo.toml"
                || name == "CMakeLists.txt"
                || name == "build.rs"
                || matches!(extension, "cmake" | "manifest" | "nix" | "sh" | "tsv" | "yaml" | "yml")
        })
        .collect()
}

fn sibling_dependency_violations(root: &Path) -> Vec<PathBuf> {
    production_inputs(root)
        .into_iter()
        .filter(|path| {
            let source =
                fs::read_to_string(root.join(path)).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            source.contains("../engine") || source.contains("../engine_rust")
        })
        .collect()
}

fn native_locality_violations(root: &Path) -> Vec<PathBuf> {
    let mut sources = Vec::new();
    walk(root, &root.join("src"), &mut sources);
    sources
        .into_iter()
        .filter(|path| {
            matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("c" | "h" | "s" | "S")
            ) && !path.starts_with("src/runtime/native")
        })
        .collect()
}

#[test]
fn every_production_input_is_independent_of_sibling_engines() {
    let root = repository_root();
    assert_eq!(
        sibling_dependency_violations(&root),
        Vec::<PathBuf>::new(),
        "production manifests, build scripts, packaging scripts, and workflows must be repository-local"
    );

    let makefile = fs::read_to_string(root.join("Makefile")).expect("read Makefile");
    for (line_number, line) in makefile.lines().enumerate() {
        assert!(
            !line.contains("../engine") && !line.contains("../engine_rust"),
            "Makefile:{} gives a target a sibling-engine dependency: {line}",
            line_number + 1
        );
        if line.contains("BENCH_C_BUILD") {
            assert!(
                line.starts_with("BENCH_C_BUILD ?=") || line.contains("test -n \"$(BENCH_C_BUILD)\""),
                "Makefile:{} exposes the optional C oracle outside its benchmark adapter: {line}",
                line_number + 1
            );
        }
    }
}

#[test]
fn repository_gates_keep_native_and_compatibility_coverage() {
    let root = repository_root();
    let makefile = fs::read_to_string(root.join("Makefile")).expect("read Makefile");
    assert!(
        makefile.contains("src/runtime/native/exec/test/memory_lifecycle.sh || status=1"),
        "the normal gate must aggregate the native memory lifecycle result"
    );
    for required in [
        ".PHONY: all check design-lint gate gate-app gate-compat gate-fixture",
        "gate-compat:",
        "cargo build --release -p engine -p testing --bins --locked --offline",
        "HL_TEST_ENGINE_APP_BIN_DIR=\"$(CURDIR)/target/release\"",
        "--backend-receipt",
        "\"backend\":\"retained-c\"",
        "mktemp -d \"$(CURDIR)/target/testing/runtime/gate.XXXXXX\"",
        "test ! -e \"$$results\"",
        "--baseline tests/runtime/baseline.tsv",
        "--engine-profile release",
    ] {
        assert!(
            makefile.contains(required),
            "compatibility gate lost required contract: {required}"
        );
    }
    let flake = fs::read_to_string(root.join("flake.nix")).expect("read flake");
    assert!(
        flake.contains("`make gate-app`"),
        "flake must name the real application gate"
    );
    assert!(
        !flake.contains("gate-gui"),
        "flake references the nonexistent gate-gui target"
    );
}

#[test]
fn production_native_sources_have_one_authoritative_root() {
    let root = repository_root();
    assert_eq!(
        native_locality_violations(&root),
        Vec::<PathBuf>::new(),
        "production C, headers, and assembly must live below src/runtime/native; tests/ remains the fixture boundary"
    );
}

#[test]
fn repository_boundary_checks_are_non_vacuous() {
    let fixture = std::env::temp_dir().join(format!(
        "husklet-repository-boundary-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    let _ = fs::remove_dir_all(&fixture);
    fs::create_dir_all(fixture.join("src/example")).expect("create fixture");
    fs::write(
        fixture.join("Cargo.toml"),
        "[dependencies]\nretired = { path = \"../engine\" }\n",
    )
    .expect("write forbidden manifest");
    fs::write(fixture.join("flake.nix"), "{}\n").expect("write flake fixture");
    fs::write(
        fixture.join("src/example/Cargo.toml"),
        "[package]\nname = \"example\"\n",
    )
    .expect("write package");
    fs::write(
        fixture.join("src/example/foreign.c"),
        "int foreign(void) { return 0; }\n",
    )
    .expect("write C fixture");

    assert_eq!(sibling_dependency_violations(&fixture), [PathBuf::from("Cargo.toml")]);
    assert_eq!(
        native_locality_violations(&fixture),
        [PathBuf::from("src/example/foreign.c")]
    );
    fs::remove_dir_all(&fixture).expect("remove fixture");
}

#[test]
fn retired_rust_differential_executor_stays_deleted() {
    assert!(
        !repository_root()
            .join("src/containers/hl-engine/src/native/executor_differential.rs")
            .exists(),
        "the retired Rust executor is neither production code nor a differential fixture"
    );
}

#[test]
fn product_manifest_excludes_the_retired_standalone_path() {
    let manifest = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtime/native/retained/COMPILED_TUS.tsv"
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
    for group in ["target_aarch64_direct", "target_x86_64_direct"] {
        let target = manifest
            .lines()
            .find(|line| line.starts_with(&format!("{group}\t")))
            .unwrap_or_else(|| panic!("missing {group} translation unit"));
        assert!(
            target
                .split('\t')
                .nth(3)
                .unwrap_or_default()
                .split(';')
                .any(|value| value == "HL_ENGINE_NO_STANDALONE=1"),
            "{group} must compile out hl_engine_entry"
        );
    }
}

#[test]
#[cfg(hl_retained_c)]
fn linked_test_binary_excludes_retired_standalone_symbols() {
    // A direct native reference keeps the retained whole-archive directives in
    // this integration test. A Rust-only hl-engine type is not a link anchor.
    std::hint::black_box(hl_engine::retained_c_link_anchor());
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
