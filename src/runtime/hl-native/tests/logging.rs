use std::{fs, path::PathBuf, process::Command};

#[test]
fn disabled_log_tag_does_not_evaluate_format_arguments() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let scratch = std::env::temp_dir().join(format!("hl-native-log-disabled-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("create logging probe directory");
    let source = scratch.join("logging.c");
    let executable = scratch.join("logging");
    fs::write(
        &source,
        r#"
#include "hl/log.h"

int main(void) {
    hl_log_context context = {0};
    int evaluations = 0;
    HL_LOGF(&context, HL_LOG_TAG_SYSCALL, "value=%d", ++evaluations);
    return evaluations;
}
"#,
    )
    .expect("write logging probe source");

    let compile = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-DHL_ENABLE_LOGGING=1"])
        .arg(format!("-I{}", native.join("include").display()))
        .arg(&source)
        .arg(native.join("engine/log.c"))
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("logging probe compiler");
    assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));

    let run = Command::new(&executable).status().expect("logging probe execution");
    assert!(run.success(), "disabled logging evaluated a format argument: {run}");
    fs::remove_dir_all(scratch).expect("remove logging probe directory");
}
