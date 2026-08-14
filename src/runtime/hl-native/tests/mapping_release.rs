use std::{fs, path::PathBuf, process::Command};

#[test]
fn mapping_release_failures_preserve_exact_retryable_ownership() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let scratch = std::env::temp_dir().join(format!("hl-native-mapping-release-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("create mapping release probe directory");
    let source = scratch.join("mapping_release.c");
    let executable = scratch.join("mapping_release");
    fs::write(
        &source,
        r#"
#include "host/range.h"

typedef struct fake_unmapper {
    unsigned calls;
    unsigned fail_at;
    uintptr_t address[8];
    size_t size[8];
} fake_unmapper;

static int fake_unmap(void *context, void *address, size_t size) {
    fake_unmapper *fake = context;
    unsigned call = fake->calls++;
    fake->address[call] = (uintptr_t)address;
    fake->size[call] = size;
    return call == fake->fail_at ? -1 : 0;
}

int main(void) {
    void *writable = (void *)(uintptr_t)0x10000;
    void *executable = (void *)(uintptr_t)0x20000;
    hl_host_hole_set holes = {0};
    fake_unmapper fake = {.fail_at = 0};

    /* An executable-alias failure cannot disturb writable ownership. */
    if (hl_host_mapping_release(&writable, &executable, 0x4000, &holes, fake_unmap, &fake) == 0) return 1;
    if (writable != (void *)(uintptr_t)0x10000 || executable != (void *)(uintptr_t)0x20000) return 2;
    if (fake.calls != 1 || fake.address[0] != 0x20000 || fake.size[0] != 0x4000) return 3;

    /* Once the alias is gone, a writable failure leaves that fact recorded and retry skips it. */
    fake = (fake_unmapper){.fail_at = 1};
    if (hl_host_mapping_release(&writable, &executable, 0x4000, &holes, fake_unmap, &fake) == 0) return 4;
    if (writable != (void *)(uintptr_t)0x10000 || executable != NULL) return 5;
    if (fake.calls != 2 || fake.address[0] != 0x20000 || fake.address[1] != 0x10000) return 6;
    fake = (fake_unmapper){.fail_at = 7};
    if (hl_host_mapping_release(&writable, &executable, 0x4000, &holes, fake_unmap, &fake) != 0) return 7;
    if (writable != NULL || executable != NULL || fake.calls != 1 || fake.address[0] != 0x10000) return 8;

    /* A later writable failure commits only the earlier accepted ranges, without allocation. */
    writable = (void *)(uintptr_t)0x30000;
    executable = (void *)(uintptr_t)0x40000;
    if (!hl_host_hole_set_retire(&holes, 0x1000, 0x1000)) return 9;
    if (!hl_host_hole_set_retire(&holes, 0x3000, 0x1000)) return 10;
    fake = (fake_unmapper){.fail_at = 2};
    if (hl_host_mapping_release(&writable, &executable, 0x5000, &holes, fake_unmap, &fake) == 0) return 11;
    if (executable != NULL || writable != (void *)(uintptr_t)0x30000) return 12;
    if (fake.calls != 3 || fake.address[0] != 0x40000 || fake.address[1] != 0x30000 ||
        fake.address[2] != 0x32000) return 13;
    if (holes.count != 2 || holes.entries[0].offset != 0 || holes.entries[0].size != 0x2000) return 14;
    fake = (fake_unmapper){.fail_at = 7};
    if (hl_host_mapping_release(&writable, &executable, 0x5000, &holes, fake_unmap, &fake) != 0) return 15;
    if (writable != NULL || executable != NULL || fake.calls != 2 || fake.address[0] != 0x32000 ||
        fake.address[1] != 0x34000) return 16;
    hl_host_hole_set_release(&holes);
    return 0;
}
"#,
    )
    .expect("write mapping release probe source");
    let compile = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(format!("-I{}", native.display()))
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("mapping release probe compiler");
    assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));
    let run = Command::new(&executable)
        .status()
        .expect("mapping release probe execution");
    assert!(run.success(), "mapping release probe failed with {run}");
    fs::remove_dir_all(scratch).expect("remove mapping release probe directory");
}
