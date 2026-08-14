use std::{fs, path::PathBuf, process::Command};

#[test]
fn guest_iovec_validation_preserves_linux_order_and_bounds() {
    let native = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/native");
    let scratch = std::env::temp_dir().join(format!("hl-native-vector-validation-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("create vector validation probe directory");
    let source = scratch.join("vector_validation.c");
    let executable = scratch.join("vector_validation");
    fs::write(
        &source,
        r#"#include "linux_abi/syscall/binding/vector_validation.h"
int main(void) {
    uint64_t total = 0;
    if (hl_guest_iov_validate(0x1000, 4096, &total) != 0 || total != 4096) return 1;
    if (hl_guest_iov_validate(0x2000, (uint64_t)INT64_MAX, &total) != -EINVAL) return 2;
    total = 0;
    if (hl_guest_iov_validate(UINT64_MAX - 1, 4, &total) != -EFAULT) return 3;
    total = 0;
    if (hl_guest_iov_validate(UINT64_C(0x0000fffffffff000), 8192, &total) != -EFAULT) return 4;
    total = 0;
    if (hl_guest_iov_validate(0, (uint64_t)INT64_MAX + 1, &total) != -EINVAL) return 5;
    return 0;
}
"#,
    )
    .expect("write vector validation probe source");
    let compile = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(format!("-I{}", native.display()))
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("vector validation probe compiler");
    assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));
    let run = Command::new(&executable)
        .status()
        .expect("vector validation probe execution");
    assert!(run.success(), "vector validation probe failed with {run}");
    fs::remove_dir_all(scratch).expect("remove vector validation probe directory");
}
