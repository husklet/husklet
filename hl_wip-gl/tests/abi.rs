//! ABI conformance gate for the guest `libEGL.so.1` shim cdylib (the PRIMARY 402-symbol object).
//!
//! This:
//!   1. natively builds the aarch64 cdylib (the host arch — the build MUST succeed here), then
//!   2. `nm -D`s its exported dynamic symbols and asserts the API surface EQUALS the committed golden
//!      symbol list exactly (no missing, no extra) and the count matches (402), and
//!   3. cross-checks the generator's source: the shim's manifest names equal the same golden.
//!
//! The build shares the dedicated `target/shim-build` dir with `build.rs`, so after the crate's build
//! script has staged the shim this is a cache hit. It sets the `HL_GL_BUILDING_SHIM` recursion sentinel +
//! `--offline` exactly like `build.rs`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

const SHIM_DIR: &str = "shim/egl";
const SHIM_LIB: &str = "libhl_egl_guest.so";
const GOLDEN: &str = "shim/egl/tests/golden/abi_symbols.txt";
const MANIFEST: &str = "shim/egl/registry/gles2_egl.manifest";
const EXPECTED: usize = 402;

/// True if `sym` is part of the shim's advertised GLES2/EGL API surface (`gl*` or `egl*`).
fn is_api(s: &str) -> bool {
    s.starts_with("gl") || s.starts_with("egl")
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The host rust target triple (this host is aarch64-unknown-linux-gnu; the test runs Linux-side).
fn host_triple() -> &'static str {
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

/// Names in the shim manifest (col 2 of each non-comment `LIB<TAB>name<TAB>ret<TAB>params` row).
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
fn built_exports(shim_target: &Path) -> BTreeSet<String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let crate_manifest = manifest_dir().join(SHIM_DIR).join("Cargo.toml");
    let triple = host_triple();

    let status = Command::new(&cargo)
        .args(["build", "--release", "--offline", "--manifest-path"])
        .arg(&crate_manifest)
        .args(["--target", triple, "--target-dir"])
        .arg(shim_target)
        .env("HL_GL_BUILDING_SHIM", "1")
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .status()
        .unwrap_or_else(|e| panic!("spawn cargo build for {SHIM_DIR}: {e}"));
    assert!(status.success(), "aarch64 build of {SHIM_DIR} must succeed");

    let so = shim_target.join(triple).join("release").join(SHIM_LIB);
    assert!(so.exists(), "expected built cdylib {} to exist", so.display());

    let out = Command::new("nm")
        .args(["-D", "--defined-only"])
        .arg(&so)
        .output()
        .unwrap_or_else(|e| panic!("run nm on {}: {e}", so.display()));
    assert!(out.status.success(), "nm -D failed on {}", so.display());

    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_whitespace().nth(2)) // "<addr> T <name>"
        .filter(|s| is_api(s))
        .map(str::to_string)
        .collect()
}

#[test]
fn shim_export_surface_matches_the_golden_abi() {
    let shim_target = manifest_dir().join("target").join("shim-build");

    let golden = read_golden(&manifest_dir().join(GOLDEN));
    assert_eq!(golden.len(), EXPECTED, "golden {GOLDEN} has an unexpected count");

    // (a) the generator's SOURCE: manifest names == golden (so the generated surface can't drift).
    let manifest = manifest_names(&manifest_dir().join(MANIFEST));
    assert_eq!(manifest, golden, "{SHIM_DIR}: manifest names differ from the golden ABI surface");

    // (b) the BUILT cdylib's exported dynamic symbols == golden, exactly.
    let exports = built_exports(&shim_target);
    let missing: Vec<_> = golden.difference(&exports).collect();
    let extra: Vec<_> = exports.difference(&golden).collect();
    assert!(missing.is_empty(), "{SHIM_DIR}: golden symbols missing from the .so: {missing:?}");
    assert!(extra.is_empty(), "{SHIM_DIR}: .so exports symbols not in the golden: {extra:?}");
    assert_eq!(exports.len(), EXPECTED, "{SHIM_DIR}: exported symbol count drifted");
}
