use std::{fs, path::PathBuf, process::Command};

#[test]
fn owner_registry_is_generational_shared_and_bounded() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let scratch = std::env::temp_dir().join(format!("hl-owner-registry-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("owner-registry probe directory");
    let source = scratch.join("probe.c");
    let executable = scratch.join("probe");
    fs::write(
        &source,
        r#"
#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>

#include "linux_abi/container/ownership/registry.c"

struct shared_control {
    _Atomic uint64_t generation;
    _Atomic uint64_t writer_owner;
};
static struct shared_control *control;
static hl_owner_namespace namespace;
static hl_owner_registry *registry;
static size_t registry_bytes;

static hl_owner_key key(uint64_t value) {
    return (hl_owner_key){.device = 7, .object = value + 1, .birth_ns = value * 17 + 3};
}

static hl_owner_writer begin(uint64_t owner) {
    uint64_t current = atomic_load_explicit(&control->generation, memory_order_relaxed);
    if (current & 1) abort();
    atomic_store_explicit(&control->writer_owner, owner, memory_order_relaxed);
    atomic_store_explicit(&control->generation, current + 1, memory_order_release);
    return (hl_owner_writer){.generation = current + 1, .identity = owner};
}

static void end(hl_owner_writer writer) {
    atomic_store_explicit(&control->writer_owner, 0, memory_order_relaxed);
    atomic_store_explicit(&control->generation, writer.generation + 1, memory_order_release);
}

static int create(uint64_t id, uint32_t uid, uint32_t gid, uint32_t links, uint32_t descriptors,
                  hl_owner_ticket *saved) {
    hl_owner_writer writer = begin(UINT64_C(0x1000) + id);
    hl_owner_ticket ticket;
    int status = hl_owner_registry_reserve(registry, namespace, writer, &ticket);
    if (status == 0)
        status = hl_owner_registry_commit(registry, namespace, writer, ticket, key(id),
                                          (hl_owner_value){uid, gid, links, descriptors});
    if (saved != NULL) *saved = ticket;
    end(writer);
    return status;
}

static int retire(uint64_t id) {
    hl_owner_writer writer = begin(UINT64_C(0x2000) + id);
    int status = hl_owner_registry_retire(registry, namespace, writer, key(id));
    end(writer);
    return status;
}

static _Atomic int stop_readers;
static _Atomic int reader_failure;

static void *reader(void *unused) {
    (void)unused;
    while (!atomic_load_explicit(&stop_readers, memory_order_relaxed)) {
        hl_owner_value value;
        int status = hl_owner_registry_lookup(registry, namespace, key(42), &value);
        if (status == HL_OWNER_FOUND && !((value.uid == 10 && value.gid == 20) ||
                                          (value.uid == 30 && value.gid == 40)))
            atomic_store(&reader_failure, 1);
        else if (status != HL_OWNER_FOUND && status != -EOWNERDEAD && status != -EAGAIN)
            atomic_store(&reader_failure, 2);
    }
    return NULL;
}

int main(void) {
    const uint64_t capacity = 16384;
    registry_bytes = hl_owner_registry_size(capacity);
    registry = mmap(NULL, registry_bytes, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (registry == MAP_FAILED) return 1;
    control = mmap(NULL, sizeof *control, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (control == MAP_FAILED) return 2;
    namespace = (hl_owner_namespace){&control->generation, &control->writer_owner};
    if (hl_owner_registry_init(registry, registry_bytes, capacity, UINT64_C(0x8899)) != 0)
        return 2;
    if (hl_owner_registry_init(registry, registry_bytes, capacity, UINT64_C(0x99)) != EALREADY)
        return 3;
    hl_owner_ticket rejected;
    hl_owner_writer invalid_writer = {0, 1};
    if (hl_owner_registry_reserve(registry, (hl_owner_namespace){0}, invalid_writer, &rejected) != EPERM ||
        hl_owner_registry_reserve(registry, namespace, invalid_writer, &rejected) != EPERM)
        return 49;
    hl_owner_writer validation_writer = begin(66);
    if (hl_owner_registry_reserve(registry, namespace,
                                  (hl_owner_writer){validation_writer.generation - 2u, validation_writer.identity},
                                  &rejected) != EPERM ||
        hl_owner_registry_reserve(registry, namespace,
                                  (hl_owner_writer){validation_writer.generation, validation_writer.identity + 1u},
                                  &rejected) != EPERM)
        return 50;
    end(validation_writer);

    /* Reuse tombstones through more than the retired implementation's entire 8192-slot capacity. */
    for (uint64_t id = 0; id < 40000; ++id) {
        if (create(id, (uint32_t)id, (uint32_t)(id + 1), 0, 0, NULL) != 0) return 4;
        if (retire(id) != 0) return 5;
    }
    if (atomic_load_explicit(&registry->occupied, memory_order_relaxed) != 0) return 46;

    /* A stale ticket cannot publish into a tombstone or its reused slot. */
    hl_owner_writer writer = begin(77);
    hl_owner_ticket stale;
    if (hl_owner_registry_reserve(registry, namespace, writer, &stale) != 0) return 6;
    if (hl_owner_registry_cancel(registry, namespace, writer, stale) != 0) return 7;
    hl_owner_ticket fresh;
    if (hl_owner_registry_reserve(registry, namespace, writer, &fresh) != 0) return 8;
    if (hl_owner_registry_commit(registry, namespace, writer, stale, key(90000),
                                 (hl_owner_value){1, 2, 0, 0}) != ESTALE)
        return 9;
    if (hl_owner_registry_commit(registry, namespace, writer, fresh, key(90001),
                                 (hl_owner_value){3, 4, 0, 0}) != 0)
        return 10;
    end(writer);

    /* Metadata is one atomic uid+gid snapshot under concurrent readers. */
    if (create(42, 10, 20, 1, 1, NULL) != 0) return 11;
    pthread_t threads[8];
    for (size_t i = 0; i < 8; ++i)
        if (pthread_create(&threads[i], NULL, reader, NULL) != 0) return 12;
    for (unsigned i = 0; i < 20000; ++i) {
        writer = begin(88);
        uint32_t uid = (i & 1) ? 10 : 30;
        uint32_t gid = (i & 1) ? 20 : 40;
        if (hl_owner_registry_update(registry, namespace, writer, key(42), uid, gid) != 0) return 13;
        end(writer);
    }
    atomic_store(&stop_readers, 1);
    for (size_t i = 0; i < 8; ++i) pthread_join(threads[i], NULL);
    if (atomic_load(&reader_failure) != 0) return 14;

    /* Reserve before direct-final bind, publish uid 70 from fstat, and retain open-unlinked ownership. */
    writer = begin(90);
    hl_owner_ticket socket_ticket;
    if (hl_owner_registry_reserve(registry, namespace, writer, &socket_ticket) != 0) return 23;
    int socket_fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (socket_fd < 0) return 24;
    struct sockaddr_un address;
    memset(&address, 0, sizeof address);
    address.sun_family = AF_UNIX;
    snprintf(address.sun_path, sizeof address.sun_path, "/tmp/hl-owner-%ld.sock", (long)getpid());
    unlink(address.sun_path);
    if (bind(socket_fd, (struct sockaddr *)&address, sizeof address) != 0) return 25;
    struct stat socket_status;
    if (lstat(address.sun_path, &socket_status) != 0 || !S_ISSOCK(socket_status.st_mode)) return 26;
    hl_owner_key socket_key = {(uint64_t)socket_status.st_dev, (uint64_t)socket_status.st_ino, 1};
    if (hl_owner_registry_commit(registry, namespace, writer, socket_ticket, socket_key,
                                 (hl_owner_value){70, 70, 1, 1}) != 0)
        return 27;
    end(writer);
    hl_owner_value socket_value;
    if (hl_owner_registry_lookup(registry, namespace, socket_key, &socket_value) != HL_OWNER_FOUND ||
        socket_value.uid != 70 || socket_value.gid != 70)
        return 28;
    if (chmod(address.sun_path, 0770) != 0 || lstat(address.sun_path, &socket_status) != 0 ||
        (socket_status.st_mode & 0777) != 0770)
        return 47;
    if (unlink(address.sun_path) != 0) return 29;
    writer = begin(91);
    if (hl_owner_registry_link(registry, namespace, writer, socket_key, -1) != 0) return 30;
    end(writer);
    if (hl_owner_registry_lookup(registry, namespace, socket_key, &socket_value) != HL_OWNER_FOUND ||
        socket_value.links != 0 || socket_value.descriptors != 1)
        return 31;
    close(socket_fd);
    writer = begin(92);
    if (hl_owner_registry_descriptor(registry, namespace, writer, socket_key, -1) != 0) return 32;
    end(writer);
    if (hl_owner_registry_lookup(registry, namespace, socket_key, &socket_value) != HL_OWNER_ABSENT) return 33;

    /* Collision chains survive deletion; collision commit cancels quota; reserve preflights publication sequence. */
    size_t small_bytes = hl_owner_registry_size(8);
    hl_owner_registry *small = mmap(NULL, small_bytes, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (small == MAP_FAILED || hl_owner_registry_init(small, small_bytes, 8, UINT64_C(0x4455)) != 0) return 38;
    hl_owner_key first = key(500000), collision = key(500001);
    while ((hl_owner_hash(first) & 7u) != (hl_owner_hash(collision) & 7u)) collision.object++;
    writer = begin(93);
    hl_owner_ticket first_ticket, collision_ticket, duplicate_ticket;
    if (hl_owner_registry_reserve(small, namespace, writer, &first_ticket) != 0 ||
        hl_owner_registry_commit(small, namespace, writer, first_ticket, first, (hl_owner_value){1, 2, 0, 0}) != 0 ||
        hl_owner_registry_reserve(small, namespace, writer, &collision_ticket) != 0 ||
        hl_owner_registry_commit(small, namespace, writer, collision_ticket, collision,
                                 (hl_owner_value){3, 4, 0, 0}) != 0 ||
        hl_owner_registry_retire(small, namespace, writer, first) != 0)
        return 39;
    if (hl_owner_registry_lookup(small, namespace, collision, &socket_value) != -EAGAIN) return 40;
    end(writer);
    if (hl_owner_registry_lookup(small, namespace, collision, &socket_value) != HL_OWNER_FOUND ||
        socket_value.uid != 3)
        return 41;
    hl_owner_key reborn = collision;
    reborn.birth_ns++;
    writer = begin(941);
    hl_owner_ticket reborn_ticket;
    if (hl_owner_registry_reserve(small, namespace, writer, &reborn_ticket) != 0 ||
        hl_owner_registry_commit(small, namespace, writer, reborn_ticket, reborn,
                                 (hl_owner_value){5, 6, 0, 0}) != 0)
        return 51;
    end(writer);
    if (hl_owner_registry_lookup(small, namespace, collision, &socket_value) != HL_OWNER_FOUND ||
        socket_value.uid != 3 ||
        hl_owner_registry_lookup(small, namespace, reborn, &socket_value) != HL_OWNER_FOUND || socket_value.uid != 5)
        return 52;
    writer = begin(94);
    if (hl_owner_registry_reserve(small, namespace, writer, &duplicate_ticket) != 0 ||
        hl_owner_registry_commit(small, namespace, writer, duplicate_ticket, collision,
                                 (hl_owner_value){9, 9, 0, 0}) != EEXIST ||
        hl_owner_registry_cancel(small, namespace, writer, duplicate_ticket) != ESTALE ||
        atomic_load_explicit(&small->occupied, memory_order_relaxed) != 2)
        return 42;
    atomic_store_explicit(&small->next_sequence, HL_OWNER_STATE_SEQUENCE_MAX - 1u, memory_order_relaxed);
    hl_owner_ticket last;
    if (hl_owner_registry_reserve(small, namespace, writer, &last) != 0) return 43;
    hl_owner_ticket invalid = last;
    invalid.epoch++;
    if (hl_owner_registry_commit(small, namespace, writer, invalid, key(600000),
                                 (hl_owner_value){1, 1, 0, 0}) != ESTALE)
        return 44;
    invalid = last;
    invalid.sequence = UINT64_MAX;
    invalid.publication_sequence = 0;
    if (hl_owner_registry_commit(small, namespace, writer, invalid, key(600001),
                                 (hl_owner_value){1, 1, 0, 0}) != ESTALE ||
        hl_owner_registry_commit(small, namespace, writer, last, key(600000),
                                 (hl_owner_value){1, 1, 0, 0}) != 0 ||
        hl_owner_registry_reserve(small, namespace, writer, &last) != EOVERFLOW)
        return 44;
    if (hl_owner_registry_link(registry, namespace, writer, key(42), INT64_MIN) != ERANGE) return 45;
    end(writer);

    /* Admission is capped at 50%, but updating an existing key remains possible at quota. */
    uint64_t before_fill = atomic_load_explicit(&registry->occupied, memory_order_relaxed);
    if (before_fill >= capacity / 2) return 36;
    for (uint64_t id = 100000; id < 100000 + capacity / 2 - before_fill; ++id)
        if (create(id, 1, 2, 1, 0, NULL) != 0) return 15;
    if (atomic_load_explicit(&registry->occupied, memory_order_relaxed) != capacity / 2) return 37;
    writer = begin(99);
    hl_owner_ticket denied;
    if (hl_owner_registry_reserve(registry, namespace, writer, &denied) != ENOSPC) return 16;
    if (hl_owner_registry_update(registry, namespace, writer, key(100000), 55, 66) != 0) return 17;
    end(writer);
    hl_owner_value value;
    if (hl_owner_registry_lookup(registry, namespace, key(100000), &value) != HL_OWNER_FOUND || value.uid != 55 ||
        value.gid != 66)
        return 18;

    /* Shared mappings publish across fork. */
    pid_t child = fork();
    if (child == 0) {
        writer = begin(1234);
        int status = hl_owner_registry_update(registry, namespace, writer, key(100001), 70, 71);
        end(writer);
        _exit(status == 0 ? 0 : 1);
    }
    int wait_status;
    if (child < 0 || waitpid(child, &wait_status, 0) != child || !WIFEXITED(wait_status) || WEXITSTATUS(wait_status))
        return 19;
    if (hl_owner_registry_lookup(registry, namespace, key(100001), &value) != HL_OWNER_FOUND || value.uid != 70)
        return 20;

    writer = begin(3456);
    if (hl_owner_registry_link(registry, namespace, writer, key(100002), -1) != 0) return 34;
    end(writer);

    /* A writer killed while CLAIMING is detected and poisoned by the namespace transaction owner. */
    child = fork();
    if (child == 0) {
        writer = begin(5678);
        hl_owner_ticket abandoned;
        if (hl_owner_registry_reserve(registry, namespace, writer, &abandoned) != 0) _exit(1);
        kill(getpid(), SIGKILL);
        _exit(2);
    }
    if (child < 0 || waitpid(child, &wait_status, 0) != child || !WIFSIGNALED(wait_status)) return 21;
    if (hl_owner_registry_lookup(registry, namespace, key(100000), &value) != -EAGAIN) return 22;
    atomic_store_explicit(&control->generation, HL_OWNER_NAMESPACE_POISON, memory_order_release);
    if (hl_owner_registry_lookup(registry, namespace, key(100000), &value) != -EOWNERDEAD) return 35;
    return 0;
}
"#,
    )
    .expect("owner-registry probe source");
    let compile = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-pthread"])
        .arg(format!("-I{}", native.display()))
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("compile owner-registry probe");
    assert!(
        compile.status.success(),
        "owner-registry probe did not compile:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(&executable).output().expect("run owner-registry probe");
    assert!(
        run.status.success(),
        "owner-registry probe failed with {:?}:\n{}",
        run.status.code(),
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = fs::remove_dir_all(scratch);
}
