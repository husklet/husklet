//! ABI freeze for the guest-visible `vk*`/`vk_icd*` ICD export surface.
//!
//! Recomputes the exported guest-symbol set from the SAME sources of truth `build.rs` consumes and
//! asserts it EXACTLY equals the checked-in golden (`tests/golden/abi_symbols.txt`). An add, removal,
//! rename, or reorder that isn't mirrored into the golden fails loudly with a diff — so the upcoming
//! rewrite cannot silently drop or rename an entry point.
//!
//! The exported surface has two parts:
//!   1. Every `C`-record command in `registry/vk_commands.manifest` becomes an exported
//!      `#[no_mangle] extern "C"` `vk*` symbol — hand-written ones (`IMPLEMENTED` in build.rs) via
//!      `src/`, the rest as generated default stubs. (`IMPLEMENTED` is a strict subset of the
//!      manifest's `C` records; verified no hand-written `vk*` export exists outside it.)
//!   2. The three loader-facing ICD-interface entry points, hand-written in `src/icd.rs`, that are
//!      NOT in the manifest (the loader resolves them by fixed name, not from vk.xml).
//!
//! `registry/vk_command_origins.manifest` and `registry/vk_core_commands.manifest` are metadata
//! sidecars (origin/version classification), NOT export sources, so they are not consulted here.

use std::collections::BTreeSet;

/// The exact bytes `build.rs` reads to emit the `vk*` command surface.
const MANIFEST: &str = include_str!("../registry/vk_commands.manifest");
/// The frozen golden surface.
const GOLDEN: &str = include_str!("golden/abi_symbols.txt");

/// The loader-facing ICD-interface exports, hand-written in `src/icd.rs` and absent from vk.xml /
/// the manifest. Frozen here as part of the ICD's public surface.
const ICD_INTERFACE_EXPORTS: &[&str] = &[
    "vk_icdGetInstanceProcAddr",
    "vk_icdGetPhysicalDeviceProcAddr",
    "vk_icdNegotiateLoaderICDInterfaceVersion",
];

/// The frozen total. Update deliberately (with the golden) only when intentionally changing the ABI.
const EXPECTED_COUNT: usize = 715;

/// Recompute the exported symbol set: manifest `C`-records (parsed identically to `build.rs` — records
/// are tab-split; skip blank / `#`-comment / `T`-type lines; a command is a `C` record, field 2 is the
/// name) unioned with the fixed `vk_icd*` ICD-interface exports.
fn exported_symbols() -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    for line in MANIFEST.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') || line.starts_with("T\t") {
            continue;
        }
        let mut f = line.split('\t');
        let rec = f.next().unwrap_or("");
        assert_eq!(rec, "C", "unknown manifest record type in line: {line:?}");
        let name = f.next().unwrap_or("");
        assert!(!name.is_empty(), "manifest line with empty name: {line:?}");
        assert!(
            set.insert(name.to_string()),
            "duplicate command in manifest: {name}"
        );
    }
    for &icd in ICD_INTERFACE_EXPORTS {
        assert!(
            set.insert(icd.to_string()),
            "ICD-interface export {icd} unexpectedly also present in the manifest"
        );
    }
    set
}

fn golden_symbols() -> BTreeSet<String> {
    GOLDEN
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

#[test]
fn abi_surface_matches_golden() {
    let got = exported_symbols();
    let want = golden_symbols();

    let removed: Vec<&String> = want.difference(&got).collect();
    let added: Vec<&String> = got.difference(&want).collect();

    assert!(
        removed.is_empty() && added.is_empty(),
        "hl-shim-vk ABI surface drift vs tests/golden/abi_symbols.txt:\n  \
         REMOVED/renamed ({}): {:?}\n  ADDED/unfrozen ({}): {:?}\n\
         If this change is intentional, regenerate the golden and bump EXPECTED_COUNT.",
        removed.len(),
        removed,
        added.len(),
        added,
    );
    assert_eq!(got, want, "hl-shim-vk ABI surface differs from golden");
}

#[test]
fn abi_symbol_count_is_frozen() {
    assert_eq!(
        exported_symbols().len(),
        EXPECTED_COUNT,
        "recomputed hl-shim-vk export count changed"
    );
    assert_eq!(
        golden_symbols().len(),
        EXPECTED_COUNT,
        "golden hl-shim-vk export count changed"
    );
}
