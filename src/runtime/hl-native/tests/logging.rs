use std::{fs, path::PathBuf, process::Command};

#[test]
fn sampling_exit_flush_joins_translation_serialization() {
    let native = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/native");
    let source = fs::read_to_string(native.join("translator/guest/x86_64/translit.inc")).expect("read transliterator");
    let start = source.find("static void translit_perf_map_flush_at_exit(void)").expect("exit flush");
    let body = &source[start..source[start..].find("\n}").map_or(source.len(), |end| start + end)];
    let lock = body.find("pthread_mutex_lock(&g_jit_lock)").expect("translation lock");
    let flush = body.find("translit_perf_map_flush()").expect("sampling flush");
    let unlock = body.find("pthread_mutex_unlock(&g_jit_lock)").expect("translation unlock");
    assert!(lock < flush && flush < unlock, "exit flush is outside translation serialization: {body}");
}

#[test]
fn x86_restore_snapshots_translation_profile_options_before_its_early_return() {
    let native = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/native");
    let source = fs::read_to_string(native.join("engine/target/x86_64.c")).expect("read x86 target");
    let start = source.find("int hl_run_linux_guest(").expect("x86 guest launch");
    let body = &source[start..];
    let diagnostics = body.find("g_prof = hl_option_get(\"HL_C_DIAGNOSTICS\")").expect("diagnostic snapshot");
    let profiling = body.find("translit_profile_options_refresh();").expect("profile option snapshot");
    let restore = body.find("const char *rdir = hl_option_get(\"HL_RESTORE\")").expect("restore branch");
    assert!(
        diagnostics < profiling && profiling < restore,
        "restore can enter translated execution before its profiling options are snapshotted"
    );
}

#[test]
fn x86_dispatch_bookkeeping_is_diagnostic_only() {
    let native = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/native");
    let target = fs::read_to_string(native.join("engine/target/x86_64.c")).expect("read x86 target");
    assert!(
        target.contains("g_dispatch_diagnostics = g_prof;"),
        "x86 target does not bind profiling to dispatcher bookkeeping"
    );
    for relative in [
        "translator/guest/x86_64/dispatch.h",
        "translator/guest/x86_64/interp_dispatch.h",
    ] {
        let source = fs::read_to_string(native.join(relative)).expect("read x86 dispatcher");
        let start = source
            .find("#define G_DISPATCH_DEBUG")
            .expect("x86 dispatcher debug hook");
        let end = source[start..].find("\n\n").map_or(source.len(), |end| start + end);
        let body = &source[start..end];
        let gate = body
            .find("if (g_dispatch_diagnostics)")
            .expect("diagnostic bookkeeping gate");
        for write in ["g_prevpc =", "g_curpc =", "g_disp_n++"] {
            let position = body
                .find(write)
                .unwrap_or_else(|| panic!("{relative} is missing {write}"));
            assert!(
                position > gate,
                "{relative} performs {write} before its diagnostic gate"
            );
        }
    }
}

#[test]
fn product_dispatch_return_census_is_complete_and_bound_to_map_misses() {
    let native = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/native");
    let backend = fs::read_to_string(native.join("engine/backend_tree.c")).expect("backend census source");
    let product_backend = backend
        .split_once("#else\n\n/* Hook-disabled translit_shape_exit")
        .map(|(_, product)| product)
        .expect("product backend census section");
    let dispatch = fs::read_to_string(native.join("engine/dispatch.c")).expect("dispatcher source");
    for field in [
        "dispatch_translation_miss",
        "dispatch_translated_return_total",
        "dispatch_translated_return_mismatch",
        "dispatch_interpreted_return_total",
        "dispatch_interpreted_return_mismatch",
        "t_fallthrough",
        "t_jcc_taken",
        "t_jcc_fall",
        "t_direct_jmp",
        "t_direct_call",
        "t_ret",
        "t_jmp_reg",
        "t_jmp_mem",
        "t_call_reg",
        "t_call_mem",
        "t_syscall",
        "t_irq",
        "t_fault",
        "t_other",
        "fall_total",
        "fall_mismatch",
        "fall_cap",
        "fall_decode",
        "fall_normal_to_sse2",
        "fall_sse2_to_normal",
        "fall_normal_to_fs",
        "fall_fs_to_normal",
        "fall_sse2_to_fs",
        "fall_fs_to_sse2",
        "fall_tl_no",
        "fall_displaced",
        "fall_fetch",
        "fall_riprel",
        "fall_fs_transaction",
        "fall_sse_riprel",
        "fall_other",
    ] {
        assert!(product_backend.contains(field), "product dispatcher census omits {field}");
    }
    let miss = dispatch.find("if (!code) {").expect("map-miss branch");
    let translate = dispatch[miss..]
        .find("G_TRANSLATE_BLOCK")
        .map(|offset| miss + offset)
        .expect("translation after map miss");
    let count = dispatch[miss..translate]
        .find("hl_backend_tree_map_miss();")
        .map(|offset| miss + offset)
        .expect("map-miss census at the dispatcher decision");
    assert!(miss < count && count < translate);
}

#[test]
fn x86_runtime_has_no_unreachable_debug_kill_switches() {
    let native = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/native");
    for relative in [
        "engine/target/x86_64.c",
        "linux_abi/x86.c",
        "linux_abi/syscall/dispatch.c",
        "translator/guest/x86_64/dispatch.h",
        "translator/guest/x86_64/emit.c",
        "translator/guest/x86_64/glue.c",
        "translator/guest/x86_64/glue.h",
        "translator/guest/x86_64/lower/branch.c",
        "translator/guest/x86_64/lower/branch.h",
        "translator/guest/x86_64/lower/sse.c",
        "translator/guest/x86_64/translate.c",
    ] {
        let source = fs::read_to_string(native.join(relative)).expect("read x86 runtime source");
        for legacy in [
            "g_noibtc",
            "g_itrace",
            "g_nochain",
            "g_tracecap",
            "g_diag",
            "g_systrace",
            "g_notier2x",
            "ibtc1way",
            "nosseopt",
            "noeaopt",
            "notier2x",
        ] {
            assert!(!source.contains(legacy), "{relative} retains dormant selector {legacy}");
        }
    }
}

#[test]
fn x86_dispatch_has_no_executable_specific_malloc_probe() {
    let native = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/native/translator/guest/x86_64");
    for relative in ["dispatch.h", "glue.c", "glue.h"] {
        let source = fs::read_to_string(native.join(relative)).expect("read x86 diagnostic source");
        for legacy in ["g_malloc_n", "g_w8", "avail_mask", "__libc_malloc_impl"] {
            assert!(
                !source.contains(legacy),
                "{relative} retains legacy diagnostic {legacy}"
            );
        }
    }
}

#[test]
fn x86_emulation_has_no_unreachable_exit_profiler() {
    let native = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/native/translator/guest/x86_64");
    for relative in ["avx.c", "avx.h", "avx_internal.h", "sse.c"] {
        let source = fs::read_to_string(native.join(relative)).expect("read x86 emulation source");
        for legacy in ["g_xs_on", "g_xs_rip", "xs_note", "hl_x86_avx_dump", "exitstat"] {
            assert!(
                !source.contains(legacy),
                "{relative} retains unreachable profiler {legacy}"
            );
        }
    }
}

#[test]
fn aarch64_ibtc_profile_counters_are_diagnostic_only() {
    let dispatch = std::fs::read_to_string(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/native/translator/guest/aarch64/dispatch.h"),
    )
    .expect("AArch64 dispatcher source");
    for counter in ["g_prof_miss++", "g_mtfill++"] {
        assert!(
            dispatch.contains(&format!("if (g_prof) {counter};")),
            "{counter} is updated while diagnostics are disabled"
        );
    }
}

#[test]
fn aarch64_shared_soft_resolver_avoids_darwin_reserved_x18() {
    let source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/native/translator/guest/aarch64/translate/emit/soft.c"),
    )
    .expect("AArch64 soft-memory emitter source");
    let start = source
        .find("if (g_soft_resolver_patch_count) {")
        .expect("shared soft resolver");
    let end = source[start..]
        .find("\n    if (g_soft_stub_patch_count)")
        .map(|end| start + end)
        .expect("end of shared soft resolver");
    let resolver = &source[start..end];
    for forbidden in [
        "e_ldr(18,",
        "e_str(18,",
        "e_br(18)",
        "(18u <<",
        "a64_cbnz_x(18",
        "a64_tbz_x(18",
    ] {
        assert!(
            !resolver.contains(forbidden),
            "shared soft resolver uses Darwin-reserved x18 via {forbidden}"
        );
    }
}

#[test]
fn faccessat2_uses_linux_guest_flag_values() {
    let source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/native/linux_abi/syscall/fs/extended_status.c"),
    )
    .expect("extended status syscall source");
    for declaration in [
        "GUEST_AT_SYMLINK_NOFOLLOW = 0x100",
        "GUEST_AT_EACCESS = 0x200",
        "GUEST_AT_EMPTY_PATH = 0x1000",
    ] {
        assert!(
            source.contains(declaration),
            "missing Linux ABI declaration {declaration}"
        );
    }
    assert!(
        !source.contains("(a3 & AT_"),
        "guest faccessat2 flags depend on host AT_* values"
    );
}

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
