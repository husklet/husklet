use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    process::Command,
};

fn headers(directory: &Path, output: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("native header directory") {
        let path = entry.expect("native header entry").path();
        if path.is_dir() {
            headers(&path, output);
        } else if path.extension().is_some_and(|extension| extension == "h") {
            output.push(path);
        }
    }
}

#[test]
fn elf64_parser_rejects_hostile_ranges_before_loader_access() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let scratch = std::env::temp_dir().join(format!("hl-native-elf64-parser-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("create ELF parser probe directory");
    let source = scratch.join("elf64_parser.c");
    let executable = scratch.join("elf64_parser");
    fs::write(
        &source,
        r#"
#include "linux_abi/image.h"
#include <stdint.h>
#include <string.h>

static void put16(uint8_t *p, uint16_t v) { memcpy(p, &v, sizeof v); }
static void put32(uint8_t *p, uint32_t v) { memcpy(p, &v, sizeof v); }
static void put64(uint8_t *p, uint64_t v) { memcpy(p, &v, sizeof v); }

static void valid(uint8_t *b, uint16_t machine) {
    memset(b, 0, 256);
    memcpy(b, "\177ELF\2\1\1", 7);
    put16(b + 16, 2); put16(b + 18, machine); put32(b + 20, 1);
    put64(b + 32, 64); put16(b + 52, 64); put16(b + 54, 56); put16(b + 56, 1);
    put64(b + 24, 0x400000); put32(b + 64, 1); put32(b + 68, 5); put64(b + 72, 120); put64(b + 80, 0x400000);
    put64(b + 96, 8); put64(b + 104, 4096); put64(b + 112, 8);
}

int main(void) {
    uint8_t bytes[256]; hl_linux_elf64_layout layout;
    hl_linux_image image = {bytes, sizeof bytes};
    valid(bytes, 0xb7);
    if (hl_linux_elf64_validate(&image, 0xb7, &layout) != 0 || layout.load_end != 0x401000) return 1;
    if (hl_linux_elf64_validate(&image, 0x3e, &layout) == 0) return 2;
    image.size = 63;
    if (hl_linux_elf64_validate(&image, 0xb7, &layout) == 0) return 3;
    image.size = sizeof bytes; valid(bytes, 0xb7); put64(bytes + 32, UINT64_MAX - 8);
    if (hl_linux_elf64_validate(&image, 0xb7, &layout) == 0) return 4;
    valid(bytes, 0xb7); put64(bytes + 72, UINT64_MAX - 3); put64(bytes + 96, 8);
    if (hl_linux_elf64_validate(&image, 0xb7, &layout) == 0) return 5;
    valid(bytes, 0xb7); put64(bytes + 80, UINT64_MAX - 7); put64(bytes + 104, 16);
    if (hl_linux_elf64_validate(&image, 0xb7, &layout) == 0) return 6;
    valid(bytes, 0xb7); put16(bytes + 56, 2); memcpy(bytes + 120, bytes + 64, 56);
    put64(bytes + 120 + 16, UINT64_MAX - 7); put64(bytes + 120 + 40, 16);
    if (hl_linux_elf64_validate(&image, 0xb7, &layout) == 0) return 9;
    valid(bytes, 0xb7); put16(bytes + 56, 2); memcpy(bytes + 120, bytes + 64, 56);
    put64(bytes + 96, 0); put64(bytes + 104, 0); put64(bytes + 80, 0);
    if (hl_linux_elf64_validate(&image, 0xb7, &layout) != 0 || layout.load_start != 0x400000) return 10;
    valid(bytes, 0xb7); put16(bytes + 56, 3); memcpy(bytes + 120, bytes + 64, 56);
    memcpy(bytes + 176, bytes + 64, 56); put32(bytes + 120, 3); put32(bytes + 176, 3);
    put64(bytes + 128, 240); put64(bytes + 152, 2); put64(bytes + 184, 240); put64(bytes + 208, 2);
    bytes[240] = 'x'; bytes[241] = 0;
    if (hl_linux_elf64_validate(&image, 0xb7, &layout) == 0) return 11;
    valid(bytes, 0xb7); put32(bytes + 64, 3); put64(bytes + 72, 250); put64(bytes + 96, 8);
    if (hl_linux_elf64_validate(&image, 0xb7, &layout) == 0) return 7;
    valid(bytes, 0x3e);
    if (hl_linux_elf64_validate(&image, 0x3e, &layout) != 0) return 8;
    return 0;
}

"#,
    )
    .expect("write ELF parser probe source");
    let compile = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(format!("-I{}", native.display()))
        .arg(format!("-I{}", native.join("include").display()))
        .arg(&source)
        .arg(native.join("linux_abi/image.c"))
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("ELF parser probe compiler");
    assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));
    let run = Command::new(&executable).status().expect("ELF parser probe execution");
    assert!(run.success(), "ELF parser probe failed with {run}");
    fs::remove_dir_all(scratch).expect("remove ELF parser probe directory");
}

#[test]
fn virtual_dac_policy_uses_guest_identity_mode_groups_and_capabilities() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let scratch = std::env::temp_dir().join(format!("hl-native-dac-policy-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("create DAC policy probe directory");
    let source = scratch.join("dac_policy.c");
    let executable = scratch.join("dac_policy");
    fs::write(
        &source,
        r#"
#include "linux_abi/container/dac_policy.h"

int main(void) {
    const uint32_t groups[] = {30, 40};
    const hl_dac_snapshot owned = {2000, 20, 0640};
    const hl_dac_snapshot foreign = {2000, 30, 0770};
    const hl_dac_snapshot world_writable = {2000, 30, 0002};
    const hl_dac_snapshot closed = {0, 0, 0755};
    hl_dac_credentials user = {2000, 20, groups, 2, 0};
    hl_dac_credentials other = {3000, 50, groups, 0, 0};
    hl_dac_credentials privileged = other;

    if (hl_dac_authorize_chmod(&owned, &user) != 0) return 1;
    if (hl_dac_authorize_chmod(&owned, &other) != EPERM) return 2;
    privileged.capabilities = UINT64_C(1) << HL_DAC_CAP_FOWNER;
    if (hl_dac_authorize_chmod(&owned, &privileged) != 0) return 3;
    if (hl_dac_authorize_explicit_times(&owned, &other) != EPERM) return 4;
    if (hl_dac_authorize_now_times(&owned, &other) != EACCES) return 12;
    if (hl_dac_authorize_now_times(&world_writable, &other) != 0) return 13;

    if (hl_dac_authorize_chown(&owned, &user, 2000, 30) != 0) return 5;
    if (hl_dac_authorize_chown(&owned, &user, 3000, 30) != EPERM) return 6;
    if (hl_dac_authorize_chown(&owned, &user, UINT32_C(0x80000000), 30) != EPERM) return 14;
    if (hl_dac_authorize_chown(&owned, &other, 3000, 50) != EPERM) return 7;
    privileged.capabilities = UINT64_C(1) << HL_DAC_CAP_CHOWN;
    if (hl_dac_authorize_chown(&owned, &privileged, 3000, 50) != 0) return 8;

    user.fsuid = 3000;
    user.fsgid = 50;
    if (hl_dac_authorize_create(&foreign, &user) != 0) return 9;
    if (hl_dac_authorize_create(&closed, &user) != EACCES) return 10;
    user.capabilities = UINT64_C(1) << HL_DAC_CAP_DAC_OVERRIDE;
    if (hl_dac_authorize_create(&closed, &user) != 0) return 11;
    const hl_dac_snapshot sticky = {1000, 1000, 01777};
    hl_dac_credentials owner = user;
    owner.fsuid = 2000;
    if (hl_dac_authorize_sticky(&sticky, &owned, &other) != EPERM) return 15;
    if (hl_dac_authorize_sticky(&sticky, &owned, &owner) != 0) return 16;
    return 0;
}
"#,
    )
    .expect("write DAC policy probe source");
    let compile = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(format!("-I{}", native.display()))
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("DAC policy probe compiler");
    assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));
    let run = Command::new(&executable).status().expect("DAC policy probe execution");
    assert!(run.success(), "DAC policy probe failed with {run}");
    fs::remove_dir_all(scratch).expect("remove DAC policy probe directory");
}

#[cfg(target_os = "linux")]
#[test]
fn guest_path_composition_rejects_truncation() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let scratch = std::env::temp_dir().join(format!("hl-native-path-compose-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("create path composition probe directory");
    let source = scratch.join("path_compose.c");
    let executable = scratch.join("path_compose");
    fs::write(
        &source,
        r#"
#include "linux_abi/container/vfs/path_compose.h"
#include <string.h>

int main(void) {
    char exact[8];
    if (hl_guest_path_compose(exact, sizeof exact, "ab", "cde", 1) != 0) return 1;
    if (strcmp(exact, "/ab/cde") != 0) return 2;
    char short_buffer[7] = "poison";
    if (hl_guest_path_compose(short_buffer, sizeof short_buffer, "ab", "cde", 1) == 0) return 3;
    if (short_buffer[0] != '\0') return 4;
    if (hl_guest_path_copy(short_buffer, sizeof short_buffer, "/abcdef") == 0) return 5;
    if (short_buffer[0] != '\0') return 6;
    return 0;
}

"#,
    )
    .expect("write path composition probe source");
    let compile = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(format!("-I{}", native.display()))
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("path composition probe compiler");
    assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));
    let run = Command::new(&executable)
        .status()
        .expect("path composition probe execution");
    assert!(run.success(), "path composition probe failed with {run}");
    fs::remove_dir_all(scratch).expect("remove path composition probe directory");
}

#[cfg(target_os = "linux")]
#[test]
fn fatal_guest_signal_diagnostic_is_exact_and_selector_gated() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let scratch = std::env::temp_dir().join(format!("hl-native-fatal-log-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("create fatal log probe directory");
    let source = scratch.join("fatal_log.c");
    let executable = scratch.join("fatal_log");
    fs::write(
        &source,
        r#"
#include "engine/fatal_diagnostic.h"
#include <string.h>

static char captured[256];
static size_t captured_size;
static unsigned calls;

static void emit(void *context, uint32_t tag, const char *message, size_t size) {
    (void)context;
    if (tag != HL_LOG_TAG_SIGNAL || size >= sizeof captured) return;
    memcpy(captured, message, size);
    captured[size] = '\0';
    captured_size = size;
    calls++;
}

int main(void) {
    const hl_host_log_services logs = {HL_HOST_LOG_ABI, sizeof(logs), emit};
    hl_host_services host = {0};
    host.abi = HL_HOST_SERVICES_ABI;
    host.size = sizeof(host);
    host.capabilities = HL_HOST_CAP_LOG;
    host.log = &logs;
    hl_fatal_diagnostic_init(&host, "0");
    hl_fatal_diagnostic_publish(11, 0x1234, 0x5678, 0x9abc);
    if (calls != 0 || captured_size != 0) return 2;
    hl_fatal_diagnostic_init(&host, "1");
    hl_fatal_diagnostic_publish(11, 0x1234, 0x5678, 0x9abc);
    const char expected[] = "fatal-guest-signal signal=11 pc=0x1234 sp=0x5678 lr=0x9abc\n";
    if (calls != 1 || captured_size != sizeof expected - 1 || strcmp(captured, expected) != 0) return 4;
    return 0;
}
"#,
    )
    .expect("write fatal log probe source");
    let compile = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(format!("-I{}", native.display()))
        .arg(format!("-I{}", native.join("include").display()))
        .arg(&source)
        .arg(native.join("engine/fatal_diagnostic.c"))
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("fatal log probe compiler");
    assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));
    let run = Command::new(&executable).status().expect("fatal log probe execution");
    assert!(run.success(), "fatal log probe failed with {run}");
    fs::remove_dir_all(scratch).expect("remove fatal log probe directory");
}

#[test]
fn fatal_guest_signal_path_excludes_printf_family_formatting() {
    let source =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/native/engine/fatal_diagnostic.c"))
            .expect("read native logging source");
    let start = source
        .find("void hl_fatal_diagnostic_publish(")
        .expect("fatal diagnostic helper");
    let body = &source[start..];
    for forbidden in ["printf(", "snprintf(", "sprintf(", "hl_log_format(", "hl_log_message("] {
        assert!(
            !body.contains(forbidden),
            "fatal signal path uses non-signal-safe {forbidden}"
        );
    }
}

#[test]
fn owned_native_headers_are_self_contained() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let guest = native.join("translator/guest/x86_64");
    let mut owned = Vec::new();
    headers(&guest, &mut owned);
    for boundary in ["bridge", "engine", "host/linux", "host/macos", "include", "linux_abi"] {
        headers(&native.join(boundary), &mut owned);
    }
    if cfg!(target_os = "windows") {
        headers(&native.join("host/windows"), &mut owned);
    } else {
        // These shared Windows boundaries are deliberately empty or Win32-type-free off Windows. The
        // fault and internal headers require the platform SDK and join the sweep on a Windows host.
        owned.extend([
            native.join("host/windows/launch.h"),
            native.join("host/windows/win32.h"),
        ]);
    }
    owned.sort();

    // These files explicitly document that they are implementation/composition fragments expanded only
    // after their target translation unit has established the target-specific macros and private helpers.
    owned.retain(|path| {
        let relative = path.strip_prefix(&native).expect("native header");
        let relative = relative.to_string_lossy();
        !matches!(
            relative.as_ref(),
            "translator/guest/x86_64/dispatch.h"
                | "translator/guest/x86_64/interp_dispatch.h"
                | "linux_abi/elf_protect.h"
                | "linux_abi/guest_stat.h"
                | "linux_abi/syscall/nonpie_args.h"
                | "linux_abi/syscall/sysv_state.h"
        )
    });

    let scratch = std::env::temp_dir().join(format!("hl-native-header-probe-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("header probe directory");
    let probe = scratch.join("probe.c");
    let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    for header in owned {
        let relative = header.strip_prefix(&native).expect("owned native header");
        fs::write(&probe, format!("#include \"{}\"\n", relative.display())).expect("header probe source");
        let result = Command::new(&compiler)
            .args([
                "-std=c11",
                "-D_GNU_SOURCE",
                "-Werror=implicit-function-declaration",
                "-fsyntax-only",
            ])
            .arg(format!("-I{}", guest.display()))
            .arg(format!("-I{}", native.display()))
            .arg(format!("-I{}", native.join("include").display()))
            .arg(&probe)
            .output()
            .expect("C compiler for header probe");
        assert!(
            result.status.success(),
            "{} is not a self-contained first include:\n{}",
            header.display(),
            String::from_utf8_lossy(&result.stderr)
        );
    }
    fs::remove_dir_all(scratch).expect("remove header probe directory");
}

#[test]
fn public_abi_is_self_contained_for_c_and_cpp_consumers() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let include = native.join("include");
    let scratch = std::env::temp_dir().join(format!("hl-native-public-abi-probe-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("public ABI probe directory");
    let mut public = headers_in(&include.join("hl"));
    public.push(native.join("bridge/api.h"));
    public.sort();

    for (compiler, standard, extension) in [
        (std::env::var_os("CC").unwrap_or_else(|| "cc".into()), "c11", "c"),
        (std::env::var_os("CXX").unwrap_or_else(|| "c++".into()), "c++17", "cpp"),
    ] {
        for header in &public {
            // linux_abi exposes C11 atomic_flag as part of its concrete implementation state. Its API is C;
            // the portable bridge and all other public headers remain valid C++ boundaries.
            if extension == "cpp" && header.ends_with("linux_abi.h") {
                continue;
            }
            let relative = header.strip_prefix(&native).expect("public native header");
            let probe = scratch.join(format!("probe.{extension}"));
            fs::write(&probe, format!("#include \"{}\"\n", relative.display())).expect("public ABI probe source");
            let result = Command::new(&compiler)
                .arg(format!("-std={standard}"))
                .arg("-fsyntax-only")
                .arg(format!("-I{}", native.display()))
                .arg(format!("-I{}", include.display()))
                .arg(&probe)
                .output()
                .expect("C or C++ compiler for public ABI probe");
            assert!(
                result.status.success(),
                "{} is not self-contained under {standard}:\n{}",
                header.display(),
                String::from_utf8_lossy(&result.stderr)
            );
        }
    }
    fs::remove_dir_all(scratch).expect("remove public ABI probe directory");
}

fn headers_in(directory: &Path) -> Vec<PathBuf> {
    let mut output = Vec::new();
    headers(directory, &mut output);
    output
}

#[test]
fn public_visibility_selects_export_and_import_annotations() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let include = package.join("src/native/include");
    let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    let preprocess = |definitions: &[&str]| {
        let mut command = Command::new(&compiler);
        command.args(["-E", "-P", "-x", "c"]);
        for definition in definitions {
            command.arg(format!("-D{definition}"));
        }
        command.arg(format!("-I{}", include.display())).arg("-");
        let mut child = command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("C preprocessor for visibility probe");
        child
            .stdin
            .take()
            .expect("visibility probe stdin")
            .write_all(b"#include \"hl/base.h\"\nHL_API void hl_visibility_probe(void);\n")
            .expect("visibility probe source");
        let output = child.wait_with_output().expect("visibility probe output");
        assert!(output.status.success());
        String::from_utf8(output.stdout).expect("visibility probe UTF-8")
    };

    let posix = preprocess(&[]);
    assert!(posix.contains("visibility(\"default\")"));
    let windows_export = preprocess(&["_WIN32", "HL_SHARED", "HL_BUILDING_ENGINE"]);
    assert!(windows_export.contains("__declspec(dllexport)"));
    let windows_import = preprocess(&["_WIN32", "HL_SHARED"]);
    assert!(windows_import.contains("__declspec(dllimport)"));
}

#[test]
fn service_validation_rejects_truncated_versions_and_null_callbacks() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let scratch = std::env::temp_dir().join(format!("hl-native-service-abi-probe-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("service ABI probe directory");
    let source = scratch.join("probe.c");
    let executable = scratch.join("probe");
    fs::write(
        &source,
        r#"#include "hl/host_services.h"

int main(void) {
    hl_host_services host = {0};
    host.abi = HL_HOST_SERVICES_ABI;
    host.size = sizeof(host);
    if (hl_host_services_validate(&host, 0) != HL_STATUS_OK) return 1;

    host.size = sizeof(host) - 1;
    if (hl_host_services_validate(&host, 0) != HL_STATUS_ABI_MISMATCH) return 2;
    host.size = sizeof(host);
    host.abi = HL_HOST_SERVICES_ABI - 1;
    if (hl_host_services_validate(&host, 0) != HL_STATUS_ABI_MISMATCH) return 3;
    host.abi = HL_HOST_SERVICES_ABI;

    hl_host_memory_services memory = {0};
    memory.abi = 7;
    memory.size = 144;
    host.capabilities = HL_HOST_CAP_MEMORY;
    host.memory = &memory;
    if (hl_host_services_validate(&host, HL_HOST_CAP_MEMORY) != HL_STATUS_ABI_MISMATCH) return 4;
    memory.abi = HL_HOST_MEMORY_ABI;
    memory.size = sizeof(memory);
    if (hl_host_services_validate(&host, HL_HOST_CAP_MEMORY) != HL_STATUS_ABI_MISMATCH) return 5;

    hl_host_network_services network = {0};
    network.abi = 1;
    network.size = 56;
    host.capabilities = HL_HOST_CAP_NETWORK;
    host.memory = 0;
    host.network = &network;
    if (hl_host_services_validate(&host, HL_HOST_CAP_NETWORK) != HL_STATUS_ABI_MISMATCH) return 6;

    hl_host_sync_services sync = {0};
    sync.abi = 2;
    sync.size = 64;
    host.capabilities = HL_HOST_CAP_SYNC;
    host.network = 0;
    host.sync = &sync;
    if (hl_host_services_validate(&host, HL_HOST_CAP_SYNC) != HL_STATUS_ABI_MISMATCH) return 7;
    return 0;
}
"#,
    )
    .expect("service ABI probe source");
    let compile = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(format!("-I{}", native.join("include").display()))
        .arg(&source)
        .arg(native.join("engine/host_services.c"))
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("service ABI probe compiler");
    assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));
    let run = Command::new(&executable).status().expect("service ABI probe execution");
    assert!(run.success(), "service ABI probe failed with {run}");
    fs::remove_dir_all(scratch).expect("remove service ABI probe directory");
}

#[test]
fn engine_create_validation_clears_stale_output() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let scratch = std::env::temp_dir().join(format!("hl-native-engine-output-probe-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("engine output probe directory");
    let source = scratch.join("probe.c");
    let executable = scratch.join("probe");
    fs::write(
        &source,
        r#"#include "engine/runtime.c"

int main(void) {
    hl_engine *output = (hl_engine *)(uintptr_t)1;
    hl_engine_config config = {0};
    hl_host_services host = {0};
    if (hl_engine_create_validate(&config, &host, 0, 0, &output) != HL_STATUS_ABI_MISMATCH) return 1;
    if (output != 0) return 2;
    output = (hl_engine *)(uintptr_t)1;
    if (hl_engine_create_validate(0, &host, 0, 0, &output) != HL_STATUS_INVALID_ARGUMENT) return 3;
    return output == 0 ? 0 : 4;
}
"#,
    )
    .expect("engine output probe source");
    let compile = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=c11", "-D_GNU_SOURCE", "-ffunction-sections", "-fdata-sections"])
        .arg(if cfg!(target_os = "macos") {
            "-Wl,-dead_strip"
        } else {
            "-Wl,--gc-sections"
        })
        .arg(format!("-I{}", native.display()))
        .arg(format!("-I{}", native.join("include").display()))
        .arg(&source)
        .arg(native.join("engine/host_services.c"))
        .arg(native.join("engine/options.c"))
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("engine output probe compiler");
    assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));
    let run = Command::new(&executable)
        .status()
        .expect("engine output probe execution");
    assert!(run.success(), "engine output probe failed with {run}");
    fs::remove_dir_all(scratch).expect("remove engine output probe directory");
}

#[test]
fn engine_run_validation_clears_stale_exit_payload() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let scratch = std::env::temp_dir().join(format!("hl-native-run-output-probe-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("run output probe directory");
    let source = scratch.join("probe.c");
    let executable = scratch.join("probe");
    fs::write(
        &source,
        r#"#include "engine/runtime.c"

int main(void) {
    hl_engine_exit output = {HL_ENGINE_ABI, sizeof(output), HL_ENGINE_EXIT_SIGNAL, 99, 123};
    if (hl_engine_run_validate(0, 0, 0, &output) != HL_STATUS_INVALID_ARGUMENT) return 1;
    if (output.kind != HL_ENGINE_EXIT_NONE || output.guest_status != 0 || output.detail != 0) return 2;

    output = (hl_engine_exit){HL_ENGINE_ABI, sizeof(output), HL_ENGINE_EXIT_CODE, 88, 456};
    if (hl_engine_run_validate((hl_engine *)(uintptr_t)1, 1, 0, &output) != HL_STATUS_INVALID_ARGUMENT) return 3;
    if (output.kind != HL_ENGINE_EXIT_NONE || output.guest_status != 0 || output.detail != 0) return 4;

    output = (hl_engine_exit){0, sizeof(output), HL_ENGINE_EXIT_CODE, 77, 789};
    if (hl_engine_run_validate(0, 0, 0, &output) != HL_STATUS_ABI_MISMATCH) return 5;
    return output.kind == HL_ENGINE_EXIT_CODE && output.guest_status == 77 && output.detail == 789 ? 0 : 6;
}
"#,
    )
    .expect("run output probe source");
    let compile = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=c11", "-D_GNU_SOURCE", "-ffunction-sections", "-fdata-sections"])
        .arg(if cfg!(target_os = "macos") {
            "-Wl,-dead_strip"
        } else {
            "-Wl,--gc-sections"
        })
        .arg(format!("-I{}", native.display()))
        .arg(format!("-I{}", native.join("include").display()))
        .arg(&source)
        .arg(native.join("engine/host_services.c"))
        .arg(native.join("engine/options.c"))
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("run output probe compiler");
    assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));
    let run = Command::new(&executable).status().expect("run output probe execution");
    assert!(run.success(), "run output probe failed with {run}");
    fs::remove_dir_all(scratch).expect("remove run output probe directory");
}

#[test]
fn descriptor_install_failure_clears_stale_guest_fd() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let scratch = std::env::temp_dir().join(format!("hl-native-fd-output-probe-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("descriptor output probe directory");
    let source = scratch.join("probe.c");
    let executable = scratch.join("probe");
    fs::write(
        &source,
        r#"#include "linux_abi/descriptor_output.h"

int main(void) {
    hl_linux_fd output = 7;
    if (!hl_linux_fd_output_prepare(&output)) return 1;
    if (output != HL_LINUX_FD_LIMIT) return 2;
    return hl_linux_fd_output_prepare(0) == 0 ? 0 : 3;
}
"#,
    )
    .expect("descriptor output probe source");
    let compile = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(format!("-I{}", native.display()))
        .arg(format!("-I{}", native.join("include").display()))
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("descriptor output probe compiler");
    assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));
    let run = Command::new(&executable)
        .status()
        .expect("descriptor output probe execution");
    assert!(run.success(), "descriptor output probe failed with {run}");
    fs::remove_dir_all(scratch).expect("remove descriptor output probe directory");
}

#[test]
fn descriptor_dup_failure_uses_invalid_descriptor_sentinel() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let scratch = std::env::temp_dir().join(format!("hl-native-dup-output-probe-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("duplicate output probe directory");
    let source = scratch.join("probe.c");
    let executable = scratch.join("probe");
    fs::write(
        &source,
        r#"#include "linux_abi/descriptor_output.h"

int main(void) {
    hl_linux_fd duplicate = 9;
    if (hl_linux_fd_output_validate_context(0, &duplicate) != HL_STATUS_INVALID_ARGUMENT) return 1;
    if (duplicate != HL_LINUX_FD_LIMIT) return 2;
    return hl_linux_fd_output_validate_context(0, 0) == HL_STATUS_INVALID_ARGUMENT ? 0 : 3;
}
"#,
    )
    .expect("duplicate output probe source");
    let compile = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(format!("-I{}", native.display()))
        .arg(format!("-I{}", native.join("include").display()))
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("duplicate output probe compiler");
    assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));
    let run = Command::new(&executable)
        .status()
        .expect("duplicate output probe execution");
    assert!(run.success(), "duplicate output probe failed with {run}");
    fs::remove_dir_all(scratch).expect("remove duplicate output probe directory");
}

#[test]
fn linux_spawn_failure_clears_stale_process_handle() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let scratch = std::env::temp_dir().join(format!("hl-native-process-output-probe-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("process output probe directory");
    let source = scratch.join("probe.c");
    let executable = scratch.join("probe");
    fs::write(
        &source,
        r#"#include "linux_abi/process_output.h"

int main(void) {
    hl_host_handle output = 42;
    if (!hl_linux_process_output_prepare(&output)) return 1;
    if (output != HL_HOST_HANDLE_INVALID) return 2;
    return hl_linux_process_output_prepare(0) == 0 ? 0 : 3;
}
"#,
    )
    .expect("process output probe source");
    let compile = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(format!("-I{}", native.display()))
        .arg(format!("-I{}", native.join("include").display()))
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("process output probe compiler");
    assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));
    let run = Command::new(&executable)
        .status()
        .expect("process output probe execution");
    assert!(run.success(), "process output probe failed with {run}");
    fs::remove_dir_all(scratch).expect("remove process output probe directory");
}

#[test]
fn linux_file_map_failure_clears_stale_mapping() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let scratch = std::env::temp_dir().join(format!("hl-native-map-output-probe-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("mapping output probe directory");
    let source = scratch.join("probe.c");
    let executable = scratch.join("probe");
    fs::write(
        &source,
        r#"#include "linux_abi/mapping_output.h"

int main(void) {
    hl_host_file_mapping output = {HL_HOST_FILE_MAPPING_ABI, sizeof(output), 42, 0x1000, 0x2000, 7};
    if (!hl_linux_file_mapping_output_prepare(&output)) return 1;
    if (output.handle != HL_HOST_HANDLE_INVALID || output.address != 0 || output.mapped_size != 0 ||
        output.reserved != 0) return 2;
    output = (hl_host_file_mapping){0, sizeof(output), 42, 0x1000, 0x2000, 7};
    if (hl_linux_file_mapping_output_prepare(&output) != 0) return 3;
    return output.handle == 42 && output.address == 0x1000 ? 0 : 4;
}
"#,
    )
    .expect("mapping output probe source");
    let compile = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(format!("-I{}", native.display()))
        .arg(format!("-I{}", native.join("include").display()))
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("mapping output probe compiler");
    assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));
    let run = Command::new(&executable)
        .status()
        .expect("mapping output probe execution");
    assert!(run.success(), "mapping output probe failed with {run}");
    fs::remove_dir_all(scratch).expect("remove mapping output probe directory");
}

#[test]
fn descriptor_snapshot_failure_clears_stale_authority() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let scratch = std::env::temp_dir().join(format!("hl-native-snapshot-output-probe-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("snapshot output probe directory");
    let source = scratch.join("probe.c");
    let executable = scratch.join("probe");
    fs::write(
        &source,
        r#"#include "linux_abi/snapshot_output.h"

int main(void) {
    hl_linux_fd_snapshot output = {.fd = 7, .host_handle = 42, .offset = 99, .flock_token = 77};
    if (!hl_linux_fd_snapshot_output_prepare(&output)) return 1;
    if (output.fd != HL_LINUX_FD_LIMIT || output.host_handle != HL_HOST_HANDLE_INVALID) return 2;
    if (output.offset != 0 || output.flock_token != 0 || output.kind != 0) return 3;
    return hl_linux_fd_snapshot_output_prepare(0) == 0 ? 0 : 4;
}
"#,
    )
    .expect("snapshot output probe source");
    let compile = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(format!("-I{}", native.display()))
        .arg(format!("-I{}", native.join("include").display()))
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("snapshot output probe compiler");
    assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));
    let run = Command::new(&executable)
        .status()
        .expect("snapshot output probe execution");
    assert!(run.success(), "snapshot output probe failed with {run}");
    fs::remove_dir_all(scratch).expect("remove snapshot output probe directory");
}

#[test]
fn descriptor_reservation_failure_clears_stale_token() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let scratch = std::env::temp_dir().join(format!("hl-native-reservation-output-probe-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("reservation output probe directory");
    let source = scratch.join("probe.c");
    let executable = scratch.join("probe");
    fs::write(
        &source,
        r#"#include "linux_abi/reservation_output.h"

int main(void) {
    hl_linux_fd_reservation output = {7, 42};
    if (!hl_linux_fd_reservation_output_prepare(&output)) return 1;
    if (output.fd != HL_LINUX_FD_LIMIT || output.generation != 0) return 2;
    return hl_linux_fd_reservation_output_prepare(0) == 0 ? 0 : 3;
}
"#,
    )
    .expect("reservation output probe source");
    let compile = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(format!("-I{}", native.display()))
        .arg(format!("-I{}", native.join("include").display()))
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("reservation output probe compiler");
    assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));
    let run = Command::new(&executable)
        .status()
        .expect("reservation output probe execution");
    assert!(run.success(), "reservation output probe failed with {run}");
    fs::remove_dir_all(scratch).expect("remove reservation output probe directory");
}

#[test]
fn descriptor_exec_failure_clears_stale_close_count() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let scratch = std::env::temp_dir().join(format!("hl-native-count-output-probe-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("count output probe directory");
    let source = scratch.join("probe.c");
    let executable = scratch.join("probe");
    fs::write(
        &source,
        r#"#include "linux_abi/count_output.h"

int main(void) {
    uint32_t output = 42;
    if (!hl_linux_count_output_prepare(&output)) return 1;
    if (output != 0) return 2;
    return hl_linux_count_output_prepare(0) == 0 ? 0 : 3;
}
"#,
    )
    .expect("count output probe source");
    let compile = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(format!("-I{}", native.display()))
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("count output probe compiler");
    assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));
    let run = Command::new(&executable)
        .status()
        .expect("count output probe execution");
    assert!(run.success(), "count output probe failed with {run}");
    fs::remove_dir_all(scratch).expect("remove count output probe directory");
}

#[test]
fn fork_prepare_failure_disarms_stale_plan() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let scratch = std::env::temp_dir().join(format!("hl-native-fork-output-probe-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("fork output probe directory");
    let source = scratch.join("probe.c");
    let executable = scratch.join("probe");
    fs::write(
        &source,
        r#"#include "linux_abi/fork_output.h"

int main(void) {
    hl_linux_fork_plan plan = {HL_LINUX_ABI_VERSION, sizeof(plan), 0, 0, 5, 1, 1};
    if (!hl_linux_fork_plan_output_prepare(&plan)) return 1;
    if (plan.count != 0 || plan.armed != 0 || plan.host_completed != 0) return 2;
    plan = (hl_linux_fork_plan){0, sizeof(plan), 0, 0, 5, 1, 1};
    if (hl_linux_fork_plan_output_prepare(&plan) != 0) return 3;
    return plan.count == 5 && plan.armed == 1 && plan.host_completed == 1 ? 0 : 4;
}
"#,
    )
    .expect("fork output probe source");
    let compile = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(format!("-I{}", native.display()))
        .arg(format!("-I{}", native.join("include").display()))
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("fork output probe compiler");
    assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));
    let run = Command::new(&executable).status().expect("fork output probe execution");
    assert!(run.success(), "fork output probe failed with {run}");
    fs::remove_dir_all(scratch).expect("remove fork output probe directory");
}

#[test]
fn file_status_failure_clears_stale_identity() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let scratch = std::env::temp_dir().join(format!("hl-native-status-output-probe-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("status output probe directory");
    let source = scratch.join("probe.c");
    let executable = scratch.join("probe");
    fs::write(
        &source,
        r#"#include "linux_abi/status_output.h"

int main(void) {
    hl_linux_file_status output = {.device = 7, .object = 42, .size = 99, .mode = 0777, .user = 5};
    if (!hl_linux_file_status_output_prepare(&output)) return 1;
    if (output.device != 0 || output.object != 0 || output.size != 0 || output.mode != 0 || output.user != 0) return 2;
    return hl_linux_file_status_output_prepare(0) == 0 ? 0 : 3;
}
"#,
    )
    .expect("status output probe source");
    let compile = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(format!("-I{}", native.display()))
        .arg(format!("-I{}", native.join("include").display()))
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("status output probe compiler");
    assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));
    let run = Command::new(&executable)
        .status()
        .expect("status output probe execution");
    assert!(run.success(), "status output probe failed with {run}");
    fs::remove_dir_all(scratch).expect("remove status output probe directory");
}

#[cfg(target_os = "linux")]
#[test]
fn cpp_bridge_declarations_retain_c_linkage() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let scratch = std::env::temp_dir().join(format!("hl-native-cpp-linkage-probe-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("C++ linkage probe directory");
    let source = scratch.join("probe.cpp");
    let object = scratch.join("probe.o");
    fs::write(
        &source,
        "#include \"bridge/api.h\"\nvoid probe() { hl_c_backend_destroy(nullptr); }\n",
    )
    .expect("C++ linkage probe source");
    let compile = Command::new(std::env::var_os("CXX").unwrap_or_else(|| "c++".into()))
        .args(["-std=c++17", "-c"])
        .arg(format!("-I{}", native.display()))
        .arg(format!("-I{}", native.join("include").display()))
        .arg(&source)
        .arg("-o")
        .arg(&object)
        .output()
        .expect("C++ linkage probe compiler");
    assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));
    let symbols = Command::new("nm")
        .arg("-u")
        .arg(&object)
        .output()
        .expect("nm for C++ linkage probe");
    assert!(symbols.status.success());
    let symbols = String::from_utf8(symbols.stdout).expect("nm UTF-8");
    assert!(
        symbols.lines().any(|line| line.ends_with(" U hl_c_backend_destroy")),
        "{symbols}"
    );
    fs::remove_dir_all(scratch).expect("remove C++ linkage probe directory");
}

#[test]
fn checkpoint_object_bounds_reject_oversize_and_inconsistent_counts() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let scratch = std::env::temp_dir().join(format!("hl-native-checkpoint-bounds-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("create checkpoint bounds probe directory");
    let source = scratch.join("checkpoint_bounds.c");
    let executable = scratch.join("checkpoint_bounds");
    fs::write(
        &source,
        r#"
#include "linux_abi/checkpoint/object_bounds.h"
#include <stdint.h>

int main(void) {
    size_t size = 0;
    if (ckpt_bounded_object_size(128, 16, 128, &size) != 0 || size != 128) return 1;
    if (ckpt_bounded_object_size(INT64_MAX, 16, 4096, &size) == 0) return 2;
    if (ckpt_bounded_object_size(15, 16, 4096, &size) == 0) return 3;
    if (ckpt_bounded_object_size(16, 16, 4096, 0) == 0) return 4;
    if (ckpt_counted_object_size(48, 16, 2, 16, 2) != 0) return 5;
    if (ckpt_counted_object_size(49, 16, 2, 16, 2) == 0) return 6;
    if (ckpt_counted_object_size(48, 16, 3, 16, 2) == 0) return 7;
    if (ckpt_counted_object_size(SIZE_MAX, 16, UINT64_MAX, 16, UINT64_MAX) == 0) return 8;
    if (ckpt_counted_object_size(16, 16, 0, 0, 0) == 0) return 9;
    if (ckpt_inotify_object_size(CKPT_INOTIFY_IMAGE_LIMIT, &size) != 0 ||
        size != CKPT_INOTIFY_IMAGE_LIMIT) return 10;
    if (ckpt_inotify_object_size((int64_t)CKPT_INOTIFY_IMAGE_LIMIT + 1, &size) == 0) return 11;
    if (ckpt_inotify_object_size(0, &size) == 0) return 12;
    size_t count = 0;
    if (ckpt_record_object_size(48, 16, 3, &size, &count) != 0 || size != 48 || count != 3) return 13;
    if (ckpt_record_object_size(-1, 16, 3, &size, &count) == 0) return 14;
    if (ckpt_record_object_size(49, 16, 4, &size, &count) == 0) return 15;
    if (ckpt_record_object_size(64, 16, 3, &size, &count) == 0) return 16;
    if (ckpt_record_object_size(0, 0, 0, &size, &count) == 0) return 17;
    if (ckpt_record_object_size(0, 16, 0, &size, &count) != 0 || size != 0 || count != 0) return 18;
    if (ckpt_minimum_counted_object_size(160, 2, 80, 4) != 0) return 19;
    if (ckpt_minimum_counted_object_size(159, 2, 80, 4) == 0) return 20;
    if (ckpt_minimum_counted_object_size(INT64_MAX, 5, 80, 4) == 0) return 21;
    if (ckpt_minimum_counted_object_size(INT64_MAX, UINT64_MAX, 80, UINT64_MAX) == 0) return 22;
    if (ckpt_minimum_counted_object_size(0, 0, 0, 0) == 0) return 23;
    if (ckpt_capacity_object_size(65536, 65536, &size) != 0 || size != 65536) return 24;
    if (ckpt_capacity_object_size(65537, 65536, &size) == 0) return 25;
    if (ckpt_capacity_object_size(-1, 65536, &size) == 0) return 26;
    if (ckpt_capacity_object_size(0, 0, &size) == 0) return 27;
    if (ckpt_decimal_capacity("8192", 65536, 1048576, &size) != 0 || size != 8192) return 28;
    if (ckpt_decimal_capacity("0", 65536, 1048576, &size) != 0 || size != 65536) return 29;
    if (ckpt_decimal_capacity("8192x", 65536, 1048576, &size) == 0) return 30;
    if (ckpt_decimal_capacity("1048577", 65536, 1048576, &size) == 0) return 31;
    if (ckpt_decimal_capacity("", 65536, 1048576, &size) == 0) return 32;
    if (ckpt_fixed_payload_object_size(48, 16, 2, 16, 2, &size) != 0 || size != 32) return 33;
    if (ckpt_fixed_payload_object_size(47, 16, 2, 16, 2, &size) == 0) return 34;
    if (ckpt_fixed_payload_object_size(49, 16, 2, 16, 2, &size) == 0) return 35;
    if (ckpt_fixed_payload_object_size(48, 16, 3, 16, 2, &size) == 0) return 36;
    if (ckpt_fixed_payload_object_size(INT64_MAX, 16, UINT64_MAX, 16, UINT64_MAX, &size) == 0) return 37;
    if (ckpt_fixed_payload_object_size(48, 16, 2, 16, 2, 0) == 0) return 38;
    return 0;
}
"#,
    )
    .expect("write checkpoint bounds probe source");
    let compile = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(format!("-I{}", native.display()))
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("checkpoint bounds probe compiler");
    assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));
    let run = Command::new(&executable)
        .status()
        .expect("checkpoint bounds probe execution");
    assert!(run.success(), "checkpoint bounds probe failed with {run}");
    fs::remove_dir_all(scratch).expect("remove checkpoint bounds probe directory");
}
