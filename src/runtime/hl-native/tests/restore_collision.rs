use std::{fs, path::Path};

#[cfg(target_os = "linux")]
use std::process::Command;

#[test]
fn actual_restore_claim_helper_fails_closed_on_both_isas() {
    for isa in [1, 2] {
        for scenario in [0, 1, 2] {
            hl_native::checkpoint_restore_claim_test(isa, scenario)
                .unwrap_or_else(|status| panic!("ISA {isa} restore claim scenario {scenario} failed at {status}"));
        }
    }
}

#[test]
fn actual_restore_rollback_unwinds_every_published_class_on_both_isas() {
    for isa in [1, 2] {
        hl_native::checkpoint_restore_rollback_test(isa)
            .unwrap_or_else(|status| panic!("ISA {isa} restore rollback failed at {status}"));
    }
}

#[test]
fn restore_collision_fails_closed_on_every_host() {
    let native = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/native");
    let restore = fs::read_to_string(native.join("linux_abi/checkpoint/memory_restore.c"))
        .expect("read direct memory restore implementation");
    assert!(restore.contains("mach_vm_allocate(mach_task_self(), &reserved"));
    assert!(restore.contains("VM_FLAGS_FIXED"));
    assert!(!restore.contains("VM_FLAGS_FIXED | VM_FLAGS_OVERWRITE"));
    assert!(restore.contains("(flags & ~MAP_FIXED) | MAP_FIXED_NOREPLACE"));
    assert!(!restore.contains("map_flags | MAP_FIXED_NOREPLACE"));
    assert!(!restore.contains("#ifdef MAP_FIXED_NOREPLACE"));
    assert!(!restore.contains("reclaiming it"));

    let host_mman = fs::read_to_string(native.join("linux_abi/host_mman.h")).expect("read portable mmap seam");
    assert!(host_mman.contains("#define MAP_FIXED_NOREPLACE 0x100000"));

    let darwin = fs::read_to_string(native.join("host/macos/memory/mapping.c"))
        .expect("read Darwin exact reservation implementation");
    assert!(darwin.contains("mach_vm_allocate(mach_task_self(), &reserved, (mach_vm_size_t)size, VM_FLAGS_FIXED)"));
    assert!(!darwin.contains("VM_FLAGS_FIXED | VM_FLAGS_OVERWRITE"));

    let windows = fs::read_to_string(native.join("host/windows/memory.c"))
        .expect("read Windows exact reservation implementation");
    assert!(windows.contains("HL_HOST_MEMORY_FIXED_NOREPLACE"));
    assert!(windows.contains("MEM_RESERVE | MEM_RESERVE_PLACEHOLDER"));
}

#[cfg(target_os = "linux")]
#[test]
fn fixed_noreplace_collision_preserves_sentinel_bytes() {
    let scratch = tempfile::tempdir().expect("create restore collision probe directory");
    let source = scratch.path().join("restore_collision.c");
    let executable = scratch.path().join("restore_collision");
    fs::write(
        &source,
        r#"
#define _GNU_SOURCE
#include <errno.h>
#include <stdint.h>
#include <string.h>
#include <sys/mman.h>

int main(void) {
    size_t length = 4096;
    unsigned char *sentinel = mmap(NULL, length, PROT_READ | PROT_WRITE,
                                   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (sentinel == MAP_FAILED) return 1;
    memset(sentinel, 0xa5, length);
    void *collision = mmap(sentinel, length, PROT_READ | PROT_WRITE,
                           MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE, -1, 0);
    if (collision != MAP_FAILED || errno != EEXIST) return 2;
    for (size_t index = 0; index < length; ++index)
        if (sentinel[index] != 0xa5) return 3;
    return munmap(sentinel, length) == 0 ? 0 : 4;
}
"#,
    )
    .expect("write restore collision probe");
    let compile = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("compile restore collision probe");
    assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));
    let run = Command::new(executable).status().expect("run restore collision probe");
    assert!(run.success(), "restore collision probe failed with {run}");
}
