use std::fs;
#[cfg(hl_retained_c)]
use std::process::Command;

const RETAINED_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../runtime/native/");
const RETIRED: &str = "src/core/environment.h";

#[test]
fn retained_environment_boundary_is_physically_absent() {
    let source_manifest = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtime/native/RUNTIME_SOURCES.manifest"
    ))
    .unwrap();
    let options = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtime/native/src/core/options.c"
    ))
    .unwrap();

    assert!(source_manifest.contains("src/core/options.c"));
    assert!(
        !std::path::Path::new(RETAINED_ROOT).join(RETIRED).exists(),
        "retired ambient environment boundary returned"
    );
    assert!(
        !source_manifest.contains(RETIRED),
        "retired ambient environment boundary returned to the source manifest"
    );
    assert!(
        !options.contains("#include \"environment.h\""),
        "retired ambient environment header returned to retained options"
    );
}

#[test]
#[cfg(hl_retained_c)]
fn retained_environment_symbols_are_not_linked() {
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
    for retired in ["hl_environment_debug_log", "hl_environment_take_activation_descriptor"] {
        assert!(
            !symbols.contains(retired),
            "retired environment symbol returned: {retired}"
        );
    }
}
