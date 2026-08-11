use std::fs;
#[cfg(all(target_os = "linux", target_arch = "aarch64", feature = "c-execution"))]
use std::process::Command;

const RETAINED_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../native/c/engine/retained/");
const RETIRED: [&str; 2] = ["src/translator/window.c", "src/translator/window.h"];

#[test]
fn retained_window_helper_is_physically_absent() {
    let manifest = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../native/c/engine/retained/COMPILED_TUS.tsv"
    ))
    .unwrap();
    assert!(manifest.contains("src/translator/guest/aarch64/signal.c"));
    for source in RETIRED {
        assert!(
            !std::path::Path::new(RETAINED_ROOT).join(source).exists(),
            "retired generic window helper returned: {source}"
        );
        assert!(
            !manifest.contains(source),
            "retired generic window helper returned to product build: {source}"
        );
    }
}

#[test]
#[cfg(all(target_os = "linux", target_arch = "aarch64", feature = "c-execution"))]
fn retained_window_symbol_is_not_linked() {
    drop(hl_engine::options::Options::default());
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
        !symbols.contains("hl_window_contains"),
        "retired generic window symbol returned"
    );
}
