//! ABI conformance gate for the three guest shim cdylibs.
//!
//! For each shim (`cuda` -> libcuda.so.1, `cudart` -> libcudart.so.1, `nvml` -> libnvidia-ml.so.1) this:
//!   1. natively builds the aarch64 cdylib (the host arch — the build MUST succeed here), then
//!   2. `nm -D`s its exported dynamic symbols and asserts the API surface EQUALS the committed golden
//!      symbol list exactly (no missing, no extra) and the count matches (145 / 62 / 62), and
//!   3. cross-checks the generator's source: the shim's manifest names equal the same golden.
//!
//! The build shares the dedicated `target/shim-build` dir with `build.rs`, so after the crate's build
//! script has staged the shims this is a cache hit. It sets the `HL_CUDA_BUILDING_SHIM` recursion
//! sentinel + `--offline` exactly like `build.rs`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Shim {
    dir: &'static str,
    lib: &'static str,
    golden: &'static str,
    manifest: &'static str,
    expected: usize,
    /// True if `sym` is part of this shim's advertised API surface.
    is_api: fn(&str) -> bool,
}

fn cuda_api(s: &str) -> bool {
    // CUDA Driver API: cuXxx (cu + uppercase). Excludes the runtime's lowercase `cuda*`.
    s.starts_with("cu") && !s.starts_with("cuda")
}
fn cudart_api(s: &str) -> bool {
    s.starts_with("cuda") || s.starts_with("__cuda")
}
fn nvml_api(s: &str) -> bool {
    s.starts_with("nvml")
}

fn shims() -> Vec<Shim> {
    vec![
        Shim {
            dir: "shim/cuda",
            lib: "libhl_cuda_guest.so",
            golden: "shim/cuda/tests/golden/abi_symbols.txt",
            manifest: "shim/cuda/registry/cuda_driver.manifest",
            expected: 145,
            is_api: cuda_api,
        },
        Shim {
            dir: "shim/cudart",
            lib: "libhl_cudart_guest.so",
            golden: "shim/cudart/tests/golden/abi_symbols.txt",
            manifest: "shim/cudart/registry/cudart.manifest",
            expected: 62,
            is_api: cudart_api,
        },
        Shim {
            dir: "shim/nvml",
            lib: "libhl_nvml_guest.so",
            golden: "shim/nvml/tests/golden/abi_symbols.txt",
            manifest: "shim/nvml/registry/nvml.manifest",
            expected: 62,
            is_api: nvml_api,
        },
    ]
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Linux guest target matching the current host architecture.
fn guest_triple() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "aarch64-unknown-linux-gnu",
        "x86_64" => "x86_64-unknown-linux-gnu",
        other => panic!("unsupported host arch for the ABI test: {other}"),
    }
}

fn read_golden(path: &Path) -> BTreeSet<String> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read golden {}: {e}", path.display()))
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// Names in a shim manifest (col 2 of each non-comment `LIB<TAB>name<TAB>ret<TAB>params` row).
fn manifest_names(path: &Path) -> BTreeSet<String> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read manifest {}: {e}", path.display()))
        .lines()
        .filter(|l| !l.trim_start().starts_with('#') && !l.trim().is_empty())
        .filter_map(|l| l.split('\t').nth(1))
        .map(str::to_string)
        .collect()
}

/// Build the shim natively and return the exported dynamic symbols matching its API filter.
fn built_exports(shim: &Shim, shim_target: &Path) -> BTreeSet<String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let crate_manifest = manifest_dir().join(shim.dir).join("Cargo.toml");
    let triple = guest_triple();
    let linker =
        std::env::var("HL_AARCH64_LINUX_CC").unwrap_or_else(|_| "aarch64-linux-gnu-gcc".to_owned());
    let linker_env = format!(
        "CARGO_TARGET_{}_LINKER",
        triple.to_uppercase().replace('-', "_")
    );

    let status = Command::new(&cargo)
        .args(["build", "--release", "--offline", "--manifest-path"])
        .arg(&crate_manifest)
        .args(["--target", triple, "--target-dir"])
        .arg(shim_target)
        .env("HL_CUDA_BUILDING_SHIM", "1")
        .env(linker_env, linker)
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("CLIPPY_ARGS")
        .env_remove("NIX_LDFLAGS")
        .env_remove("NIX_CFLAGS_COMPILE")
        .status()
        .unwrap_or_else(|e| panic!("spawn cargo build for {}: {e}", shim.dir));
    assert!(
        status.success(),
        "aarch64 build of {} must succeed",
        shim.dir
    );

    let so = shim_target.join(triple).join("release").join(shim.lib);
    assert!(
        so.exists(),
        "expected built cdylib {} to exist",
        so.display()
    );

    let out = Command::new("nm")
        .args(["-D", "--defined-only"])
        .arg(&so)
        .output()
        .unwrap_or_else(|e| panic!("run nm on {}: {e}", so.display()));
    assert!(out.status.success(), "nm -D failed on {}", so.display());

    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_whitespace().nth(2)) // "<addr> T <name>"
        .filter(|s| (shim.is_api)(s))
        .map(str::to_string)
        .collect()
}

#[test]
fn shim_export_surfaces_match_the_golden_abi() {
    let shim_target = manifest_dir().join("target").join("shim-build");
    for shim in shims() {
        let golden = read_golden(&manifest_dir().join(shim.golden));
        assert_eq!(
            golden.len(),
            shim.expected,
            "golden {} has an unexpected count",
            shim.golden
        );

        // (a) the generator's SOURCE: manifest names == golden (so the generated surface can't drift).
        let manifest = manifest_names(&manifest_dir().join(shim.manifest));
        assert_eq!(
            manifest, golden,
            "{}: manifest names differ from the golden ABI surface",
            shim.dir
        );

        // (b) the BUILT cdylib's exported dynamic symbols == golden, exactly.
        let exports = built_exports(&shim, &shim_target);
        let missing: Vec<_> = golden.difference(&exports).collect();
        let extra: Vec<_> = exports.difference(&golden).collect();
        assert!(
            missing.is_empty(),
            "{}: golden symbols missing from the .so: {missing:?}",
            shim.dir
        );
        assert!(
            extra.is_empty(),
            "{}: .so exports symbols not in the golden: {extra:?}",
            shim.dir
        );
        assert_eq!(
            exports.len(),
            shim.expected,
            "{}: exported symbol count drifted",
            shim.dir
        );
    }
}
