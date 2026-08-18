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
#include <signal.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>
#if defined(__APPLE__)
#include <mach/mach.h>
#include <mach/mach_vm.h>
#endif

#ifndef MAP_FIXED_NOREPLACE
#define MAP_FIXED_NOREPLACE 0x100000
#endif

#define NORMAL_BASE UINT64_C(0x50000000000)
#define NORMAL_LIMIT UINT64_C(0x50001000000)
#define LOW_BASE UINT64_C(0x40000000)
#define LOW_LIMIT UINT64_C(0x41000000)
#define NORMAL2_BASE UINT64_C(0x60000000000)
#define NORMAL2_LIMIT UINT64_C(0x60001000000)
#define LOW2_BASE UINT64_C(0x42000000)
#define LOW2_LIMIT UINT64_C(0x43000000)
#define NORMAL3_BASE UINT64_C(0x70000000000)
#define NORMAL3_LIMIT UINT64_C(0x70001000000)
#define LOW3_BASE UINT64_C(0x44000000)
#define LOW3_LIMIT UINT64_C(0x45000000)
#define NORMAL4_BASE UINT64_C(0x80000000000)
#define NORMAL4_LIMIT UINT64_C(0x80001000000)
#define LOW4_BASE UINT64_C(0x46000000)
#define LOW4_LIMIT UINT64_C(0x47000000)
#define NORMAL5_BASE UINT64_C(0x90000000000)
#define NORMAL5_LIMIT UINT64_C(0x90001000000)
#define LOW5_BASE UINT64_C(0x48000000)
#define LOW5_LIMIT UINT64_C(0x49000000)

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

static int write_faults(unsigned char *address) {
    pid_t child = fork();
    if (child < 0) return 0;
    if (child == 0) {
        *address = 0x7b;
        _exit(0);
    }
    int status = 0;
    return waitpid(child, &status, 0) == child && WIFSIGNALED(status) &&
           (WTERMSIG(status) == SIGSEGV || WTERMSIG(status) == SIGBUS);
}

static int range_is_claimed(uint64_t address, uint64_t length) {
#if defined(__APPLE__)
    mach_vm_address_t region = (mach_vm_address_t)address;
    mach_vm_size_t size = 0;
    vm_region_basic_info_data_64_t information;
    mach_msg_type_number_t count = VM_REGION_BASIC_INFO_COUNT_64;
    mach_port_t object = MACH_PORT_NULL;
    kern_return_t status = mach_vm_region(mach_task_self(), &region, &size, VM_REGION_BASIC_INFO_64,
                                          (vm_region_info_t)&information, &count, &object);
    if (object != MACH_PORT_NULL) mach_port_deallocate(mach_task_self(), object);
    return status == KERN_SUCCESS && region <= address && size >= length && address - region <= size - length;
#else
    long page = sysconf(_SC_PAGESIZE);
    unsigned char resident[2] = {0};
    return page > 0 && length <= 2 * (uint64_t)page &&
           mincore((void *)(uintptr_t)address, (size_t)length, resident) == 0;
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
    const uint64_t granule = hl_arena_host_granule();
    const hl_arena_config config = {granule, NORMAL_BASE, NORMAL_LIMIT, LOW_BASE, LOW_LIMIT};
    const hl_arena_config config2 = {granule, NORMAL2_BASE, NORMAL2_LIMIT, LOW2_BASE, LOW2_LIMIT};
    const hl_arena_config config3 = {granule, NORMAL3_BASE, NORMAL3_LIMIT, LOW3_BASE, LOW3_LIMIT};
    const hl_arena_config config4 = {granule, NORMAL4_BASE, NORMAL4_LIMIT, LOW4_BASE, LOW4_LIMIT};
    const hl_arena_config config5 = {granule, NORMAL5_BASE, NORMAL5_LIMIT, LOW5_BASE, LOW5_LIMIT};
    hl_arena_config invalid;
    hl_arena_authority authority = HL_ARENA_AUTHORITY_INIT;
    hl_arena_authority second = HL_ARENA_AUTHORITY_INIT;
    hl_arena_authority third = HL_ARENA_AUTHORITY_INIT;
    hl_arena_authority exhausted = HL_ARENA_AUTHORITY_INIT;
    hl_arena_authority failed = HL_ARENA_AUTHORITY_INIT;
    hl_arena_transaction transaction;
    hl_arena_fork_context fork_context;
    hl_arena_reservation first;
    hl_arena_reservation low;
    hl_arena_reservation forged;
    hl_arena_manifest before;
    hl_arena_manifest after;
    pthread_t thread;
    contender state = {0};

    if (granule == 0 || granule > UINT64_C(0x10000)) return 1;
    invalid = config;
    invalid.granule = granule / 2;
    if (hl_arena_authority_init(&authority, &invalid) == 0 || errno != EINVAL) return 42;
    invalid = config;
    invalid.normal_base++;
    if (hl_arena_authority_init(&authority, &invalid) == 0 || errno != EINVAL) return 43;
    invalid = config;
    invalid.low32_limit = UINT64_C(0x100000000) + granule;
    if (hl_arena_authority_init(&authority, &invalid) == 0 || errno != EINVAL) return 44;
    unsigned char *sentinel = claim_sentinel(NORMAL_BASE, granule);
    if (sentinel == NULL) return 1;
    memset(sentinel, 0xa5, (size_t)granule);
    if (hl_arena_authority_init(&authority, &config) == 0) return 2;
    for (uint64_t index = 0; index < granule; ++index)
        if (sentinel[index] != 0xa5) return 3;
    if (release_sentinel(sentinel, granule) != 0) return 4;

    sentinel = claim_sentinel(LOW_BASE, granule);
    if (sentinel == NULL) return 35;
    memset(sentinel, 0x5a, (size_t)granule);
    if (hl_arena_authority_init(&authority, &config) == 0) return 36;
    for (uint64_t index = 0; index < granule; ++index)
        if (sentinel[index] != 0x5a) return 37;
    unsigned char *normal = claim_sentinel(NORMAL_BASE, granule);
    if (normal == NULL) return 38;
    if (release_sentinel(normal, granule) != 0 || release_sentinel(sentinel, granule) != 0) return 39;

    if (hl_arena_authority_init(&authority, &config) != 0) return 5;
    if (hl_arena_manifest_get(&authority, &before) != 0 || before.granule != granule ||
        before.version != HL_ARENA_MANIFEST_VERSION) return 6;
    if (hl_arena_authority_init(&authority, &config) != -1 || errno != EALREADY ||
        hl_arena_manifest_get(&authority, &after) != 0 || memcmp(&before, &after, sizeof(before)) != 0) return 51;
    if (hl_arena_transaction_begin(&authority, &transaction) != 0) return 7;
    if (hl_arena_transaction_reserve(&transaction, HL_ARENA_NORMAL, 1, &first) != 0 ||
        first.address != NORMAL_BASE || first.length != granule ||
        hl_arena_reservation_owned(&authority, &first)) return 8;
    if (hl_arena_transaction_reserve(&transaction, HL_ARENA_LOW32, granule + 1, &low) != 0 ||
        low.address != LOW_BASE || low.length != 2 * granule || low.address + low.length > UINT64_C(0x100000000)) return 9;
    forged = first;
    forged.identity++;
    if (hl_arena_reservation_owned(&authority, &forged)) return 10;
    if (hl_arena_transaction_materialize_anonymous(&transaction, &first, 0) != -1 || errno != EINVAL ||
        hl_arena_transaction_materialize_anonymous(&transaction, &first, HL_ARENA_PROTECTION_WRITE) != -1 ||
        errno != EINVAL || hl_arena_transaction_materialize_anonymous(&transaction, &first, UINT32_MAX) != -1 ||
        errno != EINVAL) return 82;
    if (hl_arena_transaction_materialize_anonymous(
            &transaction, &first, HL_ARENA_PROTECTION_READ | HL_ARENA_PROTECTION_WRITE) != 0) return 71;
    unsigned char *materialized = (unsigned char *)(uintptr_t)first.address;
    memset(materialized, 0x3c, (size_t)first.length);
    if (hl_arena_transaction_materialize_anonymous(
            &transaction, &first, HL_ARENA_PROTECTION_READ | HL_ARENA_PROTECTION_WRITE) != -1 ||
        errno != EALREADY || materialized[0] != 0x3c) return 72;
    if (hl_arena_transaction_materialize_anonymous(
            &transaction, &forged, HL_ARENA_PROTECTION_READ | HL_ARENA_PROTECTION_WRITE) != -1 ||
        errno != EACCES || materialized[0] != 0x3c) return 73;
    sentinel = claim_sentinel(NORMAL_LIMIT, granule);
    if (sentinel == NULL) return 74;
    memset(sentinel, 0xa7, (size_t)granule);
    forged = first;
    forged.address = NORMAL_LIMIT;
    if (hl_arena_transaction_materialize_anonymous(
            &transaction, &forged, HL_ARENA_PROTECTION_READ | HL_ARENA_PROTECTION_WRITE) != -1 ||
        errno != EACCES || sentinel[0] != 0xa7 || sentinel[granule - 1] != 0xa7) return 75;
    if (release_sentinel(sentinel, granule) != 0) return 76;
    if (hl_arena_transaction_materialize_anonymous(&transaction, &low, HL_ARENA_PROTECTION_READ) != 0 ||
        *(const unsigned char *)(uintptr_t)low.address != 0) return 83;
    if (hl_arena_authority_fork_prepare(&authority) != -1 || errno != EBUSY) return 77;

    state.authority = &authority;
    if (pthread_create(&thread, NULL, contend, &state) != 0 || pthread_join(thread, NULL) != 0) return 11;
    if (state.result != -1 || state.error != EBUSY) return 12;

    if (hl_arena_transaction_rollback(&transaction) != 0 || !write_faults(materialized) ||
        !write_faults((unsigned char *)(uintptr_t)low.address)) return 78;
    if (hl_arena_authority_fork_prepare(&authority) != 0 ||
        hl_arena_fork_context_prepare(&fork_context) != 0) return 91;
    pid_t rollback_child = fork();
    if (rollback_child < 0) return 92;
    if (rollback_child == 0) {
        if (hl_arena_after_fork_child(&fork_context) != 0 || hl_arena_authority_fork_child(&authority) != 0 ||
            !range_is_claimed(first.address, first.length) || !range_is_claimed(low.address, low.length))
            _exit(93);
        _exit(0);
    }
    if (hl_arena_authority_fork_parent(&authority) != 0 || hl_arena_fork_context_parent(&fork_context) != 0) return 94;
    int rollback_status = 0;
    if (waitpid(rollback_child, &rollback_status, 0) != rollback_child || !WIFEXITED(rollback_status) ||
        WEXITSTATUS(rollback_status) != 0)
        return 95;
    if (hl_arena_manifest_get(&authority, &after) != 0 || after.normal_cursor != before.normal_cursor ||
        after.low32_cursor != before.low32_cursor || hl_arena_reservation_owned(&authority, &first)) return 13;

    if (hl_arena_transaction_begin(&authority, &transaction) != 0 ||
        hl_arena_transaction_reserve(&transaction, HL_ARENA_NORMAL, granule, &first) != 0 ||
        hl_arena_transaction_reserve(&transaction, HL_ARENA_LOW32, granule, &low) != 0 ||
        hl_arena_transaction_materialize_anonymous(
            &transaction, &first, HL_ARENA_PROTECTION_READ | HL_ARENA_PROTECTION_WRITE) != 0 ||
        hl_arena_transaction_materialize_anonymous(
            &transaction, &low, HL_ARENA_PROTECTION_READ | HL_ARENA_PROTECTION_EXECUTE) != 0)
        return 14;
    materialized = (unsigned char *)(uintptr_t)first.address;
    materialized[0] = 0xd4;
    if (hl_arena_transaction_commit(&transaction) != 0 || !hl_arena_reservation_owned(&authority, &first) ||
        materialized[0] != 0xd4) return 79;
    if (hl_arena_transaction_begin(&authority, &transaction) != 0) return 80;
    if (hl_arena_transaction_materialize_anonymous(
            &transaction, &first, HL_ARENA_PROTECTION_READ | HL_ARENA_PROTECTION_WRITE) != -1 ||
        errno != EALREADY || hl_arena_transaction_rollback(&transaction) != 0 || materialized[0] != 0xd4)
        return 81;
    if (hl_arena_authority_fork_prepare(&authority) != 0 ||
        hl_arena_fork_context_prepare(&fork_context) != 0) return 84;
    pid_t materialized_child = fork();
    if (materialized_child < 0) return 85;
    if (materialized_child == 0) {
        if (hl_arena_after_fork_child(&fork_context) != 0 || hl_arena_authority_fork_child(&authority) != 0 ||
            materialized[0] != 0xd4 || *(const unsigned char *)(uintptr_t)low.address != 0)
            _exit(86);
        materialized[0] = 0xe5;
        _exit(0);
    }
    if (hl_arena_authority_fork_parent(&authority) != 0 || hl_arena_fork_context_parent(&fork_context) != 0) return 87;
    int materialized_status = 0;
    if (waitpid(materialized_child, &materialized_status, 0) != materialized_child ||
        !WIFEXITED(materialized_status) || WEXITSTATUS(materialized_status) != 0 || materialized[0] != 0xd4)
        return 88;
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
    if (hl_arena_persisted_state_get(&authority, &persisted) != 0) return 19;
    persisted.manifest.reserved = 1;
    seal(&persisted);
    if (hl_arena_persisted_state_valid(&persisted)) return 20;
    if (hl_arena_persisted_state_get(&authority, &persisted) != 0) return 21;
    persisted.reservations[HL_ARENA_MAX_RESERVATIONS - 1].identity = 1;
    seal(&persisted);
    if (hl_arena_persisted_state_valid(&persisted)) return 22;

    if (hl_arena_transaction_begin(&authority, &transaction) != 0 ||
        hl_arena_transaction_reserve(&transaction, HL_ARENA_NORMAL, granule, &low) != 0) return 23;
    if (hl_arena_authority_destroy(&authority) != -1 || errno != EBUSY) return 24;
    int nonce_pipe[2];
    if (pipe(nonce_pipe) != 0) return 53;
    if (hl_arena_authority_fork_prepare(&authority) != 0) return 49;
    alarm(5);
    if (hl_arena_authority_fork_prepare(&authority) != -1 || errno != EALREADY) return 68;
    alarm(0);
    if (hl_arena_fork_context_prepare(&fork_context) != 0) return 69;
    pid_t child = fork();
    if (child < 0) return 25;
    if (child == 0) {
        close(nonce_pipe[0]);
        if (hl_arena_after_fork_child(&fork_context) != 0 ||
            hl_arena_authority_fork_child(&authority) != 0) _exit(26);
        if (hl_arena_transaction_commit(&transaction) == 0) _exit(27);
        if (hl_arena_reservation_owned(&authority, &low)) _exit(28);
        if (hl_arena_reservation_owned(&authority, &first) != 1) _exit(29);
        if (hl_arena_authority_destroy(&authority) != 0) _exit(30);
        hl_arena_authority child_authority = HL_ARENA_AUTHORITY_INIT;
        hl_arena_manifest child_manifest;
        if (hl_arena_authority_init(&child_authority, &config2) != 0 ||
            hl_arena_manifest_get(&child_authority, &child_manifest) != 0 ||
            write(nonce_pipe[1], &child_manifest.authority_nonce, sizeof(child_manifest.authority_nonce)) !=
                (ssize_t)sizeof(child_manifest.authority_nonce)) _exit(54);
        _exit(0);
    }
    close(nonce_pipe[1]);
    if (hl_arena_authority_fork_parent(&authority) != 0 ||
        hl_arena_fork_context_parent(&fork_context) != 0) return 52;
    int child_status = 0;
    if (waitpid(child, &child_status, 0) != child || !WIFEXITED(child_status) || WEXITSTATUS(child_status) != 0)
        return 31;
    uint64_t child_nonce = 0;
    if (read(nonce_pipe[0], &child_nonce, sizeof(child_nonce)) != (ssize_t)sizeof(child_nonce) || child_nonce == 0)
        return 55;
    if (child_nonce != fork_context.child_nonce) return 70;
    close(nonce_pipe[0]);
    hl_arena_transaction_rollback(&transaction);
    hl_arena_test_generation(&authority, UINT64_MAX - 1);
    if (hl_arena_transaction_begin(&authority, &transaction) != 0 ||
        hl_arena_transaction_reserve(&transaction, HL_ARENA_LOW32, granule, &low) != 0 ||
        hl_arena_authority_fork_prepare(&authority) != 0 ||
        hl_arena_fork_context_prepare(&fork_context) != 0) return 62;
    child = fork();
    if (child < 0) return 63;
    if (child == 0) {
        alarm(5);
        if (hl_arena_after_fork_child(&fork_context) != 0 ||
            hl_arena_authority_fork_child(&authority) != -1 || errno != EOVERFLOW) _exit(64);
        if (hl_arena_reservation_owned(&authority, &low)) _exit(65);
        if (hl_arena_authority_destroy(&authority) != 0) _exit(66);
        _exit(0);
    }
    if (hl_arena_authority_fork_parent(&authority) != 0 ||
        hl_arena_fork_context_parent(&fork_context) != 0 || waitpid(child, &child_status, 0) != child ||
        !WIFEXITED(child_status) || WEXITSTATUS(child_status) != 0) return 67;
    hl_arena_transaction_rollback(&transaction);
    if (hl_arena_authority_destroy(&authority) != 0) return 32;

    sentinel = claim_sentinel(NORMAL_BASE, NORMAL_LIMIT - NORMAL_BASE);
    if (sentinel != NULL || errno != EEXIST) return 33;
    normal = claim_sentinel(LOW_BASE, LOW_LIMIT - LOW_BASE);
    if (normal != NULL || errno != EEXIST) return 34;
    if (hl_arena_authority_init(&authority, &config) != -1 || errno != EALREADY) return 40;

    if (hl_arena_authority_init(&second, &config2) != 0 ||
        hl_arena_manifest_get(&second, &after) != 0 || after.authority_nonce == child_nonce ||
        hl_arena_reservation_owned(&second, &first) ||
        hl_arena_transaction_begin(&second, &transaction) != 0) return 45;
    for (uint32_t index = 0; index < HL_ARENA_MAX_RESERVATIONS; ++index)
        if (hl_arena_transaction_reserve(&transaction, HL_ARENA_NORMAL, 1, &low) != 0) return 46;
    if (hl_arena_transaction_reserve(&transaction, HL_ARENA_NORMAL, 1, &low) != -1 || errno != ENOSPC) return 47;
    if (hl_arena_transaction_commit(&transaction) != 0 ||
        hl_arena_persisted_state_get(&second, &persisted) != 0 ||
        persisted.manifest.reservation_count != HL_ARENA_MAX_RESERVATIONS ||
        !hl_arena_persisted_state_valid(&persisted)) return 50;
    if (hl_arena_authority_destroy(&second) != 0) return 48;

    if (hl_arena_authority_init(&failed, &config5) != 0 ||
        hl_arena_transaction_begin(&failed, &transaction) != 0 ||
        hl_arena_transaction_reserve(&transaction, HL_ARENA_NORMAL, granule, &first) != 0 ||
        hl_arena_transaction_materialize_anonymous(
            &transaction, &first, HL_ARENA_PROTECTION_READ | HL_ARENA_PROTECTION_WRITE) != 0)
        return 89;
    hl_arena_test_fail_next_placeholder_restore();
    if (hl_arena_transaction_rollback(&transaction) != -1 || errno != EIO ||
        hl_arena_manifest_get(&failed, &after) != -1 || errno != EINVAL ||
        hl_arena_authority_destroy(&failed) != -1 || errno != EINVAL)
        return 90;
    hl_arena_test_identity_sequence(UINT64_MAX - 1);
    if (hl_arena_authority_init(&third, &config3) != 0 || hl_arena_reservation_owned(&third, &first)) return 56;
    if (hl_arena_authority_init(&exhausted, &config4) != -1 || errno != EOVERFLOW) return 57;
    if (hl_arena_authority_init(&exhausted, &config4) != -1 || errno != EOVERFLOW) return 58;
    sentinel = claim_sentinel(NORMAL4_BASE, NORMAL4_LIMIT - NORMAL4_BASE);
    normal = claim_sentinel(LOW4_BASE, LOW4_LIMIT - LOW4_BASE);
    if (sentinel == NULL || normal == NULL) return 59;
    if (release_sentinel(sentinel, NORMAL4_LIMIT - NORMAL4_BASE) != 0 ||
        release_sentinel(normal, LOW4_LIMIT - LOW4_BASE) != 0) return 60;
    return hl_arena_authority_destroy(&third) == 0 ? 0 : 61;
}
"#,
    )
    .expect("write arena authority probe");
    let output = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args([
            "-std=c11",
            "-D_GNU_SOURCE",
            "-DHL_NATIVE_TEST_HOOKS",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-pthread",
        ])
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
