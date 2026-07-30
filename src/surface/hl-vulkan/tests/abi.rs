//! ABI conformance gate for the guest Vulkan ICD shim cdylib (`shim/vulkan` -> `libvk_hl.so.1`).
//!
//! This:
//!   1. natively builds the guest cdylib for the host architecture (the build MUST succeed here),
//!   2. asserts the DISPATCH census — the manifest's 712 `vk*` command names plus the 3 hand-written
//!      `vk_icd*` loader hooks — equals the committed golden list exactly (no missing, no extra, 715),
//!      which is what guarantees no command silently disappears, and
//!   3. asserts the cdylib's *exported dynamic symbols* are ONLY the 3 `vk_icd*` loader hooks. The
//!      command surface must NOT be dynamically exported: see
//!      [`shim_exports_only_the_icd_hooks`].
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

/// Build the shim natively and return its Linux guest library.
fn built_shim(shim_target: &Path) -> PathBuf {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let crate_manifest = manifest_dir().join(SHIM_DIR).join("Cargo.toml");
    let vendor = manifest_dir().join("../../../vendor/rust/shim-deps");
    let triple = guest_triple();
    let linker =
        std::env::var("HL_AARCH64_LINUX_CC").unwrap_or_else(|_| "aarch64-linux-gnu-gcc".to_owned());
    let linker_env = format!(
        "CARGO_TARGET_{}_LINKER",
        triple.to_uppercase().replace('-', "_")
    );

    let status = Command::new(&cargo)
        .args([
            "build",
            "--release",
            "--offline",
            "--config",
            "source.crates-io.replace-with=\"vendored-sources\"",
            "--config",
        ])
        .arg(format!("source.vendored-sources.directory={vendor:?}"))
        .arg("--manifest-path")
        .arg(&crate_manifest)
        .args(["--target", triple, "--target-dir"])
        .arg(shim_target)
        .env("HL_VULKAN_BUILDING_SHIM", "1")
        .env(linker_env, linker)
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("CLIPPY_ARGS")
        .env_remove("NIX_LDFLAGS")
        .env_remove("NIX_CFLAGS_COMPILE")
        .status()
        .unwrap_or_else(|e| panic!("spawn cargo build for {SHIM_DIR}: {e}"));
    assert!(status.success(), "aarch64 build of {SHIM_DIR} must succeed");

    let so = shim_target.join(triple).join("release").join(SHIM_LIB);
    assert!(
        so.exists(),
        "expected built cdylib {} to exist",
        so.display()
    );
    so
}

fn exports(so: &Path) -> BTreeSet<String> {
    let out = Command::new("nm")
        .args(["-D", "--defined-only"])
        .arg(so)
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
fn shim_dispatch_census_matches_the_golden_abi() {
    let golden = read_golden(&manifest_dir().join(GOLDEN));
    assert_eq!(
        golden.len(),
        EXPECTED,
        "golden {GOLDEN} has an unexpected count"
    );

    // The generator's SOURCE: manifest command names + the 3 vk_icd* hooks == golden. This is the
    // census that must not drift; it is deliberately NOT the exported dynamic symbol set, because the
    // command surface is reached through `vk_icdGetInstanceProcAddr` -> `dispatch_addr`, which resolves
    // link-time addresses inside the cdylib rather than dynamic symbols.
    let surface = manifest_surface(&manifest_dir().join(MANIFEST));
    assert_eq!(
        surface, golden,
        "manifest+vk_icd names differ from the golden ABI surface"
    );
}

/// A Vulkan driver loaded into a process that also links the real loader must not define the loader's
/// own `vk*` symbols with default visibility: ELF preemption makes the driver's definition satisfy the
/// LOADER's internal references, the loader detects the resulting recursion
/// ("vkEnumerateInstanceExtensionProperties points to the loader, this would lead to infinite
/// recursion") and DISCARDS this driver's instance-extension list. The application then sees
/// `vkCreateInstance` fail with `VK_ERROR_EXTENSION_NOT_PRESENT` for `VK_KHR_surface` even though the
/// driver advertises it. `-Bsymbolic` does not prevent this: it binds only the driver's OWN references
/// locally, not its definitions' visibility to others.
///
/// Only the 3 loader-facing `vk_icd*` hooks may be exported; the loader needs nothing else.
#[test]
fn shim_exports_only_the_icd_hooks() {
    let shim_target = manifest_dir().join("target").join("shim-build");
    let exports = exports(&built_shim(&shim_target));
    let expected: BTreeSet<String> = VK_ICD_HOOKS.iter().map(|h| h.to_string()).collect();
    let leaked: Vec<_> = exports.difference(&expected).collect();
    assert!(
        leaked.is_empty(),
        "{} Vulkan command symbols are dynamically exported and will preempt the loader's own \
         definitions; only the vk_icd* hooks may be exported. First few: {:?}",
        leaked.len(),
        leaked.iter().take(5).collect::<Vec<_>>()
    );
    assert_eq!(
        exports, expected,
        "the exported surface must be exactly the 3 vk_icd* loader hooks"
    );
}

/// End-to-end proof of the loader contract, run against the real built cdylib: `dlopen` it into the
/// global scope exactly as the Vulkan loader does (NOT `RTLD_LOCAL`), then
///   * assert a bare `dlsym("vkEnumerateInstanceExtensionProperties")` finds NOTHING, so the driver can
///     never preempt the loader's own definition and trip its infinite-recursion guard, and
///   * assert the very same command, resolved the way the loader really resolves it — through
///     `vk_icdGetInstanceProcAddr` — still returns the driver's three advertised instance extensions.
///
/// Together these are the behaviour that was broken: with the commands exported, the loader discarded
/// this driver's extension list and applications saw `vkCreateInstance` fail with
/// `VK_ERROR_EXTENSION_NOT_PRESENT` for `VK_KHR_surface`. The probe is C because `dlopen` and calling a
/// C function pointer require `unsafe`, which workspace crates forbid.
///
/// Skipped when the host cannot execute the guest library (a non-Linux or mismatched-arch host).
#[test]
fn icd_hook_resolves_instance_extensions_while_bare_symbols_stay_hidden() {
    if !cfg!(target_os = "linux") {
        eprintln!("skipping: the guest cdylib is only loadable on a Linux host");
        return;
    }
    let shim_target = manifest_dir().join("target").join("shim-build");
    let so = built_shim(&shim_target);

    let out_dir = shim_target.join("loader-probe");
    std::fs::create_dir_all(&out_dir).expect("create probe dir");
    let source = out_dir.join("probe.c");
    std::fs::write(&source, LOADER_PROBE).expect("write probe source");
    let probe = out_dir.join("probe");
    let compiled = Command::new("cc")
        .arg("-o")
        .arg(&probe)
        .arg(&source)
        .arg("-ldl")
        .status();
    match compiled {
        Ok(status) if status.success() => {}
        _ => {
            eprintln!("skipping: no working C compiler to build the loader probe");
            return;
        }
    }

    let run = Command::new(&probe)
        .arg(&so)
        .output()
        .unwrap_or_else(|e| panic!("run loader probe: {e}"));
    let report = String::from_utf8_lossy(&run.stdout).into_owned();
    assert!(
        run.status.success(),
        "loader probe failed on {}:\n{report}{}",
        so.display(),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        report.contains("bare-dlsym=hidden"),
        "the command surface must not be dynamically exported, or it preempts the loader's own \
         definitions and its recursion guard discards this driver:\n{report}"
    );
    for extension in [
        "VK_KHR_surface",
        "VK_KHR_wayland_surface",
        "VK_KHR_get_physical_device_properties2",
    ] {
        assert!(
            report.contains(extension),
            "{extension} must survive resolution through vk_icdGetInstanceProcAddr:\n{report}"
        );
    }
}

/// Loads the guest ICD the way the Vulkan loader does and prints what the loader would observe.
const LOADER_PROBE: &str = r#"
#include <dlfcn.h>
#include <stdint.h>
#include <stdio.h>

typedef void (*pfn)(void);
typedef int32_t (*negotiate)(uint32_t *);
typedef pfn (*get_instance_proc_addr)(void *, const char *);
typedef int32_t (*enumerate)(const char *, uint32_t *, void *);

struct extension_properties {
    char name[256];
    uint32_t spec_version;
};

int main(int argc, char **argv) {
    if (argc < 2) { printf("usage: probe <library>\n"); return 1; }
    /* RTLD_GLOBAL, exactly as the loader loads a driver — this is what makes preemption possible. */
    void *library = dlopen(argv[1], RTLD_NOW | RTLD_GLOBAL);
    if (!library) { printf("dlopen failed: %s\n", dlerror()); return 1; }

    printf("bare-dlsym=%s\n",
           dlsym(library, "vkEnumerateInstanceExtensionProperties") ? "EXPORTED" : "hidden");

    negotiate agree = (negotiate)dlsym(library, "vk_icdNegotiateLoaderICDInterfaceVersion");
    if (!agree) { printf("vk_icdNegotiateLoaderICDInterfaceVersion missing\n"); return 2; }
    uint32_t version = 7;
    if (agree(&version) != 0) { printf("negotiation failed\n"); return 2; }
    printf("negotiated-interface=%u\n", version);

    get_instance_proc_addr resolve =
        (get_instance_proc_addr)dlsym(library, "vk_icdGetInstanceProcAddr");
    if (!resolve) { printf("vk_icdGetInstanceProcAddr missing\n"); return 2; }

    enumerate list = (enumerate)resolve(NULL, "vkEnumerateInstanceExtensionProperties");
    if (!list) { printf("vkEnumerateInstanceExtensionProperties did not resolve\n"); return 3; }

    uint32_t count = 0;
    if (list(NULL, &count, NULL) != 0) { printf("count query failed\n"); return 4; }
    printf("instance-extension-count=%u\n", count);
    if (count > 16) { printf("implausible count\n"); return 4; }

    struct extension_properties properties[16];
    uint32_t written = count;
    if (list(NULL, &written, properties) != 0) { printf("fill query failed\n"); return 4; }
    for (uint32_t i = 0; i < written; i++) {
        printf("instance-extension=%s spec=%u\n", properties[i].name, properties[i].spec_version);
    }
    return 0;
}
"#;

#[test]
fn shim_entry_points_bind_to_the_icd_not_the_loader() {
    let shim_target = manifest_dir().join("target").join("shim-build");
    let so = built_shim(&shim_target);
    let out = Command::new("readelf")
        .args(["-dW"])
        .arg(&so)
        .output()
        .unwrap_or_else(|e| panic!("run readelf on {}: {e}", so.display()));
    assert!(
        out.status.success(),
        "readelf -dW failed on {}",
        so.display()
    );
    let dynamic = String::from_utf8_lossy(&out.stdout);
    assert!(
        dynamic.lines().any(|line| {
            line.contains("(FLAGS)") && line.split_whitespace().any(|field| field == "SYMBOLIC")
        }),
        "{} must carry DF_SYMBOLIC so its proc-address table cannot resolve back into libvulkan",
        so.display()
    );
}
