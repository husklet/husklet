//! The build-freshness gate: this executable must carry the C sources now in the tree.
//!
//! A lane spent an afternoon measuring a checkpoint fix that had never run. The reasoning that
//! misled it was that the Rust test binary's sha256 was byte-identical before and after the C
//! edit — which on Linux it always is, because the engine is dlopened and not linked in. Two
//! checks it did make were not enough either: the static archive contained the new symbol, and
//! `strings` on the shared object would have found it, yet neither says whether the *process*
//! loaded that shared object.
//!
//! The only sound statement is the one this file makes: recompute the fingerprint from the C
//! sources on disk and compare it to the value Cargo baked into this binary. They differ
//! exactly when the running executable predates the tree it is being credited with testing.
//! The companion check lives in the loader, which refuses a shared object whose own compiled-in
//! fingerprint disagrees with this same value.

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/inventory/fingerprint.rs"));

#[test]
fn this_executable_was_built_from_the_native_sources_now_in_the_tree() {
    // Anchored on the package rather than the working directory: this test is meant to be run
    // straight off a saved binary as well as through `cargo test`, and the two do not share a cwd.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/native");
    let current = native_fingerprint(&root);
    let compiled = env!("HL_NATIVE_BUILD_FINGERPRINT");
    assert_eq!(
        current, compiled,
        "this test binary was built from native C sources fingerprinted {compiled}, but the tree \
         now holds {current}. Every C-level observation from this binary describes the earlier \
         sources. Rebuild before trusting any measurement taken from it."
    );
}

#[test]
fn the_loaded_engine_reports_the_same_fingerprint_as_this_executable() {
    // Loading is what proves the point: the archive and the file on disk can both be current
    // while the process holds an older mapping. `engine_version` forces the load and the
    // loader's own fingerprint check, which fails the load loudly on a mismatch.
    assert!(
        hl_native::artifact_load_error().is_none(),
        "the native engine did not load: {}",
        hl_native::artifact_load_error().expect("checked above")
    );
    assert!(
        hl_native::artifact_smoke(),
        "the loaded engine must answer its own metadata"
    );
}
