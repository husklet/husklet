#include "hl/engine.h"
#include "hl/linux_abi.h"
#include "backend.h"
#include "options.h"
#include "../host/system.h"

#include <stdlib.h>
#include <stdbool.h>
#include <stdatomic.h>
#include <stddef.h>
#include <stdio.h>
#include <signal.h>
#include <string.h>
#include <fcntl.h>
#include <unistd.h>
#if !defined(_WIN32)
#include <pthread.h>
#include <sys/stat.h>
#include <sys/wait.h>
#endif

#define HL_ENGINE_REQUEST_CHECKPOINT_PRIVATE 4u
#if !defined(_WIN32)
#include <sys/socket.h>
#include <errno.h>
#include <poll.h>
#endif

/* One process may embed every guest translator.  Keep registration keyed by
 * guest ISA: a constructor for one backend must never overwrite another. */
static const hl_engine_backend *production_backends[HL_GUEST_ISA_X86_64 + 1];

#if defined(HL_NATIVE_TEST_HOOKS)
static atomic_uint engine_finish_test_phase;

HL_API uint32_t hl_c_backend_engine_finish_test_arm(void) {
    atomic_store_explicit(&engine_finish_test_phase, 1, memory_order_release);
    return 1;
}

HL_API uint32_t hl_c_backend_engine_finish_test_phase(void) {
    return atomic_load_explicit(&engine_finish_test_phase, memory_order_acquire);
}

HL_API void hl_c_backend_engine_finish_test_release(void) {
    atomic_store_explicit(&engine_finish_test_phase, 3, memory_order_release);
}
#endif

void hl_engine_backend_register(const hl_engine_backend *backend) {
    if (backend == NULL || backend->guest_isa > HL_GUEST_ISA_X86_64) return;
    production_backends[backend->guest_isa] = backend;
}

struct hl_engine {
    hl_engine_config config;
    hl_host_services host;
    const hl_engine_backend *backend;
    atomic_flag lock;
    atomic_bool checkpoint_control_lock;
    hl_host_handle process;
    hl_host_handle process_result;
    uint32_t state;
    uint32_t pending_termination;
    hl_linux_abi box;
    hl_linux_fd_entry *box_fds;
    hl_linux_ofd_entry *box_ofds;
    uint32_t box_initialized;
    hl_options options;
    uint32_t options_initialized;
    uint32_t options_owned;
    void *syscall_context;
    hl_syscall_trap_fn syscall_dispatch;
    hl_engine_box_config box_config;
    char *owned_rootfs;
    char *owned_working_directory;
    char *owned_hostname;
    char *owned_environment;
    char *owned_box_strings[11];
    hl_engine_publish_rule *owned_publish;
    hl_host_handle executable;
    hl_engine_executable executable_config;
    hl_engine_main_image_plan main_image_plan;
    unsigned char *owned_executable_image;
    unsigned char *owned_interpreter_image;
    size_t interpreter_image_size;
    uint64_t translations;
    int checkpoint_broker;
    int checkpoint_trigger;
    int checkpoint_control_parent;
    int checkpoint_control_child;
    uint32_t checkpoint_control_ready;
};

#if !defined(_WIN32)
static pthread_mutex_t checkpoint_registry_lock = PTHREAD_MUTEX_INITIALIZER;

typedef struct hl_checkpoint_descriptor {
    int descriptor;
    dev_t device;
    ino_t inode;
} hl_checkpoint_descriptor;

static hl_checkpoint_descriptor *checkpoint_registry;
static size_t checkpoint_registry_count;
static size_t checkpoint_registry_capacity;
#if defined(HL_NATIVE_TEST_HOOKS)
static _Thread_local int checkpoint_registry_fail_allocation;
static _Thread_local uint32_t checkpoint_adopt_failure_position;
static _Thread_local uint32_t checkpoint_adopt_position;
#endif

static void hl_engine_checkpoint_registry_compact_locked(void) {
    size_t index;
    for (index = 0; index < checkpoint_registry_count; ++index) {
        hl_checkpoint_descriptor *entry = &checkpoint_registry[index];
        struct stat current;
        if (fstat(entry->descriptor, &current) != 0 || current.st_dev != entry->device ||
            current.st_ino != entry->inode)
            *entry = checkpoint_registry[--checkpoint_registry_count];
        else
            continue;
        --index;
    }
}

static int hl_engine_checkpoint_registry_reserve_locked(size_t additional) {
    size_t required = checkpoint_registry_count + additional;
#if defined(HL_NATIVE_TEST_HOOKS)
    if (checkpoint_registry_fail_allocation) {
        checkpoint_registry_fail_allocation = 0;
        return -1;
    }
#endif
    if (required > checkpoint_registry_capacity) {
        size_t capacity = checkpoint_registry_capacity == 0 ? 16 : checkpoint_registry_capacity;
        while (capacity < required)
            capacity *= 2;
        hl_checkpoint_descriptor *grown = realloc(checkpoint_registry, capacity * sizeof(*grown));
        if (grown == NULL) return -1;
        checkpoint_registry = grown;
        checkpoint_registry_capacity = capacity;
    }
    return 0;
}

static int hl_engine_checkpoint_descriptor_identity(int descriptor, hl_checkpoint_descriptor *out) {
    struct stat identity;
    if (descriptor < 0 || fstat(descriptor, &identity) != 0) return -1;
    *out = (hl_checkpoint_descriptor){descriptor, identity.st_dev, identity.st_ino};
    return 0;
}

static void hl_engine_checkpoint_descriptor_append_locked(hl_checkpoint_descriptor descriptor) {
    size_t index;
    for (index = 0; index < checkpoint_registry_count; ++index) {
        hl_checkpoint_descriptor *entry = &checkpoint_registry[index];
        if (entry->descriptor == descriptor.descriptor && entry->device == descriptor.device &&
            entry->inode == descriptor.inode)
            return;
    }
    checkpoint_registry[checkpoint_registry_count++] = descriptor;
}

int hl_engine_checkpoint_descriptors_register(int first, int second) {
    hl_checkpoint_descriptor descriptors[2];
    size_t count = second < 0 ? 1 : 2;
    int status = -1;
    (void)pthread_mutex_lock(&checkpoint_registry_lock);
    hl_engine_checkpoint_registry_compact_locked();
    if (hl_engine_checkpoint_descriptor_identity(first, &descriptors[0]) == 0 &&
        (count == 1 || hl_engine_checkpoint_descriptor_identity(second, &descriptors[1]) == 0) &&
        hl_engine_checkpoint_registry_reserve_locked(count) == 0) {
        hl_engine_checkpoint_descriptor_append_locked(descriptors[0]);
        if (count == 2) hl_engine_checkpoint_descriptor_append_locked(descriptors[1]);
        status = 0;
    }
    (void)pthread_mutex_unlock(&checkpoint_registry_lock);
    return status;
}

void hl_engine_checkpoint_fork_prepare(void) {
    (void)pthread_mutex_lock(&checkpoint_registry_lock);
}

void hl_engine_checkpoint_fork_parent(void) {
    (void)pthread_mutex_unlock(&checkpoint_registry_lock);
}

static void hl_engine_checkpoint_fork_close(int descriptor, int broker, int trigger, int control) {
    if (descriptor < 0 || descriptor == broker || descriptor == trigger || descriptor == control) return;
    hl_host_process_fd_private_remove(descriptor);
    (void)close(descriptor);
}

void hl_engine_checkpoint_fork_child(int broker, int trigger, int control) {
    size_t index;
    for (index = 0; index < checkpoint_registry_count; ++index) {
        hl_checkpoint_descriptor *entry = &checkpoint_registry[index];
        struct stat identity;
        if (fstat(entry->descriptor, &identity) == 0 && identity.st_dev == entry->device &&
            identity.st_ino == entry->inode)
            hl_engine_checkpoint_fork_close(entry->descriptor, broker, trigger, control);
    }
    (void)pthread_mutex_unlock(&checkpoint_registry_lock);
}
#else
void hl_engine_checkpoint_fork_prepare(void) {
}

void hl_engine_checkpoint_fork_parent(void) {
}

void hl_engine_checkpoint_fork_child(int broker, int trigger, int control) {
    (void)broker;
    (void)trigger;
    (void)control;
}

int hl_engine_checkpoint_descriptors_register(int first, int second) {
    (void)first;
    (void)second;
    return 0;
}
#endif

enum {
    HL_ENGINE_CREATED = 0,
    HL_ENGINE_STARTING = 1,
    HL_ENGINE_RUNNING = 2,
    HL_ENGINE_FINISHED = 3,
    HL_ENGINE_DESTROYING = 4
};

static void hl_engine_yield(hl_engine *engine);

static void hl_engine_lock(hl_engine *engine) {
    while (atomic_flag_test_and_set_explicit(&engine->lock, memory_order_acquire)) {}
}

static void hl_engine_unlock(hl_engine *engine) {
    atomic_flag_clear_explicit(&engine->lock, memory_order_release);
}

static void hl_engine_checkpoint_control_lock(hl_engine *engine) {
    while (atomic_exchange_explicit(&engine->checkpoint_control_lock, true, memory_order_acquire))
        hl_engine_yield(engine);
}

static void hl_engine_checkpoint_control_unlock(hl_engine *engine) {
    atomic_store_explicit(&engine->checkpoint_control_lock, false, memory_order_release);
}

#if defined(HL_NATIVE_TEST_HOOKS)
static atomic_uint checkpoint_test_phase;

HL_API uint32_t hl_c_backend_checkpoint_test_arm(void);
HL_API uint32_t hl_c_backend_checkpoint_test_phase(void);
HL_API void hl_c_backend_checkpoint_test_release(void);
HL_API void hl_c_backend_checkpoint_test_reset(void);
HL_API uint32_t hl_c_backend_checkpoint_test_prune_foreign_descriptors(void);
HL_API void hl_c_backend_checkpoint_test_fail_registry_allocation(void);
HL_API void hl_c_backend_checkpoint_test_fail_private_adopt(uint32_t position);
HL_API uint64_t hl_c_backend_checkpoint_test_private_descriptor_count(void);

HL_API uint32_t hl_c_backend_checkpoint_test_arm(void) {
    atomic_store_explicit(&checkpoint_test_phase, 1, memory_order_release);
    return 1;
}

HL_API uint32_t hl_c_backend_checkpoint_test_phase(void) {
    return atomic_load_explicit(&checkpoint_test_phase, memory_order_acquire);
}

HL_API void hl_c_backend_checkpoint_test_release(void) {
    atomic_store_explicit(&checkpoint_test_phase, 5, memory_order_release);
}

HL_API void hl_c_backend_checkpoint_test_reset(void) {
    atomic_store_explicit(&checkpoint_test_phase, 0, memory_order_release);
}

HL_API void hl_c_backend_checkpoint_test_fail_registry_allocation(void) {
#if !defined(_WIN32)
    checkpoint_registry_fail_allocation = 1;
#endif
}

HL_API void hl_c_backend_checkpoint_test_fail_private_adopt(uint32_t position) {
#if !defined(_WIN32)
    checkpoint_adopt_failure_position = position;
    checkpoint_adopt_position = 0;
#else
    (void)position;
#endif
}

HL_API uint64_t hl_c_backend_checkpoint_test_private_descriptor_count(void) {
#if defined(_WIN32)
    return 0;
#else
    return (uint64_t)hl_host_process_fd_private_count_current();
#endif
}

HL_API uint32_t hl_c_backend_checkpoint_test_prune_foreign_descriptors(void) {
#if defined(_WIN32)
    return 0;
#else
    int foreign[2] = {-1, -1};
    int active[2] = {-1, -1};
    int release[2] = {-1, -1};
    pid_t child;
    int status = 0;
    unsigned char byte = 1;
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, foreign) != 0 || socketpair(AF_UNIX, SOCK_STREAM, 0, active) != 0 ||
        pipe(release) != 0)
        goto cleanup;
    if (hl_engine_checkpoint_descriptors_register(foreign[0], foreign[1]) != 0 ||
        hl_engine_checkpoint_descriptors_register(active[0], active[1]) != 0)
        goto cleanup;
    hl_engine_checkpoint_fork_prepare();
    child = fork();
    if (child == 0) {
        int valid;
        hl_engine_checkpoint_fork_child(active[0], active[1], -1);
        (void)close(release[1]);
        if (read(release[0], &byte, sizeof(byte)) != (ssize_t)sizeof(byte)) _exit(2);
        valid = fcntl(foreign[0], F_GETFD) < 0 && errno == EBADF && fcntl(foreign[1], F_GETFD) < 0 && errno == EBADF &&
                fcntl(active[0], F_GETFD) >= 0 && fcntl(active[1], F_GETFD) >= 0;
        _exit(valid ? 0 : 3);
    }
    hl_engine_checkpoint_fork_parent();
    if (child < 0) goto cleanup;
    (void)close(release[0]);
    release[0] = -1;
    (void)close(foreign[0]);
    (void)close(foreign[1]);
    if (foreign[0] < 0 || foreign[1] < 0 || active[0] < 0 || active[1] < 0) goto cleanup_child;
    if (dup2(active[0], foreign[0]) < 0 || dup2(active[1], foreign[1]) < 0) goto cleanup_child;
    if (write(release[1], &byte, sizeof(byte)) != (ssize_t)sizeof(byte)) goto cleanup_child;
    if (waitpid(child, &status, 0) != child) goto cleanup;
    status = WIFEXITED(status) && WEXITSTATUS(status) == 0;
    goto cleanup;
cleanup_child:
    (void)close(release[1]);
    release[1] = -1;
    (void)waitpid(child, NULL, 0);
cleanup:
    if (foreign[0] >= 0) (void)close(foreign[0]);
    if (foreign[1] >= 0) (void)close(foreign[1]);
    if (active[0] >= 0) (void)close(active[0]);
    if (active[1] >= 0) (void)close(active[1]);
    if (release[0] >= 0) (void)close(release[0]);
    if (release[1] >= 0) (void)close(release[1]);
    return status ? 1u : 0u;
#endif
}

static void hl_engine_checkpoint_test_pause(hl_engine *engine) {
    unsigned int expected = 1;
    if (!atomic_compare_exchange_strong_explicit(&checkpoint_test_phase, &expected, 2, memory_order_acq_rel,
                                                 memory_order_acquire))
        return;
    while (atomic_load_explicit(&checkpoint_test_phase, memory_order_acquire) != 5)
        hl_engine_yield(engine);
}

static void hl_engine_checkpoint_test_process_started(void) {
    unsigned int expected = 2;
    (void)atomic_compare_exchange_strong_explicit(&checkpoint_test_phase, &expected, 3, memory_order_acq_rel,
                                                  memory_order_acquire);
}

static void hl_engine_checkpoint_test_request_completed(void) {
    unsigned int expected = 5;
    (void)atomic_compare_exchange_strong_explicit(&checkpoint_test_phase, &expected, 6, memory_order_acq_rel,
                                                  memory_order_acquire);
}

static void hl_engine_checkpoint_test_run_ready(void) {
    unsigned int expected = 6;
    (void)atomic_compare_exchange_strong_explicit(&checkpoint_test_phase, &expected, 7, memory_order_acq_rel,
                                                  memory_order_acquire);
}
#endif

static void hl_engine_yield(hl_engine *engine) {
    hl_host_result now = engine->host.clock->monotonic_ns(engine->host.context);
    uint64_t deadline;
    if (now.status != HL_STATUS_OK) return;
    deadline = now.value == UINT64_MAX ? UINT64_MAX : now.value + 1u;
    (void)engine->host.clock->sleep_until(engine->host.context, HL_HOST_CLOCK_MONOTONIC, deadline);
}

static hl_status hl_engine_checkpoint_control_ready(hl_engine *engine) {
#if defined(_WIN32)
    (void)engine;
    return HL_STATUS_NOT_SUPPORTED;
#else
    struct pollfd waiting;
    unsigned char ack = 0;
    int ready;
    if (engine->checkpoint_control_ready) return HL_STATUS_OK;
    if (engine->checkpoint_control_parent < 0) return HL_STATUS_NOT_SUPPORTED;
    waiting = (struct pollfd){.fd = engine->checkpoint_control_parent, .events = POLLIN};
    do {
        ready = poll(&waiting, 1, 5000);
    } while (ready < 0 && errno == EINTR);
    if (ready <= 0 || read(waiting.fd, &ack, sizeof(ack)) != (ssize_t)sizeof(ack) || ack != 0xa5)
        return HL_STATUS_PLATFORM_FAILURE;
    engine->checkpoint_control_ready = 1;
    return HL_STATUS_OK;
#endif
}

static void hl_engine_checkpoint_arena_stop(hl_engine *engine) {
#if defined(_WIN32)
    (void)engine;
#else
    if (engine->checkpoint_control_parent >= 0) (void)shutdown(engine->checkpoint_control_parent, SHUT_RDWR);
#endif
}

uint32_t hl_engine_abi(void) {
    return HL_ENGINE_ABI;
}

const char *hl_engine_version(void) {
    return "0.1.2";
}

uint64_t hl_engine_translation_count(const hl_engine *engine) {
    return engine == NULL ? 0 : engine->translations;
}

#if !defined(_WIN32)
static void hl_engine_checkpoint_descriptor_close(int *descriptor) {
    if (*descriptor < 0) return;
    hl_host_process_fd_private_remove(*descriptor);
    (void)close(*descriptor);
    *descriptor = -1;
}

static int hl_engine_checkpoint_descriptor_adopt(int *descriptor) {
    int original = *descriptor;
    int adopted;
    int fail_adopt = 0;
#if defined(HL_NATIVE_TEST_HOOKS)
    checkpoint_adopt_position++;
    if (checkpoint_adopt_failure_position == checkpoint_adopt_position) {
        checkpoint_adopt_failure_position = 0;
        fail_adopt = 1;
    }
#endif
    adopted = fail_adopt ? -1 : hl_host_process_fd_private_adopt(original);
    if (adopted < 0) {
        (void)close(original);
        *descriptor = -1;
        return -1;
    }
    *descriptor = adopted;
    return 0;
}
#endif

hl_status hl_engine_checkpoint_configure(hl_engine *engine, int broker, int trigger) {
#if defined(_WIN32)
    if (engine == NULL || broker < 0 || trigger < 0) return HL_STATUS_INVALID_ARGUMENT;
    return HL_STATUS_NOT_SUPPORTED;
#else
    int broker_copy = -1;
    int trigger_copy = -1;
    int control_parent = engine == NULL ? -1 : engine->checkpoint_control_parent;
    int control_child = engine == NULL ? -1 : engine->checkpoint_control_child;
    int control_created = 0;
    hl_status status = HL_STATUS_PLATFORM_FAILURE;
    if (engine == NULL || broker < 0 || trigger < 0) return HL_STATUS_INVALID_ARGUMENT;
    (void)pthread_mutex_lock(&checkpoint_registry_lock);
    hl_engine_checkpoint_registry_compact_locked();
    if (hl_engine_checkpoint_registry_reserve_locked(4) != 0) {
        (void)pthread_mutex_unlock(&checkpoint_registry_lock);
        return HL_STATUS_OUT_OF_MEMORY;
    }
    broker_copy = dup(broker);
    if (broker_copy < 0) goto fail;
    trigger_copy = dup(trigger);
    if (trigger_copy < 0) goto fail;
    (void)fcntl(broker_copy, F_SETFD, FD_CLOEXEC);
    (void)fcntl(trigger_copy, F_SETFD, FD_CLOEXEC);
    if (engine->checkpoint_control_parent < 0) {
        int control[2];
        if (socketpair(AF_UNIX, SOCK_STREAM, 0, control) != 0) goto fail;
        (void)fcntl(control[0], F_SETFD, FD_CLOEXEC);
        (void)fcntl(control[1], F_SETFD, FD_CLOEXEC);
        control_parent = control[0];
        control_child = control[1];
        control_created = 1;
    }
    if (hl_engine_checkpoint_descriptor_adopt(&broker_copy) != 0 ||
        hl_engine_checkpoint_descriptor_adopt(&trigger_copy) != 0 ||
        (control_created && hl_engine_checkpoint_descriptor_adopt(&control_parent) != 0) ||
        (control_created && hl_engine_checkpoint_descriptor_adopt(&control_child) != 0))
        goto fail;
    {
        hl_checkpoint_descriptor registered[4];
        if (hl_engine_checkpoint_descriptor_identity(broker_copy, &registered[0]) != 0 ||
            hl_engine_checkpoint_descriptor_identity(trigger_copy, &registered[1]) != 0 ||
            hl_engine_checkpoint_descriptor_identity(control_parent, &registered[2]) != 0 ||
            hl_engine_checkpoint_descriptor_identity(control_child, &registered[3]) != 0)
            goto fail;
        /* A configured transport is the authority that capture is available. Keep the guest-side
         * trigger arm coupled to that capability instead of relying on every embedder to duplicate
         * the private launch option correctly. Restore remains independently selected by HL_RESTORE,
         * while the restored process is immediately armed for its next capture. */
        if (hl_options_set(&engine->options, "HL_CHECKPOINT", "1", 1) != 0) {
            status = HL_STATUS_OUT_OF_MEMORY;
            goto fail;
        }
        hl_engine_checkpoint_descriptor_append_locked(registered[0]);
        hl_engine_checkpoint_descriptor_append_locked(registered[1]);
        hl_engine_checkpoint_descriptor_append_locked(registered[2]);
        hl_engine_checkpoint_descriptor_append_locked(registered[3]);
    }
    hl_engine_checkpoint_descriptor_close(&engine->checkpoint_broker);
    hl_engine_checkpoint_descriptor_close(&engine->checkpoint_trigger);
    engine->checkpoint_broker = broker_copy;
    engine->checkpoint_trigger = trigger_copy;
    if (control_created) {
        engine->checkpoint_control_parent = control_parent;
        engine->checkpoint_control_child = control_child;
    }
    (void)pthread_mutex_unlock(&checkpoint_registry_lock);
    return HL_STATUS_OK;
fail:
    hl_engine_checkpoint_descriptor_close(&broker_copy);
    hl_engine_checkpoint_descriptor_close(&trigger_copy);
    if (control_created) {
        hl_engine_checkpoint_descriptor_close(&control_parent);
        hl_engine_checkpoint_descriptor_close(&control_child);
    }
    (void)pthread_mutex_unlock(&checkpoint_registry_lock);
    return status;
#endif
}

enum { HL_ENGINE_STRING_LIMIT = 64 * 1024 * 1024 };

enum { HL_ENGINE_EXECUTABLE_LIMIT = 64 * 1024 * 1024 };

static hl_status hl_engine_read_executable(hl_engine *engine, hl_host_handle handle) {
    hl_host_file_metadata before = {0}, after = {0};
    uint64_t offset = 0;
    if (engine->host.file->metadata(engine->host.context, handle, &before).status != HL_STATUS_OK)
        return HL_STATUS_PLATFORM_FAILURE;
    if (before.type != HL_HOST_FILE_TYPE_REGULAR || before.size == 0 || before.size > HL_ENGINE_EXECUTABLE_LIMIT)
        return HL_STATUS_INVALID_ARGUMENT;
    engine->executable_config.image_size = (size_t)before.size;
    engine->owned_executable_image = malloc((size_t)before.size);
    if (engine->owned_executable_image == NULL) return HL_STATUS_OUT_OF_MEMORY;
    while (offset < before.size) {
        hl_host_result read =
            engine->host.file->read_at(engine->host.context, handle, offset,
                                       (hl_host_bytes){engine->owned_executable_image + offset, before.size - offset});
        if (read.status != HL_STATUS_OK || read.value == 0 || read.value > before.size - offset)
            return HL_STATUS_PLATFORM_FAILURE;
        offset += read.value;
    }
    if (engine->host.file->metadata(engine->host.context, handle, &after).status != HL_STATUS_OK)
        return HL_STATUS_PLATFORM_FAILURE;
    if (before.stable_device != after.stable_device || before.stable_object != after.stable_object ||
        before.size != after.size || before.modified_ns != after.modified_ns || before.changed_ns != after.changed_ns)
        return HL_STATUS_OK;
    engine->executable_config.image = engine->owned_executable_image;
    engine->executable_config.image_size = (size_t)before.size;
    return HL_STATUS_OK;
}

static char *hl_engine_copy_string(const char *value) {
    size_t length;
    char *copy;
    if (value == NULL) return NULL;
    for (length = 0; length < HL_ENGINE_STRING_LIMIT && value[length] != 0; ++length) {}
    if (length == HL_ENGINE_STRING_LIMIT) return NULL;
    copy = malloc(length + 1);
    if (copy != NULL) memcpy(copy, value, length + 1);
    return copy;
}

static int hl_engine_set_option(hl_options *options, const char *name, const char *value) {
    return value == NULL || value[0] == 0 ? 0 : hl_options_set(options, name, value, 1);
}

static int hl_engine_name_start(unsigned char c) {
    return (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || c == '_';
}

static int hl_engine_name_continue(unsigned char c) {
    return hl_engine_name_start(c) || (c >= '0' && c <= '9');
}

static int hl_engine_environment_valid(const char *environment) {
    size_t offset = 0;
    if (environment == NULL) return 1;
    if (environment[0] == 0) return 0;
    while (offset < HL_ENGINE_STRING_LIMIT && environment[offset] != 0) {
        if (!hl_engine_name_start((unsigned char)environment[offset])) return 0;
        do {
            ++offset;
        } while (offset < HL_ENGINE_STRING_LIMIT && hl_engine_name_continue((unsigned char)environment[offset]));
        if (offset == HL_ENGINE_STRING_LIMIT) return 0;
        if (environment[offset++] != '=') return 0;
        while (offset < HL_ENGINE_STRING_LIMIT && environment[offset] != 0 && environment[offset] != '\n')
            ++offset;
        if (offset == HL_ENGINE_STRING_LIMIT) return 0;
        if (environment[offset] == '\n') {
            ++offset;
            if (environment[offset] == 0) return 0;
        }
    }
    return offset < HL_ENGINE_STRING_LIMIT;
}

static int hl_engine_hostname_valid(const char *hostname) {
    size_t length = 0;
    if (hostname == NULL) return 1;
    while (length <= 64 && hostname[length] != 0) {
        unsigned char c = (unsigned char)hostname[length];
        if (!((c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') || c == '-')) return 0;
        ++length;
    }
    return length != 0 && length <= 64 && hostname[0] != '-' && hostname[length - 1] != '-';
}

static int hl_engine_nonempty_string(const char *value) {
    size_t length = 0;
    if (value == NULL) return 1;
    while (length < HL_ENGINE_STRING_LIMIT && value[length] != 0)
        ++length;
    return length != 0 && length < HL_ENGINE_STRING_LIMIT;
}

static int hl_engine_absolute_string(const char *value) {
    return value == NULL || (value[0] == '/' && hl_engine_nonempty_string(value));
}

static int hl_engine_uint_range(const char *begin, const char *end, unsigned maximum) {
    unsigned value = 0;
    if (begin == end) return 0;
    while (begin != end) {
        if (*begin < '0' || *begin > '9' || value > (maximum - (unsigned)(*begin - '0')) / 10u) return 0;
        value = value * 10u + (unsigned)(*begin++ - '0');
    }
    return value != 0 && value <= maximum;
}

static int hl_engine_volumes_valid(const char *spec) {
    const char *entry = spec;
    unsigned count = 0;
    if (spec == NULL) return 1;
    while (*entry != 0) {
        const char *end = strchr(entry, ',');
        const char *colon;
        if (end == NULL) end = entry + strlen(entry);
        if (++count > 32) return 0;
        if ((size_t)(end - entry) > 3 && ((entry[0] == 'r' && entry[1] == 'o' && entry[2] == ':') ||
                                          (entry[0] == 'r' && entry[1] == 'w' && entry[2] == ':')))
            entry += 3;
        colon = memchr(entry, ':', (size_t)(end - entry));
        if (colon == NULL || entry == colon || colon + 1 == end || entry[0] != '/' || colon[1] != '/' ||
            memchr(colon + 1, ':', (size_t)(end - colon - 1)) != NULL)
            return 0;
        if (*end == 0) return 1;
        entry = end + 1;
    }
    return 0;
}

static int hl_engine_lower_valid(const char *spec) {
    const char *entry = spec;
    if (spec == NULL) return 1;
    while (*entry != 0) {
        const char *end = strchr(entry, ':');
        if (end == NULL) end = entry + strlen(entry);
        if (entry == end || *entry != '/') return 0;
        if (*end == 0) return 1;
        entry = end + 1;
    }
    return 0;
}

static int hl_engine_limits_valid(const char *spec) {
    const char *entry = spec;
    if (spec == NULL) return 1;
    while (*entry != 0) {
        const char *end = strchr(entry, ',');
        const char *equals;
        const char *colon;
        if (end == NULL) end = entry + strlen(entry);
        equals = memchr(entry, '=', (size_t)(end - entry));
        if (equals == NULL || equals == entry || equals + 1 == end) return 0;
        colon = memchr(equals + 1, ':', (size_t)(end - equals - 1));
        if (colon != NULL &&
            (colon == equals + 1 || colon + 1 == end || memchr(colon + 1, ':', (size_t)(end - colon - 1)) != NULL))
            return 0;
        {
            const char *value = equals + 1;
            const char *value_end = colon == NULL ? end : colon;
            for (;;) {
                const char *cursor = value;
                int special = (size_t)(value_end - value) == 9 && memcmp(value, "unlimited", 9) == 0;
                if (!special && (size_t)(value_end - value) == 2 && value[0] == '-' && value[1] == '1') special = 1;
                if (!special) {
                    if (cursor == value_end) return 0;
                    while (cursor != value_end && *cursor >= '0' && *cursor <= '9')
                        ++cursor;
                    if (cursor != value_end) return 0;
                }
                if (colon == NULL || value == colon + 1) break;
                value = colon + 1;
                value_end = end;
            }
        }
        if (*end == 0) return 1;
        entry = end + 1;
    }
    return 0;
}

static int hl_engine_identity_valid(const char *value, size_t maximum) {
    size_t index = 0;
    if (value == NULL) return 1;
    while (index < maximum && value[index] != 0) {
        unsigned char c = (unsigned char)value[index++];
        if (!hl_engine_name_continue(c) && c != '-' && c != '.') return 0;
    }
    return index != 0 && index <= maximum && value[index] == 0;
}

static int hl_engine_ip_valid(const char *value) {
    unsigned part = 0, digits = 0, separators = 0;
    if (value == NULL) return 1;
    while (*value != 0) {
        if (*value >= '0' && *value <= '9') {
            part = part * 10u + (unsigned)(*value - '0');
            if (++digits > 3 || part > 255) return 0;
        } else if (*value == '.' && digits != 0 && separators < 3) {
            ++separators;
            part = 0;
            digits = 0;
        } else
            return 0;
        ++value;
    }
    return separators == 3 && digits != 0;
}

static int hl_engine_proxy_valid(const char *value) {
    const char *colon;
    if (value == NULL) return 1;
    colon = strrchr(value, ':');
    return colon != NULL && colon != value && hl_engine_uint_range(colon + 1, value + strlen(value), 65535);
}

/* Hand-laid X-macro table: clang-format is NOT idempotent on this
 * continuation-backslash block (formatting it twice yields different output), so
 * `format` followed by `format-check` could never converge and the check could not
 * be gated in CI. One field per line, by intent.
 * NOTE: the off/on markers must be the ENTIRE comment -- trailing prose on the same
 * line makes clang-format ignore them. */
/* clang-format off */
#define HL_BOX_STRING_FIELDS(X)                                                                                        \
    X(lower_layers, "HL_LOWER")                                                                                        \
    X(volumes, "HL_VOLUMES")                                                                                           \
    X(limits, "HL_ULIMITS")                                                                                            \
    X(network_namespace, "HL_NETNS") X(translation_cache, "HL_PCACHE_DIR") X(network_bridge, "HL_NETBR")               \
        X(ip, "HL_IP") X(filesystem_generation, "HL_FSGEN_FILE") X(egress_proxy, "HL_EGRESS_SOCKS")                    \
            X(file_owners, "HL_FILE_OWNERS")
/* clang-format on */

static hl_status hl_engine_apply_box(hl_engine *engine, const hl_engine_box_config *box) {
    char number[32];
    char publish[1024];
    uint32_t known_flags = HL_ENGINE_BOX_ROOTFS_READ_ONLY | HL_ENGINE_BOX_SANDBOX | HL_ENGINE_BOX_NETWORK_ISOLATED |
                           HL_ENGINE_BOX_PUBLISH_EXTERNAL | HL_ENGINE_BOX_TRANSLATION_CACHE_DISABLED |
                           HL_ENGINE_BOX_SENTRY_ONLY;
    if (box == NULL) return HL_STATUS_OK;
    /* One accepted generation. An undersized box is rejected rather than partially read. */
    if (box->abi != HL_ENGINE_BOX_ABI || box->size < sizeof(*box)) return HL_STATUS_ABI_MISMATCH;
    if ((box->flags & ~known_flags) != 0 || box->reserved != 0 || box->uid < -1 || box->gid < -1 ||
        box->checkpoint_policy > HL_ENGINE_CHECKPOINT_REFUSE ||
        (box->checkpoint_mode & ~(HL_ENGINE_CHECKPOINT_CAPTURE | HL_ENGINE_CHECKPOINT_RESTORE)) != 0)
        return HL_STATUS_INVALID_ARGUMENT;
    if (box->working_directory != NULL && box->working_directory[0] != '/') return HL_STATUS_INVALID_ARGUMENT;
    if (!hl_engine_hostname_valid(box->hostname) || !hl_engine_environment_valid(box->environment))
        return HL_STATUS_INVALID_ARGUMENT;
#define VALIDATE_BOX_STRING(field, option)                                                                             \
    if (!hl_engine_nonempty_string(box->field)) return HL_STATUS_INVALID_ARGUMENT;
    HL_BOX_STRING_FIELDS(VALIDATE_BOX_STRING)
#undef VALIDATE_BOX_STRING
    if (!hl_engine_absolute_string(box->translation_cache) || !hl_engine_absolute_string(box->filesystem_generation) ||
        !hl_engine_lower_valid(box->lower_layers) || box->publish_count > 32 ||
        ((box->publish_count == 0) != (box->publish == NULL)) || !hl_engine_volumes_valid(box->volumes) ||
        !hl_engine_limits_valid(box->limits) || !hl_engine_identity_valid(box->network_namespace, 39) ||
        !hl_engine_identity_valid(box->network_bridge, 40) || !hl_engine_ip_valid(box->ip) ||
        !hl_engine_proxy_valid(box->egress_proxy) ||
        ((box->flags & HL_ENGINE_BOX_SANDBOX) && (box->flags & HL_ENGINE_BOX_SENTRY_ONLY)) ||
        ((box->flags & HL_ENGINE_BOX_TRANSLATION_CACHE_DISABLED) && box->translation_cache != NULL) ||
        (box->ip != NULL && box->network_bridge == NULL) ||
        ((box->flags & HL_ENGINE_BOX_PUBLISH_EXTERNAL) && box->publish_count == 0) ||
        ((box->flags & HL_ENGINE_BOX_NETWORK_ISOLATED) &&
         (box->publish_count != 0 || box->network_bridge != NULL || box->ip != NULL || box->egress_proxy != NULL)))
        return HL_STATUS_INVALID_ARGUMENT;
    for (uint32_t index = 0; index < box->publish_count; ++index)
        if (box->publish[index].host_port == 0 || box->publish[index].guest_port == 0)
            return HL_STATUS_INVALID_ARGUMENT;
    if (box->checkpoint_mode != 0) {
        number[0] = (char)('0' + box->checkpoint_policy);
        number[1] = 0;
        if (hl_options_set(&engine->options, "HL_CHECKPOINT_POLICY", number, 1) != HL_STATUS_OK ||
            ((box->checkpoint_mode & HL_ENGINE_CHECKPOINT_CAPTURE) &&
             hl_options_set(&engine->options, "HL_CHECKPOINT", "1", 1) != HL_STATUS_OK) ||
            ((box->checkpoint_mode & HL_ENGINE_CHECKPOINT_RESTORE) &&
             hl_options_set(&engine->options, "HL_RESTORE", "1", 1) != HL_STATUS_OK))
            return HL_STATUS_OUT_OF_MEMORY;
    }
    engine->owned_working_directory = hl_engine_copy_string(box->working_directory);
    engine->owned_hostname = hl_engine_copy_string(box->hostname);
    engine->owned_environment = hl_engine_copy_string(box->environment);
    if ((box->working_directory != NULL && engine->owned_working_directory == NULL) ||
        (box->hostname != NULL && engine->owned_hostname == NULL) ||
        (box->environment != NULL && engine->owned_environment == NULL))
        return HL_STATUS_OUT_OF_MEMORY;
    engine->box_config = *box;
    engine->box_config.size = sizeof(engine->box_config);
    engine->box_config.working_directory = engine->owned_working_directory;
    engine->box_config.hostname = engine->owned_hostname;
    engine->box_config.environment = engine->owned_environment;
    {
        size_t string_index = 0;
#define COPY_BOX_STRING(field, option)                                                                                 \
    engine->owned_box_strings[string_index] = hl_engine_copy_string(box->field);                                       \
    if (box->field != NULL && engine->owned_box_strings[string_index] == NULL) return HL_STATUS_OUT_OF_MEMORY;         \
    engine->box_config.field = engine->owned_box_strings[string_index++];
        HL_BOX_STRING_FIELDS(COPY_BOX_STRING)
#undef COPY_BOX_STRING
        if (box->publish_count != 0) {
            engine->owned_publish = malloc((size_t)box->publish_count * sizeof(*engine->owned_publish));
            if (engine->owned_publish == NULL) return HL_STATUS_OUT_OF_MEMORY;
            memcpy(engine->owned_publish, box->publish, (size_t)box->publish_count * sizeof(*engine->owned_publish));
            engine->box_config.publish = engine->owned_publish;
        }
    }
    engine->config.box = &engine->box_config;
    if (hl_engine_set_option(&engine->options, "HL_CWD", engine->owned_working_directory) != 0 ||
        hl_engine_set_option(&engine->options, "HL_HOSTNAME", engine->owned_hostname) != 0 ||
        hl_engine_set_option(&engine->options, "HL_GUEST_ENV", engine->owned_environment) != 0)
        return HL_STATUS_OUT_OF_MEMORY;
    if (box->uid >= 0) {
        snprintf(number, sizeof(number), "%d", box->uid);
        if (hl_options_set(&engine->options, "HL_UID", number, 1) != 0) return HL_STATUS_OUT_OF_MEMORY;
    }
    if (box->gid >= 0) {
        snprintf(number, sizeof(number), "%d", box->gid);
        if (hl_options_set(&engine->options, "HL_GID", number, 1) != 0) return HL_STATUS_OUT_OF_MEMORY;
    }
    if ((box->flags & HL_ENGINE_BOX_ROOTFS_READ_ONLY) != 0 &&
        hl_options_set(&engine->options, "HL_ROOTFS_RO", "1", 1) != 0)
        return HL_STATUS_OUT_OF_MEMORY;
    if ((box->flags & HL_ENGINE_BOX_SANDBOX) != 0 && (hl_options_set(&engine->options, "HL_SANDBOX", "1", 1) != 0 ||
                                                      hl_options_set(&engine->options, "HL_UNTRUSTED", "1", 1) != 0))
        return HL_STATUS_OUT_OF_MEMORY;
    if ((box->flags & HL_ENGINE_BOX_NETWORK_ISOLATED) != 0 &&
        hl_options_set(&engine->options, "HL_NET_ISOLATE", "1", 1) != 0)
        return HL_STATUS_OUT_OF_MEMORY;
    {
#define APPLY_BOX_STRING(field, option)                                                                                \
    if (hl_engine_set_option(&engine->options, option, engine->box_config.field) != 0) return HL_STATUS_OUT_OF_MEMORY;
        HL_BOX_STRING_FIELDS(APPLY_BOX_STRING)
#undef APPLY_BOX_STRING
        {
            size_t used = 0;
            uint32_t index;
            publish[0] = 0;
            for (index = 0; index < box->publish_count; ++index) {
                const uint8_t *address = (const uint8_t *)&box->publish[index].host_ipv4_be;
                int written =
                    box->publish[index].host_ipv4_be == 0
                        ? snprintf(publish + used, sizeof publish - used, "%s%u:%u", index ? "," : "",
                                   (unsigned)box->publish[index].host_port, (unsigned)box->publish[index].guest_port)
                        : snprintf(publish + used, sizeof publish - used, "%s%u.%u.%u.%u:%u:%u", index ? "," : "",
                                   (unsigned)address[0], (unsigned)address[1], (unsigned)address[2],
                                   (unsigned)address[3], (unsigned)box->publish[index].host_port,
                                   (unsigned)box->publish[index].guest_port);
                if (written < 0 || (size_t)written >= sizeof publish - used) return HL_STATUS_INVALID_ARGUMENT;
                used += (size_t)written;
            }
            if (box->publish_count != 0 && hl_engine_set_option(&engine->options, "HL_PUBLISH", publish) != 0)
                return HL_STATUS_OUT_OF_MEMORY;
        }
        if (box->translation_cache != NULL && hl_options_set(&engine->options, "HL_PCACHE", "1", 1) != 0)
            return HL_STATUS_OUT_OF_MEMORY;
        if ((box->flags & HL_ENGINE_BOX_TRANSLATION_CACHE_DISABLED) != 0 &&
            (hl_options_unset(&engine->options, "HL_PCACHE") != 0 ||
             hl_options_unset(&engine->options, "HL_PCACHE_DIR") != 0))
            return HL_STATUS_OUT_OF_MEMORY;
        if ((box->flags & HL_ENGINE_BOX_PUBLISH_EXTERNAL) != 0 &&
            hl_options_set(&engine->options, "HL_PUBLISH_DAEMON", "1", 1) != 0)
            return HL_STATUS_OUT_OF_MEMORY;
        if ((box->flags & HL_ENGINE_BOX_SENTRY_ONLY) != 0 &&
            hl_options_set(&engine->options, "HL_UNTRUSTED", "1", 1) != 0)
            return HL_STATUS_OUT_OF_MEMORY;
    }
    return HL_STATUS_OK;
}

static hl_status hl_engine_create_validate(const hl_engine_config *config, const hl_host_services *host,
                                           const hl_options *source_options, uint32_t borrow_options,
                                           hl_engine **out_engine) {
    if (out_engine == NULL) return HL_STATUS_INVALID_ARGUMENT;
    *out_engine = NULL;
    if (config == NULL || host == NULL) return HL_STATUS_INVALID_ARGUMENT;
    if (config->abi != HL_ENGINE_ABI || config->size < sizeof(*config)) return HL_STATUS_ABI_MISMATCH;
    if (config->guest_isa != HL_GUEST_ISA_AARCH64 && config->guest_isa != HL_GUEST_ISA_X86_64)
        return HL_STATUS_INVALID_ARGUMENT;
    if (config->flags != 0 || config->reserved != 0) return HL_STATUS_INVALID_ARGUMENT;
    if (config->payload_size != 0 && config->payload == NULL) return HL_STATUS_INVALID_ARGUMENT;
    if (config->payload_size != 0) return HL_STATUS_NOT_SUPPORTED;
    if (borrow_options && (source_options == NULL || config->memory_limit != 0 || config->pid_limit != 0 ||
                           config->cpu_limit != 0 || config->box != NULL || hl_options_validate(source_options) != 0))
        return HL_STATUS_INVALID_ARGUMENT;
    if (config->executable != NULL &&
        (config->executable->abi != HL_ENGINE_ABI || config->executable->size < sizeof(*config->executable)))
        return HL_STATUS_ABI_MISMATCH;
    if (config->executable != NULL &&
        (config->executable->reserved != 0 || config->executable->host_handle == HL_HOST_HANDLE_INVALID ||
         config->executable->image != NULL || config->executable->image_size != 0 ||
         (config->executable->ownership != HL_ENGINE_FD_TRANSFER &&
          config->executable->ownership != HL_ENGINE_FD_BORROW)))
        return HL_STATUS_INVALID_ARGUMENT;
    if (config->main_image_plan != NULL && (config->main_image_plan->abi != HL_ENGINE_MAIN_IMAGE_PLAN_ABI ||
                                            config->main_image_plan->size < sizeof(*config->main_image_plan)))
        return HL_STATUS_ABI_MISMATCH;
    if (config->main_image_plan != NULL &&
        ((config->main_image_plan->flags & ~HL_ENGINE_MAIN_IMAGE_PLAN_FORCE_DISPLACED) != 0 ||
         (config->main_image_plan->flags != 0 && config->main_image_plan->kind != 1) ||
         config->main_image_plan->link_end <= config->main_image_plan->link_start ||
         (config->main_image_plan->kind != 1 && config->main_image_plan->kind != 2)))
        return HL_STATUS_INVALID_ARGUMENT;
    if (config->fd_binding_count != 0 && config->fd_bindings == NULL) return HL_STATUS_INVALID_ARGUMENT;
    return hl_host_services_validate(host, HL_HOST_CAP_MEMORY | HL_HOST_CAP_CLOCK | HL_HOST_CAP_SYNC);
}

static hl_status hl_engine_interpreter_validate(const hl_engine_config *config, const void *interpreter_image,
                                                size_t interpreter_size) {
    if ((interpreter_image == NULL) != (interpreter_size == 0) || interpreter_size > 64u * 1024u * 1024u)
        return HL_STATUS_INVALID_ARGUMENT;
#if defined(_WIN32)
    if (interpreter_size != 0) return HL_STATUS_NOT_SUPPORTED;
#endif
    if (interpreter_size != 0 && (config->main_image_plan == NULL || config->main_image_plan->has_interpreter == 0))
        return HL_STATUS_INVALID_ARGUMENT;
    return HL_STATUS_OK;
}

static hl_engine *hl_engine_allocate(const hl_engine_config *config, const hl_host_services *host) {
    hl_engine *engine = calloc(1, sizeof(*engine));
    if (engine == NULL) return NULL;
    engine->checkpoint_broker = -1;
    engine->checkpoint_trigger = -1;
    engine->checkpoint_control_parent = -1;
    engine->checkpoint_control_child = -1;
    memcpy(&engine->config, config, sizeof(*config));
    memcpy(&engine->host, host, sizeof(*host));
    engine->executable = HL_HOST_HANDLE_INVALID;
    return engine;
}

static hl_status hl_engine_copy_image_plan(hl_engine *engine, const hl_engine_config *config,
                                           const void *interpreter_image, size_t interpreter_size) {
    if (config->main_image_plan == NULL) return HL_STATUS_OK;
    engine->main_image_plan = *config->main_image_plan;
    if (interpreter_size != 0) {
        engine->owned_interpreter_image = malloc(interpreter_size);
        if (engine->owned_interpreter_image == NULL) return HL_STATUS_OUT_OF_MEMORY;
        memcpy(engine->owned_interpreter_image, interpreter_image, interpreter_size);
        engine->interpreter_image_size = interpreter_size;
    }
    engine->config.main_image_plan = &engine->main_image_plan;
    return HL_STATUS_OK;
}

static hl_status hl_engine_pin_executable(hl_engine *engine, const hl_engine_config *config,
                                          const hl_host_services *host) {
    hl_host_result cloned;
    hl_status status;
    if (config->executable == NULL) return HL_STATUS_OK;
    status = hl_host_services_validate(host, HL_HOST_CAP_FILE);
    if (status != HL_STATUS_OK) return status;
    if (host->file->clone_for_fork == NULL || host->file->close == NULL) return HL_STATUS_ABI_MISMATCH;
    cloned = host->file->clone_for_fork(host->context, config->executable->host_handle);
    if (cloned.status != HL_STATUS_OK || cloned.value == HL_HOST_HANDLE_INVALID)
        return cloned.status == HL_STATUS_OK ? HL_STATUS_PLATFORM_FAILURE : (hl_status)cloned.status;
    engine->executable = cloned.value;
    engine->executable_config = (hl_engine_executable){
        HL_ENGINE_ABI, sizeof(engine->executable_config), HL_ENGINE_FD_BORROW, 0, cloned.value, NULL, 0};
    status = hl_engine_read_executable(engine, cloned.value);
    if (status != HL_STATUS_OK) return status;
    engine->config.executable = &engine->executable_config;
    return HL_STATUS_OK;
}

static hl_status hl_engine_initialize_options(hl_engine *engine, const hl_engine_config *config,
                                              const hl_options *source_options, uint32_t borrow_options) {
    char value[32];
    if (borrow_options) {
        engine->options = *source_options;
    } else {
        if ((source_options == NULL ? hl_options_clone_current(&engine->options)
                                    : hl_options_clone(&engine->options, source_options)) != 0)
            return HL_STATUS_OUT_OF_MEMORY;
        engine->options_owned = 1;
    }
    /* An explicit per-engine snapshot is authoritative. Embedders must not
     * inherit unrelated process-global HL_* settings behind Rust's resolved
     * launch plan. The legacy entry point still imports its environment. */
    if (source_options == NULL) hl_options_import_environment(&engine->options);
    engine->options_initialized = 1;
    engine->owned_rootfs = hl_engine_copy_string(config->rootfs);
    if (config->rootfs != NULL && engine->owned_rootfs == NULL) return HL_STATUS_OUT_OF_MEMORY;
    engine->config.rootfs = engine->owned_rootfs;
    if (config->memory_limit != 0) {
        snprintf(value, sizeof(value), "%llu", (unsigned long long)config->memory_limit);
        if (hl_options_set(&engine->options, "HL_MEM_MAX", value, 1) != 0) return HL_STATUS_OUT_OF_MEMORY;
    }
    if (config->pid_limit != 0) {
        snprintf(value, sizeof(value), "%u", config->pid_limit);
        if (hl_options_set(&engine->options, "HL_PIDS_MAX", value, 1) != 0) return HL_STATUS_OUT_OF_MEMORY;
    }
    if (config->cpu_limit != 0) {
        snprintf(value, sizeof(value), "%u", config->cpu_limit);
        if (hl_options_set(&engine->options, "HL_CPUS", value, 1) != 0) return HL_STATUS_OUT_OF_MEMORY;
    }
    return hl_engine_apply_box(engine, config->box);
}

static hl_status hl_engine_initialize_linux_abi(hl_engine *engine) {
    hl_status status;
    engine->box_fds = calloc(HL_LINUX_FD_LIMIT, sizeof(*engine->box_fds));
    engine->box_ofds = calloc(HL_LINUX_OFD_LIMIT, sizeof(*engine->box_ofds));
    if (engine->box_fds == NULL || engine->box_ofds == NULL) return HL_STATUS_OUT_OF_MEMORY;
    status = hl_linux_abi_init(&engine->box, &engine->host, engine->box_fds, HL_LINUX_FD_LIMIT, engine->box_ofds,
                               HL_LINUX_OFD_LIMIT);
    if (status != HL_STATUS_OK) return status;
    engine->box_initialized = 1;
    return HL_STATUS_OK;
}

static hl_status hl_engine_validate_fd_bindings(const hl_engine_config *config) {
    uint32_t index;
    for (index = 0; index < config->fd_binding_count; ++index) {
        const hl_engine_fd_binding *binding = &config->fd_bindings[index];
        uint32_t previous;
        if (binding->abi != HL_ENGINE_ABI || binding->size < sizeof(*binding) ||
            binding->host_handle == HL_HOST_HANDLE_INVALID || binding->guest_fd >= HL_LINUX_FD_LIMIT ||
            (binding->ownership != HL_ENGINE_FD_TRANSFER && binding->ownership != HL_ENGINE_FD_BORROW))
            return HL_STATUS_INVALID_ARGUMENT;
        for (previous = 0; previous < index; ++previous)
            if (config->fd_bindings[previous].guest_fd == binding->guest_fd) return HL_STATUS_INVALID_ARGUMENT;
    }
    return HL_STATUS_OK;
}

static hl_status hl_engine_install_fd_bindings(hl_engine *engine, const hl_engine_config *config,
                                               const hl_host_services *host) {
    hl_host_handle *candidate_handles;
    hl_status status;
    uint32_t index;
    if (config->fd_binding_count == 0) return HL_STATUS_OK;
    status = hl_host_services_validate(host, HL_HOST_CAP_FILE);
    if (status != HL_STATUS_OK) return status;
    status = hl_engine_validate_fd_bindings(config);
    if (status != HL_STATUS_OK) return status;
    candidate_handles = malloc(config->fd_binding_count * sizeof(*candidate_handles));
    if (candidate_handles == NULL) return HL_STATUS_OUT_OF_MEMORY;
    for (index = 0; index < config->fd_binding_count; ++index)
        candidate_handles[index] = HL_HOST_HANDLE_INVALID;
    for (index = 0; index < config->fd_binding_count; ++index) {
        const hl_engine_fd_binding *binding = &config->fd_bindings[index];
        hl_host_result cloned = engine->host.file->clone_for_fork(engine->host.context, binding->host_handle);
        if (cloned.status != HL_STATUS_OK || cloned.value == HL_HOST_HANDLE_INVALID) {
            status = cloned.status == HL_STATUS_OK ? HL_STATUS_PLATFORM_FAILURE : (hl_status)cloned.status;
            goto cleanup;
        }
        candidate_handles[index] = cloned.value;
        status = hl_linux_fd_install_at(&engine->box, binding->guest_fd, candidate_handles[index],
                                        binding->status_flags, binding->descriptor_flags);
        if (status != HL_STATUS_OK) goto cleanup;
        candidate_handles[index] = HL_HOST_HANDLE_INVALID;
    }
    for (index = 0; index < config->fd_binding_count; ++index) {
        const hl_engine_fd_binding *binding = &config->fd_bindings[index];
        if (binding->ownership == HL_ENGINE_FD_TRANSFER)
            (void)engine->host.file->close(engine->host.context, binding->host_handle);
    }
    engine->config.fd_bindings = NULL;
    engine->config.fd_binding_count = 0;
    status = HL_STATUS_OK;
cleanup:
    for (index = 0; index < config->fd_binding_count; ++index)
        if (candidate_handles[index] != HL_HOST_HANDLE_INVALID)
            (void)engine->host.file->close(engine->host.context, candidate_handles[index]);
    free(candidate_handles);
    return status;
}

static void hl_engine_create_cleanup(hl_engine *engine) {
    uint32_t fd;
    size_t index;
    if (engine == NULL) return;
    if (engine->box_initialized) {
        for (fd = 0; fd < engine->box.fd_capacity; ++fd) {
            hl_host_handle handle;
            if (hl_linux_fd_close(&engine->box, fd, &handle) == HL_STATUS_OK && handle != HL_HOST_HANDLE_INVALID)
                (void)engine->host.file->close(engine->host.context, handle);
        }
        (void)hl_linux_abi_destroy(&engine->box);
    }
    free(engine->box_fds);
    free(engine->box_ofds);
    if (engine->options_initialized && engine->options_owned) hl_options_destroy(&engine->options);
    free(engine->owned_rootfs);
    if (engine->owned_executable_image != NULL) {
        memset(engine->owned_executable_image, 0, engine->executable_config.image_size);
        free(engine->owned_executable_image);
    }
    if (engine->owned_interpreter_image != NULL) {
        memset(engine->owned_interpreter_image, 0, engine->interpreter_image_size);
        free(engine->owned_interpreter_image);
    }
    if (engine->executable != HL_HOST_HANDLE_INVALID)
        (void)engine->host.file->close(engine->host.context, engine->executable);
    free(engine->owned_working_directory);
    free(engine->owned_hostname);
    free(engine->owned_environment);
    if (engine->checkpoint_broker >= 0) (void)close(engine->checkpoint_broker);
    if (engine->checkpoint_trigger >= 0) (void)close(engine->checkpoint_trigger);
    if (engine->checkpoint_control_parent >= 0) (void)close(engine->checkpoint_control_parent);
    if (engine->checkpoint_control_child >= 0) (void)close(engine->checkpoint_control_child);
    for (index = 0; index < sizeof(engine->owned_box_strings) / sizeof(engine->owned_box_strings[0]); ++index)
        free(engine->owned_box_strings[index]);
    free(engine->owned_publish);
    free(engine);
}

static hl_status hl_engine_create_with_options_mode(const hl_engine_config *config, const hl_host_services *host,
                                                    const hl_options *source_options, uint32_t borrow_options,
                                                    const void *interpreter_image, size_t interpreter_size,
                                                    hl_engine **out_engine) {
    hl_engine *engine;
    hl_status status;
    status = hl_engine_create_validate(config, host, source_options, borrow_options, out_engine);
    if (status != HL_STATUS_OK) return status;
    status = hl_engine_interpreter_validate(config, interpreter_image, interpreter_size);
    if (status != HL_STATUS_OK) return status;
    engine = hl_engine_allocate(config, host);
    if (engine == NULL) return HL_STATUS_OUT_OF_MEMORY;
    status = hl_engine_copy_image_plan(engine, config, interpreter_image, interpreter_size);
    if (status != HL_STATUS_OK) goto fail;
    status = hl_engine_pin_executable(engine, config, host);
    if (status != HL_STATUS_OK) goto fail;
    status = hl_engine_initialize_options(engine, config, source_options, borrow_options);
    if (status != HL_STATUS_OK) goto fail;
    status = hl_engine_initialize_linux_abi(engine);
    if (status != HL_STATUS_OK) goto fail;
    status = hl_engine_install_fd_bindings(engine, config, host);
    if (status != HL_STATUS_OK) goto fail;
    atomic_flag_clear(&engine->lock);
    atomic_init(&engine->checkpoint_control_lock, false);
    engine->backend = production_backends[config->guest_isa];
    *out_engine = engine;
    if (config->executable != NULL && config->executable->ownership == HL_ENGINE_FD_TRANSFER)
        (void)host->file->close(host->context, config->executable->host_handle);
    return HL_STATUS_OK;
fail:
    hl_engine_create_cleanup(engine);
    return status;
}

hl_status hl_engine_create_with_options(const hl_engine_config *config, const hl_host_services *host,
                                        const hl_options *source_options, hl_engine **out_engine) {
    return hl_engine_create_with_options_mode(config, host, source_options, 0, NULL, 0, out_engine);
}

hl_status hl_engine_create_with_borrowed_options(const hl_engine_config *config, const hl_host_services *host,
                                                 const hl_options *source_options, hl_engine **out_engine) {
    return hl_engine_create_with_options_mode(config, host, source_options, 1, NULL, 0, out_engine);
}

hl_status
hl_engine_create_with_borrowed_options_and_syscall_trap(const hl_engine_config *config, const hl_host_services *host,
                                                        const hl_options *source_options, void *syscall_context,
                                                        hl_syscall_trap_fn syscall_dispatch, hl_engine **out_engine) {
    return hl_engine_create_with_borrowed_options_and_syscall_trap_and_interpreter(
        config, host, source_options, syscall_context, syscall_dispatch, NULL, 0, out_engine);
}

hl_status hl_engine_create_with_borrowed_options_and_syscall_trap_and_interpreter(
    const hl_engine_config *config, const hl_host_services *host, const hl_options *source_options,
    void *syscall_context, hl_syscall_trap_fn syscall_dispatch, const void *interpreter_image, size_t interpreter_size,
    hl_engine **out_engine) {
    hl_status status = hl_engine_create_with_options_mode(config, host, source_options, 1, interpreter_image,
                                                          interpreter_size, out_engine);
    if (status == HL_STATUS_OK) {
        (*out_engine)->syscall_context = syscall_context;
        (*out_engine)->syscall_dispatch = syscall_dispatch;
    }
    return status;
}

hl_status hl_engine_create(const hl_engine_config *config, const hl_host_services *host, hl_engine **out_engine) {
    return hl_engine_create_with_options(config, host, NULL, out_engine);
}

static hl_status hl_engine_run_validate(hl_engine *engine, int argc, const char *const argv[],
                                        hl_engine_exit *out_exit) {
    if (out_exit == NULL) return HL_STATUS_INVALID_ARGUMENT;
    if (out_exit->abi != HL_ENGINE_ABI || out_exit->size < sizeof(*out_exit)) return HL_STATUS_ABI_MISMATCH;
    out_exit->kind = HL_ENGINE_EXIT_NONE;
    out_exit->guest_status = 0;
    out_exit->detail = 0;
    if (engine == NULL || argc < 0 || (argc != 0 && argv == NULL)) return HL_STATUS_INVALID_ARGUMENT;
    return HL_STATUS_OK;
}

hl_status hl_engine_run(hl_engine *engine, int argc, const char *const argv[], hl_engine_exit *out_exit) {
    hl_host_result waited;
    hl_host_result closed;
    hl_host_handle process = HL_HOST_HANDLE_INVALID;
    hl_host_handle process_result = HL_HOST_HANDLE_INVALID;
    uint32_t pending;
    hl_status status;
    status = hl_engine_run_validate(engine, argc, argv, out_exit);
    if (status != HL_STATUS_OK) return status;
    hl_engine_lock(engine);
    if (engine->state != HL_ENGINE_CREATED) {
        hl_engine_unlock(engine);
        return HL_STATUS_BUSY;
    }
    engine->state = HL_ENGINE_STARTING;
    hl_engine_unlock(engine);
    out_exit->kind = HL_ENGINE_EXIT_ENGINE_ERROR;
    out_exit->guest_status = HL_STATUS_NOT_SUPPORTED;
    out_exit->detail = engine->config.guest_isa;
    if (engine->backend == NULL || engine->backend->guest_isa != engine->config.guest_isa ||
        engine->backend->start_process == NULL) {
        hl_engine_lock(engine);
        engine->state = HL_ENGINE_FINISHED;
        hl_engine_unlock(engine);
        return HL_STATUS_NOT_SUPPORTED;
    }
    status = engine->backend->start_process(
        &engine->host, engine->box_initialized ? &engine->box : NULL, &engine->options, &engine->config, (uint32_t)argc,
        argv, engine->syscall_context, engine->syscall_dispatch, engine->checkpoint_broker, engine->checkpoint_trigger,
        engine->checkpoint_control_child, engine->owned_interpreter_image,
        engine->owned_interpreter_image == NULL ? 0 : engine->interpreter_image_size, &process, &process_result);
    if (status != HL_STATUS_OK) {
        hl_engine_lock(engine);
        engine->state = HL_ENGINE_FINISHED;
        hl_engine_unlock(engine);
        return status;
    }
    hl_engine_lock(engine);
    engine->process = process;
    engine->process_result = process_result;
    if (engine->state != HL_ENGINE_DESTROYING) engine->state = HL_ENGINE_RUNNING;
    pending = engine->pending_termination;
    hl_engine_unlock(engine);
    if (pending != 0) engine->host.process->terminate(engine->host.context, process, pending);
    if (engine->checkpoint_control_parent >= 0) {
#if defined(HL_NATIVE_TEST_HOOKS)
        hl_engine_checkpoint_test_process_started();
#endif
        hl_engine_checkpoint_control_lock(engine);
        status = hl_engine_checkpoint_control_ready(engine);
        hl_engine_checkpoint_control_unlock(engine);
#if defined(HL_NATIVE_TEST_HOOKS)
        hl_engine_checkpoint_test_run_ready();
#endif
        if (status != HL_STATUS_OK)
            (void)engine->host.process->terminate(engine->host.context, process, HL_HOST_PROCESS_TERMINATE_FORCE);
    }
    waited = engine->host.process->wait(engine->host.context, process, HL_HOST_DEADLINE_INFINITE);
    hl_engine_checkpoint_arena_stop(engine);
    hl_engine_lock(engine);
    engine->process = HL_HOST_HANDLE_INVALID;
    /* Withdrawn before finish_process, which consumes the token: a reader that outlived the guest must
       find no handle rather than a released one. */
    engine->process_result = HL_HOST_HANDLE_INVALID;
    hl_engine_unlock(engine);
    closed = engine->host.process->close(engine->host.context, process);
    if (waited.status != HL_STATUS_OK) {
        status = (hl_status)waited.status;
    } else if (closed.status != HL_STATUS_OK) {
        status = (hl_status)closed.status;
    } else if (engine->backend->finish_process != NULL && process_result != HL_HOST_HANDLE_INVALID) {
        status =
            engine->backend->finish_process(&engine->host, process_result, &waited, out_exit, &engine->translations);
        process_result = HL_HOST_HANDLE_INVALID;
    } else {
        out_exit->detail = 0;
        if (waited.detail == HL_HOST_PROCESS_EXIT_CODE) {
            out_exit->kind = HL_ENGINE_EXIT_CODE;
            out_exit->guest_status = (int32_t)waited.value;
            status = HL_STATUS_OK;
        } else if (waited.detail == HL_HOST_PROCESS_EXIT_SIGNAL) {
            out_exit->kind = HL_ENGINE_EXIT_SIGNAL;
            out_exit->guest_status = (int32_t)waited.value;
            status = HL_STATUS_OK;
        } else {
            out_exit->guest_status = HL_STATUS_CORRUPT;
            status = HL_STATUS_CORRUPT;
        }
    }
    if (process_result != HL_HOST_HANDLE_INVALID && engine->backend->release_process_result != NULL)
        engine->backend->release_process_result(&engine->host, process_result);
    hl_engine_lock(engine);
    engine->state = HL_ENGINE_FINISHED;
    hl_engine_unlock(engine);
#if defined(HL_NATIVE_TEST_HOOKS)
    {
        unsigned int expected = 1;
        if (atomic_compare_exchange_strong_explicit(&engine_finish_test_phase, &expected, 2, memory_order_acq_rel,
                                                    memory_order_acquire))
            while (atomic_load_explicit(&engine_finish_test_phase, memory_order_acquire) != 3)
                hl_engine_yield(engine);
    }
#endif
    return status;
}

int32_t hl_engine_guest_pid(hl_engine *engine) {
    hl_host_handle token;
    int32_t guest_pid = 0;
    if (engine == NULL) return 0;
    hl_engine_lock(engine);
    token = engine->process_result;
    if (token != HL_HOST_HANDLE_INVALID && engine->backend != NULL && engine->backend->process_guest_pid != NULL)
        guest_pid = engine->backend->process_guest_pid(token);
    hl_engine_unlock(engine);
    return guest_pid;
}

hl_status hl_engine_request(hl_engine *engine, uint32_t request, const void *data, size_t data_size) {
    uint32_t reason;
    hl_host_handle process;
    hl_status status;
    if (engine == NULL || (data_size != 0 && data == NULL)) return HL_STATUS_INVALID_ARGUMENT;
    if (request == HL_ENGINE_REQUEST_CHECKPOINT_PRIVATE) {
#if defined(_WIN32)
        return HL_STATUS_NOT_SUPPORTED;
#else
        uint32_t signal_number;
        if (data == NULL || data_size != sizeof(signal_number)) return HL_STATUS_INVALID_ARGUMENT;
        memcpy(&signal_number, data, sizeof(signal_number));
        if (signal_number == 0 || signal_number > 64) return HL_STATUS_INVALID_ARGUMENT;
        if (engine->checkpoint_control_parent < 0) return HL_STATUS_NOT_SUPPORTED;
        hl_engine_checkpoint_control_lock(engine);
#if defined(HL_NATIVE_TEST_HOOKS)
        hl_engine_checkpoint_test_pause(engine);
#endif
        status = hl_engine_checkpoint_control_ready(engine);
        if (status == HL_STATUS_OK) {
            unsigned char command = 1;
            ssize_t written;
            do {
                written = write(engine->checkpoint_control_parent, &command, sizeof(command));
            } while (written < 0 && errno == EINTR);
            if (written == (ssize_t)sizeof(command)) {
                unsigned char interrupted = 0;
                struct pollfd waiting = {.fd = engine->checkpoint_control_parent, .events = POLLIN};
                int ready;
                do {
                    ready = poll(&waiting, 1, 5000);
                } while (ready < 0 && errno == EINTR);
                if (ready <= 0 || read(waiting.fd, &interrupted, sizeof(interrupted)) != (ssize_t)sizeof(interrupted) ||
                    interrupted == 0)
                    status = HL_STATUS_PLATFORM_FAILURE;
            } else {
                status = HL_STATUS_PLATFORM_FAILURE;
            }
        }
        hl_engine_checkpoint_control_unlock(engine);
#if defined(HL_NATIVE_TEST_HOOKS)
        hl_engine_checkpoint_test_request_completed();
#endif
        if (status != HL_STATUS_OK) return status;
        reason = HL_HOST_PROCESS_TERMINATE_NATIVE_SIGNAL + signal_number;
#endif
    } else if (request == HL_ENGINE_REQUEST_SIGNAL) {
        uint32_t signal_number;
        if (data == NULL || data_size != sizeof(signal_number)) return HL_STATUS_INVALID_ARGUMENT;
        memcpy(&signal_number, data, sizeof(signal_number));
        if (signal_number == 0 || signal_number > 64 || signal_number == 9) return HL_STATUS_INVALID_ARGUMENT;
        reason = HL_HOST_PROCESS_TERMINATE_SIGNAL + signal_number;
    } else if (data_size != 0) {
        return HL_STATUS_INVALID_ARGUMENT;
    } else if (request == HL_ENGINE_REQUEST_INTERRUPT)
        reason = HL_HOST_PROCESS_TERMINATE_INTERRUPT;
    else if (request == HL_ENGINE_REQUEST_FORCE_STOP)
        reason = HL_HOST_PROCESS_TERMINATE_FORCE;
    else
        return HL_STATUS_NOT_SUPPORTED;
    hl_engine_lock(engine);
    if (engine->state == HL_ENGINE_FINISHED) {
        hl_engine_unlock(engine);
        return HL_STATUS_OK;
    }
    if (engine->state == HL_ENGINE_DESTROYING) {
        hl_engine_unlock(engine);
        return HL_STATUS_BUSY;
    }
    /* HL_ENGINE_CREATED is accepted, not rejected: a caller that signals between
     * create and run must not have that signal silently discarded.  hl_engine_run
     * replays pending_termination once the guest process exists. */
    engine->pending_termination = reason;
    process = engine->process;
    hl_engine_unlock(engine);
    if (process == HL_HOST_HANDLE_INVALID) return HL_STATUS_OK;
    if (reason == HL_HOST_PROCESS_TERMINATE_FORCE) hl_engine_checkpoint_arena_stop(engine);
    status = (hl_status)engine->host.process->terminate(engine->host.context, process, reason).status;
    if (status == HL_STATUS_INVALID_ARGUMENT) {
        hl_engine_lock(engine);
        if (engine->state == HL_ENGINE_FINISHED) status = HL_STATUS_OK;
        hl_engine_unlock(engine);
    }
    return status;
}

#if defined(HL_NATIVE_TEST_HOOKS)
static hl_host_result hl_engine_finish_test_terminate(void *context, hl_host_handle process, uint32_t reason) {
    hl_engine *engine = context;
    (void)process;
    (void)reason;
    hl_engine_lock(engine);
    engine->state = HL_ENGINE_FINISHED;
    hl_engine_unlock(engine);
    return (hl_host_result){.status = HL_STATUS_INVALID_ARGUMENT};
}

HL_API int hl_c_backend_engine_request_state_test(uint32_t scenario) {
    hl_engine engine = {0};
    static const hl_host_process_services process_services = {
        .abi = HL_HOST_PROCESS_ABI,
        .size = sizeof(hl_host_process_services),
        .terminate = hl_engine_finish_test_terminate,
    };
    atomic_flag_clear_explicit(&engine.lock, memory_order_release);
    if (scenario == 0) {
        engine.state = HL_ENGINE_DESTROYING;
    } else if (scenario == 1) {
        engine.state = HL_ENGINE_RUNNING;
        engine.process = 1;
        engine.host.context = &engine;
        engine.host.process = &process_services;
    } else {
        return -1;
    }
    return (int)hl_engine_request(&engine, HL_ENGINE_REQUEST_FORCE_STOP, NULL, 0);
}
#endif

void hl_engine_destroy(hl_engine *engine) {
    hl_host_handle process;
    uint32_t fd;
    if (engine == NULL) return;
    hl_engine_lock(engine);
    if (engine->state == HL_ENGINE_STARTING || engine->state == HL_ENGINE_RUNNING) {
        engine->state = HL_ENGINE_DESTROYING;
        engine->pending_termination = HL_HOST_PROCESS_TERMINATE_FORCE;
        process = engine->process;
        hl_engine_unlock(engine);
        hl_engine_checkpoint_arena_stop(engine);
        if (process != HL_HOST_HANDLE_INVALID)
            (void)engine->host.process->terminate(engine->host.context, process, HL_HOST_PROCESS_TERMINATE_FORCE);
        for (;;) {
            uint32_t state;
            hl_engine_lock(engine);
            state = engine->state;
            hl_engine_unlock(engine);
            if (state == HL_ENGINE_FINISHED) break;
            hl_engine_yield(engine);
        }
    } else {
        engine->state = HL_ENGINE_DESTROYING;
        hl_engine_unlock(engine);
    }
    if (engine->box_initialized) {
        for (fd = 0; fd < engine->box.fd_capacity; ++fd)
            (void)hl_linux_close(&engine->box, fd);
        (void)hl_linux_abi_destroy(&engine->box);
    }
    free(engine->box_fds);
    free(engine->box_ofds);
    if (engine->options_initialized && engine->options_owned) hl_options_destroy(&engine->options);
    free(engine->owned_rootfs);
    if (engine->owned_executable_image != NULL) {
        memset(engine->owned_executable_image, 0, engine->executable_config.image_size);
        free(engine->owned_executable_image);
    }
    if (engine->owned_interpreter_image != NULL) {
        memset(engine->owned_interpreter_image, 0, engine->interpreter_image_size);
        free(engine->owned_interpreter_image);
    }
    if (engine->executable != HL_HOST_HANDLE_INVALID)
        (void)engine->host.file->close(engine->host.context, engine->executable);
    free(engine->owned_working_directory);
    free(engine->owned_hostname);
    free(engine->owned_environment);
    if (engine->checkpoint_broker >= 0) (void)close(engine->checkpoint_broker);
    if (engine->checkpoint_trigger >= 0) (void)close(engine->checkpoint_trigger);
    if (engine->checkpoint_control_parent >= 0) (void)close(engine->checkpoint_control_parent);
    if (engine->checkpoint_control_child >= 0) (void)close(engine->checkpoint_control_child);
    {
        size_t index;
        for (index = 0; index < sizeof(engine->owned_box_strings) / sizeof(engine->owned_box_strings[0]); ++index)
            free(engine->owned_box_strings[index]);
        free(engine->owned_publish);
    }
    free(engine);
}
