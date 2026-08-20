//! An armed failure injection belongs to the launch that asked for it and to nothing derived
//! from it.
//!
//! `hl_options_clone` is the one place a launch's whole option store is copied, and both derived
//! stores go through it: a nested engine created with no explicit options
//! (`hl_options_clone_current`) and the environment an exec carries forward
//! (`hl_exec_environment_prepare`). Copying an injection there arms a run that never asked, which
//! makes every checkpoint result taken afterwards unattributable. The `HL_OPTION_TEST_INJECTION`
//! ownership class is what stops it, and this probe observes the behaviour at runtime rather than
//! reading the table -- `hl_option_definitions` is `static`, so neither `nm` nor `strings` can
//! answer whether the build actually has it.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn cloning_a_launch_store_drops_its_injections_and_keeps_everything_else() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let scratch = std::env::temp_dir().join(format!("hl-native-injection-scope-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("injection scope probe directory");
    let source = scratch.join("probe.c");
    let executable = scratch.join("probe");
    std::fs::write(
        &source,
        r#"#include "engine/options.h"

#include <stdio.h>
#include <string.h>

int main(void) {
    hl_options armed;
    hl_options derived;
    if (hl_options_init(&armed) != 0) return 1;
    if (hl_options_set(&armed, "HL_UID", "1000", 1) != 0) return 2;
    if (hl_options_set(&armed, "HL_CKPT_TEST_FAIL_AFTER_FORK", "1", 1) != 0) return 3;
    if (hl_options_set(&armed, "HL_CKPT_TEST_FAIL_PIDMAP_AT", "7", 1) != 0) return 4;
    /* Arming works: the launch that asked reads its own store. */
    if (hl_options_get(&armed, "HL_CKPT_TEST_FAIL_AFTER_FORK") == NULL) return 5;
    if (hl_options_get(&armed, "HL_CKPT_TEST_FAIL_PIDMAP_AT") == NULL) return 6;

    if (hl_options_clone(&derived, &armed) != 0) return 7;
    if (hl_options_get(&derived, "HL_CKPT_TEST_FAIL_AFTER_FORK") != NULL) return 8;
    if (hl_options_get(&derived, "HL_CKPT_TEST_FAIL_PIDMAP_AT") != NULL) return 9;
    /* Everything that is not an injection is still inherited, which is what the clone is for. */
    if (hl_options_get(&derived, "HL_UID") == NULL) return 10;
    if (strcmp(hl_options_get(&derived, "HL_UID"), "1000") != 0) return 11;
    if (hl_options_validate(&derived) != 0) return 12;

    /* The process-bound path a nested engine takes when it is given no options of its own. */
    hl_options inherited;
    (void)hl_options_bind_process(&armed);
    if (hl_options_clone_current(&inherited) != 0) return 13;
    if (hl_options_get(&inherited, "HL_CKPT_TEST_FAIL_AFTER_FORK") != NULL) return 14;
    if (hl_options_get(&inherited, "HL_UID") == NULL) return 15;
    (void)hl_options_bind_process(NULL);

    hl_options_destroy(&inherited);
    hl_options_destroy(&derived);
    hl_options_destroy(&armed);
    return 0;
}
"#,
    )
    .expect("injection scope probe source");
    let compile = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=c11", "-D_GNU_SOURCE"])
        .arg(format!("-I{}", native.display()))
        .arg(format!("-I{}", native.join("include").display()))
        .arg(&source)
        .arg(native.join("engine/options.c"))
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("injection scope probe compiler");
    assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));
    let run = Command::new(&executable)
        .status()
        .expect("injection scope probe execution");
    assert!(
        run.success(),
        "injection scope probe failed with {run}; the exit code is the numbered check in probe.c"
    );
    std::fs::remove_dir_all(scratch).expect("remove injection scope probe directory");
}
