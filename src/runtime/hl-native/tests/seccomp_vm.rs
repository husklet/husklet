use std::{fs, path::PathBuf, process::Command};

#[test]
fn validator_rejects_out_of_range_jump_before_filter_publication() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let scratch = std::env::temp_dir().join(format!("hl-native-seccomp-vm-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("create seccomp VM probe directory");
    let source = scratch.join("seccomp_vm_probe.c");
    let executable = scratch.join("seccomp_vm_probe");
    fs::write(
        &source,
        r#"
#include "linux_abi/seccomp_vm.h"

int main(void) {
    const struct hl_linux_sock_filter valid[] = {
        {0x20, 0, 0, 0},
        {0x15, 0, 1, 172},
        {0x06, 0, 0, HL_LINUX_SECCOMP_RET_ALLOW},
        {0x06, 0, 0, HL_LINUX_SECCOMP_RET_ERRNO | 23},
    };
    const struct hl_linux_sock_filter invalid_jump[] = {{0x05, 0, 0, 8}};
    const struct hl_linux_sock_filter invalid_divide[] = {
        {0x34, 0, 0, 0},
        {0x06, 0, 0, HL_LINUX_SECCOMP_RET_ALLOW},
    };
    if (!hl_seccomp_validate(valid, 4)) return 1;
    if (hl_seccomp_validate(invalid_jump, 1)) return 2;
    if (hl_seccomp_validate(invalid_divide, 2)) return 3;
    return 0;
}
"#,
    )
    .expect("write seccomp VM probe source");
    let compile = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(format!("-I{}", native.display()))
        .arg(&source)
        .arg(native.join("linux_abi/seccomp_vm.c"))
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("seccomp VM probe compiler");
    assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));
    let run = Command::new(&executable).status().expect("seccomp VM probe execution");
    assert!(run.success(), "seccomp VM probe failed with {run}");
    fs::remove_dir_all(scratch).expect("remove seccomp VM probe directory");
}
