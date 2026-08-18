#![cfg(unix)]

use std::{fs, path::PathBuf, process::Command};

#[test]
fn os_owned_arenas_are_transactional_and_collision_safe() {
    let scratch = tempfile::tempdir().expect("create arena test directory");
    let source = scratch.path().join("arena_test.c");
    let executable = scratch.path().join("arena_test");
    let native = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/native");
    fs::write(
        &source,
        r#"
#include "engine/arena.h"

#include <errno.h>
#include <pthread.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <sys/mman.h>
#if defined(__APPLE__)
#include <mach/mach.h>
#include <mach/mach_vm.h>
#endif

#ifndef MAP_FIXED_NOREPLACE
#define MAP_FIXED_NOREPLACE 0x100000
#endif

#define NORMAL_BASE UINT64_C(0x50000000000)
#define NORMAL_LIMIT UINT64_C(0x50000040000)
#define LOW_BASE UINT64_C(0x40000000)
#define LOW_LIMIT UINT64_C(0x40040000)

static unsigned char *claim_sentinel(uint64_t address, uint64_t length) {
#if defined(__APPLE__)
    mach_vm_address_t claimed = (mach_vm_address_t)address;
    return mach_vm_allocate(mach_task_self(), &claimed, (mach_vm_size_t)length, VM_FLAGS_FIXED) == KERN_SUCCESS &&
                   claimed == address
               ? (unsigned char *)(uintptr_t)claimed
               : NULL;
#else
    void *claimed = mmap((void *)(uintptr_t)address, (size_t)length, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE, -1, 0);
    return claimed == (void *)(uintptr_t)address ? claimed : NULL;
#endif
}

static int release_sentinel(unsigned char *address, uint64_t length) {
#if defined(__APPLE__)
    return mach_vm_deallocate(mach_task_self(), (mach_vm_address_t)(uintptr_t)address, (mach_vm_size_t)length) ==
                   KERN_SUCCESS
               ? 0
               : -1;
#else
    return munmap(address, (size_t)length);
#endif
}

typedef struct contender {
    hl_arena_authority *authority;
    int result;
    int error;
} contender;

static void seal(hl_arena_persisted_state *state) {
    const unsigned char *bytes = (const unsigned char *)state;
    uint64_t checksum = UINT64_C(1469598103934665603);
    for (size_t index = 0; index < offsetof(hl_arena_persisted_state, checksum); ++index)
        checksum = (checksum ^ bytes[index]) * UINT64_C(1099511628211);
    state->checksum = checksum;
}

static void *contend(void *opaque) {
    contender *state = opaque;
    hl_arena_transaction transaction;
    state->result = hl_arena_transaction_begin(state->authority, &transaction);
    state->error = errno;
    if (state->result == 0) hl_arena_transaction_rollback(&transaction);
    return NULL;
}

int main(void) {
    const hl_arena_config config = {UINT64_C(0x4000), NORMAL_BASE, NORMAL_LIMIT, LOW_BASE, LOW_LIMIT};
    hl_arena_authority authority;
    hl_arena_transaction transaction;
    hl_arena_reservation first;
    hl_arena_reservation low;
    hl_arena_reservation forged;
    hl_arena_manifest before;
    hl_arena_manifest after;
    pthread_t thread;
    contender state = {0};

    unsigned char *sentinel = claim_sentinel(NORMAL_BASE, 0x4000);
    if (sentinel == NULL) return 1;
    memset(sentinel, 0xa5, 0x4000);
    if (hl_arena_authority_init(&authority, &config) == 0) return 2;
    for (unsigned index = 0; index < 0x4000; ++index)
        if (sentinel[index] != 0xa5) return 3;
    if (release_sentinel(sentinel, 0x4000) != 0) return 4;

    if (hl_arena_authority_init(&authority, &config) != 0) return 5;
    if (hl_arena_manifest_get(&authority, &before) != 0 || before.granule != 0x4000 ||
        before.version != HL_ARENA_MANIFEST_VERSION) return 6;
    if (hl_arena_transaction_begin(&authority, &transaction) != 0) return 7;
    if (hl_arena_transaction_reserve(&transaction, HL_ARENA_NORMAL, 1, &first) != 0 ||
        first.address != NORMAL_BASE || first.length != 0x4000 ||
        !hl_arena_reservation_owned(&authority, &first)) return 8;
    if (hl_arena_transaction_reserve(&transaction, HL_ARENA_LOW32, 0x4001, &low) != 0 ||
        low.address != LOW_BASE || low.length != 0x8000 || low.address + low.length > UINT64_C(0x100000000)) return 9;
    forged = first;
    forged.identity++;
    if (hl_arena_reservation_owned(&authority, &forged)) return 10;

    state.authority = &authority;
    if (pthread_create(&thread, NULL, contend, &state) != 0 || pthread_join(thread, NULL) != 0) return 11;
    if (state.result != -1 || state.error != EBUSY) return 12;

    hl_arena_transaction_rollback(&transaction);
    if (hl_arena_manifest_get(&authority, &after) != 0 || after.normal_cursor != before.normal_cursor ||
        after.low32_cursor != before.low32_cursor || hl_arena_reservation_owned(&authority, &first)) return 13;

    if (hl_arena_transaction_begin(&authority, &transaction) != 0 ||
        hl_arena_transaction_reserve(&transaction, HL_ARENA_NORMAL, 0x4000, &first) != 0 ||
        hl_arena_transaction_commit(&transaction) != 0 || !hl_arena_reservation_owned(&authority, &first)) return 14;
    hl_arena_persisted_state persisted;
    if (hl_arena_persisted_state_get(&authority, &persisted) != 0 ||
        !hl_arena_persisted_state_valid(&persisted)) return 15;
    persisted.manifest.next_identity = first.identity;
    seal(&persisted);
    if (hl_arena_persisted_state_valid(&persisted)) return 16;
    if (hl_arena_persisted_state_get(&authority, &persisted) != 0) return 17;
    persisted.manifest.normal_cursor += persisted.manifest.granule;
    seal(&persisted);
    if (hl_arena_persisted_state_valid(&persisted)) return 18;
    hl_arena_authority_destroy(&authority);

    sentinel = claim_sentinel(NORMAL_BASE, 0x4000);
    if (sentinel == NULL) return 19;
    return release_sentinel(sentinel, 0x4000) == 0 ? 0 : 20;
}
"#,
    )
    .expect("write arena authority probe");
    let output = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=c11", "-D_GNU_SOURCE", "-Wall", "-Wextra", "-Werror", "-pthread"])
        .arg(format!("-I{}", native.display()))
        .arg(&source)
        .arg(native.join("engine/arena.c"))
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("compile arena authority probe");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let status = Command::new(executable).status().expect("run arena authority probe");
    assert!(status.success(), "arena authority probe failed with {status}");
}
