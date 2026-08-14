use std::{fs, path::PathBuf, process::Command};

const MACRO_CONTRACT_PROBE: &str = r#"
#include "hl/log.h"
#include <string.h>

static hl_log_context log_context;
static unsigned context_evaluations;
static unsigned tag_evaluations;
static unsigned value_evaluations;
static unsigned emissions;
static unsigned char captured[128];
static size_t captured_size;

static hl_log_context *evaluated_context(void) {
    ++context_evaluations;
    return &log_context;
}

static uint32_t evaluated_tag(void) {
    ++tag_evaluations;
    return HL_LOG_TAG_SYSCALL;
}

static int evaluated_value(int left, int right) {
    ++value_evaluations;
    return left + right;
}

static void emit(void *context, uint32_t tag, const char *message, size_t size) {
    (void)context;
    if (tag != HL_LOG_TAG_SYSCALL || size > sizeof captured) return;
    memcpy(captured, message, size);
    captured_size = size;
    ++emissions;
}

int main(void) {
    hl_host_log_services logs;
    hl_host_services host;
    memset(&logs, 0, sizeof logs);
    memset(&host, 0, sizeof host);
    logs.abi = HL_HOST_LOG_ABI;
    logs.size = sizeof logs;
    logs.emit = emit;
    host.abi = HL_HOST_SERVICES_ABI;
    host.size = sizeof host;
    host.capabilities = HL_HOST_CAP_LOG;
    host.log = &logs;
    if (hl_log_context_init(&log_context, &host, "syscall") != HL_STATUS_OK) return 1;

    HL_LOG(evaluated_context(), evaluated_tag(), "left\0right");
    static const unsigned char literal_expected[] = "[hl:syscall] left\0right\n";
    if (context_evaluations != 1 || tag_evaluations != 1 || emissions != 1) return 2;
    if (captured_size != sizeof literal_expected - 1u ||
        memcmp(captured, literal_expected, sizeof literal_expected - 1u) != 0) return 3;

    log_context.enabled_tags = 0;
    HL_LOGF(evaluated_context(), evaluated_tag(), "sum=%d", evaluated_value(1, 2));
    if (context_evaluations != 2 || tag_evaluations != 2 || value_evaluations != 0 || emissions != 1) return 4;

    log_context.enabled_tags = HL_LOG_TAG_SYSCALL;
    HL_LOGF(evaluated_context(), evaluated_tag(), "sum=%d", evaluated_value(1, 2));
    static const unsigned char format_expected[] = "[hl:syscall] sum=3\n";
    if (context_evaluations != 3 || tag_evaluations != 3 || value_evaluations != 1 || emissions != 2) return 5;
    if (captured_size != sizeof format_expected - 1u ||
        memcmp(captured, format_expected, sizeof format_expected - 1u) != 0) return 6;
    return 0;
}
"#;

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

#[test]
fn logging_macros_are_single_evaluation_and_literal_extent_safe_in_c_and_cpp() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let scratch = std::env::temp_dir().join(format!("hl-native-log-contract-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("create logging macro probe directory");
    let log_object = scratch.join("log.o");
    let compile_log = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-DHL_ENABLE_LOGGING=1"])
        .arg(format!("-I{}", native.join("include").display()))
        .args(["-c"])
        .arg(native.join("engine/log.c"))
        .arg("-o")
        .arg(&log_object)
        .output()
        .expect("logging implementation compiler");
    assert!(
        compile_log.status.success(),
        "{}",
        String::from_utf8_lossy(&compile_log.stderr)
    );

    for (compiler, standard, extension) in [("CC", "c11", "c"), ("CXX", "c++17", "cc")] {
        let source = scratch.join(format!("logging.{extension}"));
        let executable = scratch.join(format!("logging-{extension}"));
        fs::write(&source, MACRO_CONTRACT_PROBE).expect("write logging macro probe source");
        let compile = Command::new(
            std::env::var_os(compiler).unwrap_or_else(|| if compiler == "CC" { "cc".into() } else { "c++".into() }),
        )
        .args([
            format!("-std={standard}"),
            "-Wall".into(),
            "-Wextra".into(),
            "-Werror".into(),
        ])
        .arg("-DHL_ENABLE_LOGGING=1")
        .arg(format!("-I{}", native.join("include").display()))
        .arg(&source)
        .arg(&log_object)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("logging macro probe compiler");
        assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));
        let run = Command::new(&executable)
            .status()
            .expect("logging macro probe execution");
        assert!(run.success(), "{extension} logging macro probe failed with {run}");
    }
    fs::remove_dir_all(scratch).expect("remove logging macro probe directory");
}
