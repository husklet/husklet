//! ABI conformance gate for the guest Vulkan ICD shim cdylib (`shim/vulkan` -> `libvk_hl.so.1`).
//!
//! This:
//!   1. natively builds the aarch64 cdylib (the host arch — the build MUST succeed here), then
//!   2. `nm -D`s its exported dynamic symbols and asserts the API surface EQUALS the committed golden
//!      symbol list exactly (no missing, no extra) and the count matches (715), and
//!   3. cross-checks the generator's source: the shim manifest's 712 `vk*` command names plus the 3
//!      hand-written `vk_icd*` loader hooks equal the same golden.
//!
//! The build shares the dedicated `target/shim-build` dir with `build.rs`, so after the crate's build
//! script has staged the shim this is a cache hit. It sets the `HL_VULKAN_BUILDING_SHIM` recursion
//! sentinel + `--offline` exactly like `build.rs`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

const SHIM_DIR: &str = "shim/vulkan";
const SHIM_LIB: &str = "libvk_hl_guest.so";
const GOLDEN: &str = "shim/vulkan/tests/golden/abi_symbols.txt";
const MANIFEST: &str = "shim/vulkan/registry/vk_commands.manifest";
const EXPECTED: usize = 715;

/// The 3 loader-facing `vk_icd*` hooks — hand-written in `icd.rs`, NOT in the command manifest.
const VK_ICD_HOOKS: &[&str] = &[
    "vk_icdGetInstanceProcAddr",
    "vk_icdGetPhysicalDeviceProcAddr",
    "vk_icdNegotiateLoaderICDInterfaceVersion",
];

/// A shim-exported symbol part of the advertised API surface: every `vk*` name (both `vkFoo` commands
/// and the `vk_icd*` loader hooks).
fn is_api(s: &str) -> bool {
    s.starts_with("vk")
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

/// Names in the shim manifest (col 2 of each `C<TAB>name<TAB>ret<TAB>params` row) plus the 3 `vk_icd*`.
fn manifest_surface(path: &Path) -> BTreeSet<String> {
    let mut set: BTreeSet<String> = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read manifest {}: {e}", path.display()))
        .lines()
        .filter(|l| l.starts_with("C\t"))
        .filter_map(|l| l.split('\t').nth(1))
        .map(str::to_string)
        .collect();
    for h in VK_ICD_HOOKS {
        set.insert(h.to_string());
    }
    set
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
        .env("HL_VULKAN_BUILDING_SHIM", "1")
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
    let golden = read_golden(&manifest_dir().join(GOLDEN));
    assert_eq!(golden.len(), EXPECTED, "golden {GOLDEN} has an unexpected count");

    // (a) the generator's SOURCE: manifest command names + the 3 vk_icd* hooks == golden.
    let surface = manifest_surface(&manifest_dir().join(MANIFEST));
    assert_eq!(surface, golden, "manifest+vk_icd names differ from the golden ABI surface");

    // (b) the BUILT cdylib's exported dynamic symbols == golden, exactly.
    let shim_target = manifest_dir().join("target").join("shim-build");
    let exports = built_exports(&shim_target);
    let missing: Vec<_> = golden.difference(&exports).collect();
    let extra: Vec<_> = exports.difference(&golden).collect();
    assert!(missing.is_empty(), "golden symbols missing from the .so: {missing:?}");
    assert!(extra.is_empty(), ".so exports symbols not in the golden: {extra:?}");
    assert_eq!(exports.len(), EXPECTED, "exported symbol count drifted");
}
