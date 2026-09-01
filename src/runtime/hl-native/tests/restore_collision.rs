use std::{fs, path::Path};

#[cfg(target_os = "linux")]
use std::process::Command;

static NATIVE_FIXTURE: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn actual_restore_claim_helper_fails_closed_on_both_isas() {
    let _fixture = NATIVE_FIXTURE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for isa in [1, 2] {
        for scenario in [0, 1, 2] {
            hl_native::checkpoint_restore_claim_test(isa, scenario)
                .unwrap_or_else(|status| panic!("ISA {isa} restore claim scenario {scenario} failed at {status}"));
        }
    }
}

#[test]
fn descriptor_reset_scans_only_the_inherited_population_and_preserves_desired_pipes() {
    let _fixture = NATIVE_FIXTURE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for isa in [1, 2] {
        assert_eq!(hl_native::checkpoint_restore_fd_reset_test(isa, 0).unwrap(), 0);
        let inspected = hl_native::checkpoint_restore_fd_reset_test(isa, 1).unwrap();
        assert!(
            inspected > 0,
            "ISA {isa} did not inspect its populated descriptor prefix"
        );
        assert!(
            inspected < 2 * 65_536,
            "ISA {isa} scanned the whole descriptor table ({inspected} slots)"
        );
    }
}

#[test]
fn a_restore_created_process_reclaims_its_inherited_private_descriptors() {
    let _fixture = NATIVE_FIXTURE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let restore = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/native/linux_abi/checkpoint/socket_restore.c"),
    )
    .expect("read checkpoint process-tree restore implementation");
    assert!(
        restore.contains("pid_t p = ckpt_restore_clone_current(&private_status);"),
        "the process-tree restore bypasses the private-descriptor lifecycle helper"
    );
    for isa in [1, 2] {
        assert_eq!(
            hl_native::checkpoint_restore_fd_reset_test(isa, 2).unwrap(),
            1,
            "ISA {isa} restore fork did not publish private-descriptor ownership"
        );
    }
}

/// A rounded host claim must step over the host page a neighbouring guest region of the same image
/// already claimed, instead of colliding with this restore's own mapping. Reproduces the real
/// Apple Silicon addresses; on a 4 KiB host the two regions never share a page at all.
#[test]
fn a_rounded_claim_shares_a_host_page_with_its_neighbour_on_both_isas() {
    let _fixture = NATIVE_FIXTURE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for isa in [1, 2] {
        for scenario in [0, 1, 2, 3] {
            hl_native::checkpoint_restore_slice_test(isa, scenario)
                .unwrap_or_else(|status| panic!("ISA {isa} restore slice scenario {scenario} failed at {status}"));
        }
    }
}

#[test]
fn actual_restore_rollback_unwinds_every_published_class_on_both_isas() {
    let _fixture = NATIVE_FIXTURE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for isa in [1, 2] {
        hl_native::checkpoint_restore_rollback_test(isa)
            .unwrap_or_else(|status| panic!("ISA {isa} restore rollback failed at {status}"));
    }
}

#[test]
fn restore_collision_fails_closed_on_every_host() {
    let _fixture = NATIVE_FIXTURE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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
    let _fixture = NATIVE_FIXTURE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let scratch = tempfile::tempdir().expect("create restore collision probe directory");
    let source = scratch.path().join("restore_collision.c");
    let executable = scratch.path().join("restore_collision");
    fs::write(
        &source,
        r"
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
",
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

/// A re-forked restorer drops its parent's inherited address space before it claims its own image at the
/// captured guest addresses. The deterministic arena starts the brk heap one 4 KiB guard page above
/// `HL_LINUX_SNAPSHOT_BASE`, so on a 16 KiB host that range begins mid-page and Darwin's `munmap(2)` refuses
/// an unaligned address outright: the teardown released nothing, the parent's heap stayed live, and any
/// member whose own image named the same address failed its exact claim with `EEXIST`.
#[test]
fn a_registry_teardown_releases_the_host_pages_a_guest_range_occupies_on_both_isas() {
    let _fixture = NATIVE_FIXTURE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for isa in [1, 2] {
        for scenario in [0, 1] {
            claim_the_released_pages(isa, scenario);
        }
    }
}

/// `22` is the hook's "the exact reclaim was refused with `EEXIST`" verdict, and it now has exactly one
/// cause: the teardown released nothing, which is a property of the host's page granularity and so fails
/// every time. It used to have a second, which is why this called the hook up to eight times -- the probe
/// range came from the kernel's own allocator, so the hole the release opens sat at the top of the free
/// area and the next `mmap` on any other thread of this binary was handed it. Retrying cannot answer that:
/// each attempt re-opens the same hole in the same place. The hook takes its range from a fixed band the
/// top-down search does not reach, so one call is the whole measurement.
fn claim_the_released_pages(isa: u32, scenario: u32) {
    hl_native::checkpoint_gmap_release_test(isa, scenario).unwrap_or_else(|status| {
        panic!("ISA {isa} gmap release scenario {scenario} failed at {status}");
    });
}
