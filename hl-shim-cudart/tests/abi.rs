//! ABI freeze for the guest-visible `cuda*`/`__cuda*` export surface.
//!
//! Recomputes the exported guest-symbol set from the SAME source of truth `build.rs` consumes
//! (`registry/cudart.manifest`) and asserts it EXACTLY equals the checked-in golden
//! (`tests/golden/abi_symbols.txt`). An add, removal, rename, or reorder that isn't mirrored into the
//! golden fails loudly with a diff.
//!
//! Why the manifest is authoritative: every non-comment manifest line becomes an exported
//! `#[no_mangle] extern "C"` symbol. The whole runtime surface is hand-implemented (`IMPLEMENTED` in
//! build.rs) and that set is a strict subset of the manifest, so the exported surface is exactly the
//! set of manifest names. (Verified: no hand-written `cuda*`/`__cuda*` export exists outside the
//! manifest; the extra `IMPLEMENTED`-adjacent names in build.rs are C type names, not entry points.)

use std::collections::BTreeSet;

/// The exact bytes `build.rs` reads to emit the `cuda*`/`__cuda*` surface.
const MANIFEST: &str = include_str!("../registry/cudart.manifest");
/// The frozen golden surface.
const GOLDEN: &str = include_str!("golden/abi_symbols.txt");

/// The frozen total. Update deliberately (with the golden) only when intentionally changing the ABI.
const EXPECTED_COUNT: usize = 49;

/// Recompute the exported symbol set from the manifest, parsing it identically to `build.rs`
/// (tab-split; skip blank / `#`-comment lines; field 2 is the entry-point name).
fn exported_symbols() -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    for line in MANIFEST.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut f = line.split('\t');
        let _lib = f.next().unwrap_or("");
        let name = f.next().unwrap_or("");
        assert!(!name.is_empty(), "manifest line with empty name: {line:?}");
        assert!(
            set.insert(name.to_string()),
            "duplicate entry point in manifest: {name}"
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
        "hl-shim-cudart ABI surface drift vs tests/golden/abi_symbols.txt:\n  \
         REMOVED/renamed ({}): {:?}\n  ADDED/unfrozen ({}): {:?}\n\
         If this change is intentional, regenerate the golden and bump EXPECTED_COUNT.",
        removed.len(),
        removed,
        added.len(),
        added,
    );
    assert_eq!(got, want, "hl-shim-cudart ABI surface differs from golden");
}

#[test]
fn abi_symbol_count_is_frozen() {
    assert_eq!(
        exported_symbols().len(),
        EXPECTED_COUNT,
        "recomputed hl-shim-cudart export count changed"
    );
    assert_eq!(
        golden_symbols().len(),
        EXPECTED_COUNT,
        "golden hl-shim-cudart export count changed"
    );
}
