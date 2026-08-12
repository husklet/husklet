use std::fs;
#[cfg(hl_retained_c)]
use std::process::Command;

const RETAINED_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../runtime/hl-native/");
const WINDOW: [&str; 2] = ["src/translator/window.c", "src/translator/window.h"];

#[test]
fn retained_window_helper_is_owned_and_compiled_for_x86_translation() {
    let manifest = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtime/hl-native/COMPILED_TUS.tsv"
    ))
    .unwrap();
    let source_manifest = fs::read_to_string(format!("{RETAINED_ROOT}RUNTIME_SOURCES.manifest")).unwrap();
    assert!(manifest.contains("src/translator/guest/aarch64/signal.c"));
    for source in WINDOW {
        assert!(
            std::path::Path::new(RETAINED_ROOT).join(source).exists(),
            "x86 translation window helper is missing: {source}"
        );
    }
    assert!(
        manifest.contains("src/translator/window.c"),
        "x86 translation window helper is absent from the product build"
    );
    assert!(
        fs::read_to_string(format!("{RETAINED_ROOT}src/translator/guest/x86_64/cache.c"))
            .unwrap()
            .contains("hl_window_contains"),
        "x86 translation no longer consumes the checked window helper"
    );
    for source in WINDOW {
        assert!(
            source_manifest.contains(source),
            "x86 translation window helper is absent from the source inventory: {source}"
        );
    }
}

#[test]
#[cfg(hl_retained_c)]
fn retained_window_symbol_is_linked_with_the_x86_backend() {
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
    assert!(
        symbols.contains("hl_window_contains"),
        "x86 translation window symbol was not linked"
    );
}
