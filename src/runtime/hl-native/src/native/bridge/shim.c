#include "../engine/checkpoint_channel.h"
#include "../engine/backend.h"
#include "../engine/options.h"
#include "executable_authority.h"
#include "api.h"
#include "hl/engine.h"
#include "hl/linux_abi.h"
#include "hl/syscall_trap.h"
#include "../host/system.h"
#include "../host/process.h"
#include "main_plan.h"
#include "host.h"

#include <fcntl.h>
#include <errno.h>
/* `poll` has exactly one caller here, the __APPLE__ arm of
   `hl_c_backend_process_identity_signal`. Windows ships no <poll.h>. */
#if defined(__APPLE__)
#include <poll.h>
#endif
#include <stdbool.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#if defined(__linux__)
#include <sys/socket.h>
#include <sys/syscall.h>
#endif
#include <unistd.h>

#if defined(_WIN32)
#define HL_C_OPEN_CLOEXEC 0

static ssize_t hl_c_backend_pread(int fd, void *buffer, size_t size, off_t offset) {
    off_t saved = lseek(fd, 0, SEEK_CUR);
    if (saved < 0 || lseek(fd, offset, SEEK_SET) < 0) return -1;
    ssize_t result = read(fd, buffer, size);
    if (lseek(fd, saved, SEEK_SET) < 0) return -1;
    return result;
}
#else
#define HL_C_OPEN_CLOEXEC O_CLOEXEC
#define hl_c_backend_pread pread
#endif

/* Explicitly anchors the lifecycle object when the retained backend is linked
 * from static archives. The function is idempotent and replaces reliance on
 * linker-specific constructor extraction. */
extern void hl_aarch64_target_register_backend(void);
extern void hl_x86_64_target_register_backend(void);

struct hl_c_backend {
    hl_c_bridge_host *host;
    hl_host_services services;
    hl_engine *engine;
    hl_options options;
    uint32_t options_initialized;
    atomic_bool result_lock;
    hl_engine_exit result;
};

#if defined(HL_NATIVE_TEST_HOOKS)
HL_API uint32_t hl_c_backend_engine_finish_test_arm(hl_c_backend *backend) {
    return backend == NULL ? 0 : hl_engine_finish_test_arm(backend->engine);
}

HL_API uint32_t hl_c_backend_engine_finish_test_phase(hl_c_backend *backend) {
    return backend == NULL ? 0 : hl_engine_finish_test_phase(backend->engine);
}

HL_API void hl_c_backend_engine_finish_test_release(hl_c_backend *backend) {
    if (backend != NULL) hl_engine_finish_test_release(backend->engine);
}
#endif

static void hl_c_backend_result_lock(hl_c_backend *backend) {
    while (atomic_exchange_explicit(&backend->result_lock, true, memory_order_acquire)) {}
}

static void hl_c_backend_result_unlock(hl_c_backend *backend) {
    atomic_store_explicit(&backend->result_lock, false, memory_order_release);
}

#if defined(HL_LEAK_CHECK_PROBE)
/* Non-vacuity hook for the dedicated sanitizer gate. It is compiled only into
 * sanitizer artifacts and activated only by the gate's probe subprocess. */
static void *volatile hl_leak_check_probe;

static void hl_c_backend_leak_check_probe(void) {
    if (getenv("HL_LEAK_CHECK_PROBE") != NULL && hl_leak_check_probe == NULL) {
        hl_leak_check_probe = malloc(4096);
        hl_leak_check_probe = NULL;
    }
}

#if defined(HL_LEAK_SANITIZER)
extern int __lsan_do_recoverable_leak_check(void);

static void hl_c_backend_leak_check_verdict(void) {
    if (getenv("HL_LEAK_CHECK_PROBE") != NULL && __lsan_do_recoverable_leak_check() != 0) _exit(97);
}
#else
static void hl_c_backend_leak_check_verdict(void) {
}
#endif
#else
static void hl_c_backend_leak_check_probe(void) {
}

static void hl_c_backend_leak_check_verdict(void) {
}
#endif

#if defined(HL_LEAK_CHECK_PROBE)
__attribute__((noinline)) static void hl_c_backend_make_deliberate_leak(void) {
    hl_leak_check_probe = malloc(4096);
    hl_leak_check_probe = NULL;
}

__attribute__((noinline)) static void hl_c_backend_scrub_probe_stack(void) {
    volatile uintptr_t scrub[8192];
    memset((void *)scrub, 0, sizeof(scrub));
}
#endif

HL_API int32_t hl_c_backend_leak_check_nonvacuity(void) {
#if defined(HL_ADDRESS_SANITIZER)
    volatile unsigned char *allocation = malloc(4096);
    if (allocation == NULL) return 1;
    allocation[0] = 0x5a;
    free((void *)allocation);
    return allocation[0];
#elif defined(HL_LEAK_CHECK_PROBE)
    hl_c_backend_make_deliberate_leak();
    hl_c_backend_scrub_probe_stack();
    return 0;
#else
    return 0;
#endif
}

#if defined(HL_NATIVE_TEST_HOOKS)
extern int hl_linux_errno_from_host(int host_errno);
extern int hl_linux_errno_from_darwin(int host_errno);
extern int hl_linux_errno_from_ucrt(int host_errno);

HL_API int32_t hl_c_backend_errno_from_host_test(uint32_t domain, int32_t host_errno) {
    if (domain == 1) return hl_linux_errno_from_darwin(host_errno);
    if (domain == 2) return hl_linux_errno_from_ucrt(host_errno);
    return hl_linux_errno_from_host(host_errno);
}
#endif

#if defined(HL_BUILD_TARGET_X86_64_ONLY)
#define HL_BRIDGE_CKPT(name) hl_x86_64_ckpt_##name
#else
#define HL_BRIDGE_CKPT(name) hl_aarch64_ckpt_##name
#endif

extern int HL_BRIDGE_CKPT(broker_pair)(hl_activation_descriptor *, hl_activation_descriptor *);
extern hl_activation_descriptor HL_BRIDGE_CKPT(broker_accept)(hl_activation_descriptor, int, uint64_t *);
extern int HL_BRIDGE_CKPT(channel_authenticate_peer)(int, uint64_t, uint64_t *);
#if defined(HL_NATIVE_TEST_HOOKS)
extern void HL_BRIDGE_CKPT(channel_publish)(int);
extern int HL_BRIDGE_CKPT(channel_acquire)(void);
extern void HL_BRIDGE_CKPT(channel_forget_for_test)(void);
#endif
extern int HL_BRIDGE_CKPT(trigger_create)(hl_activation_descriptor *, void **);
extern uint32_t HL_BRIDGE_CKPT(trigger_bump)(void *);
extern void HL_BRIDGE_CKPT(trigger_destroy)(void *, hl_activation_descriptor);

HL_API int32_t hl_c_backend_checkpoint_broker_pair(int32_t *parent, int32_t *child) {
    hl_activation_descriptor parent_descriptor = HL_ACTIVATION_DESCRIPTOR_NONE;
    hl_activation_descriptor child_descriptor = HL_ACTIVATION_DESCRIPTOR_NONE;
    if (parent == NULL || child == NULL) return HL_STATUS_INVALID_ARGUMENT;
    *parent = -1;
    *child = -1;
    if (HL_BRIDGE_CKPT(broker_pair)(&parent_descriptor, &child_descriptor) != 0 || parent_descriptor > INT32_MAX ||
        child_descriptor > INT32_MAX)
        return HL_STATUS_PLATFORM_FAILURE;
    *parent = (int32_t)parent_descriptor;
    *child = (int32_t)child_descriptor;
    return HL_STATUS_OK;
}

HL_API int32_t hl_c_backend_checkpoint_broker_accept(int32_t broker, int32_t timeout_ms, uint64_t *host_pid) {
    hl_activation_descriptor channel;
    if (broker < 0 || timeout_ms < 0) return -1;
    channel = HL_BRIDGE_CKPT(broker_accept)((hl_activation_descriptor)broker, timeout_ms, host_pid);
    return channel == HL_ACTIVATION_DESCRIPTOR_NONE || channel > INT32_MAX ? -1 : (int32_t)channel;
}

HL_API int32_t hl_c_backend_checkpoint_broker_accept_authenticated(int32_t broker, int32_t timeout_ms,
                                                                   uint64_t *host_pid, uint64_t *host_birth,
                                                                   uint64_t *host_generation, int32_t *process_handle) {
    hl_activation_descriptor channel;
#if defined(__linux__)
    hl_host_process_info process;
#endif
    if (host_pid != NULL) *host_pid = 0;
    if (host_birth != NULL) *host_birth = 0;
    if (host_generation != NULL) *host_generation = 0;
    if (process_handle != NULL) *process_handle = -1;
    if (broker < 0 || timeout_ms < 0 || host_pid == NULL || host_birth == NULL || host_generation == NULL ||
        process_handle == NULL)
        return -1;
    channel = HL_BRIDGE_CKPT(broker_accept)((hl_activation_descriptor)broker, timeout_ms, host_pid);
#if defined(__linux__)
    if (channel != HL_ACTIVATION_DESCRIPTOR_NONE && channel <= INT32_MAX) {
        socklen_t handle_size = (socklen_t)sizeof *process_handle;
#ifndef SO_PEERPIDFD
#define SO_PEERPIDFD 77
#endif
        if (getsockopt((int)channel, SOL_SOCKET, SO_PEERPIDFD, process_handle, &handle_size) != 0 ||
            handle_size != (socklen_t)sizeof *process_handle || *process_handle < 0)
            *process_handle = -1;
    }
    if (channel == HL_ACTIVATION_DESCRIPTOR_NONE || channel > INT32_MAX || *process_handle < 0 ||
        *host_pid > INT64_MAX || syscall(SYS_pidfd_send_signal, *process_handle, 0, NULL, 0) != 0 ||
        !hl_host_process_read((int64_t)*host_pid, &process) || process.start_time_ns == 0 ||
        syscall(SYS_pidfd_send_signal, *process_handle, 0, NULL, 0) != 0) {
        if (channel != HL_ACTIVATION_DESCRIPTOR_NONE && channel <= INT32_MAX) (void)close((int)channel);
        if (*process_handle >= 0) (void)close(*process_handle);
        *process_handle = -1;
        *host_pid = 0;
        return -1;
    }
#elif defined(__APPLE__)
    if (channel == HL_ACTIVATION_DESCRIPTOR_NONE || channel > INT32_MAX || *host_pid > INT64_MAX) {
        if (channel != HL_ACTIVATION_DESCRIPTOR_NONE && channel <= INT32_MAX) (void)close((int)channel);
        *host_pid = 0;
        return -1;
    }
    *process_handle =
        hl_host_process_peer_identity_open((int)channel, *host_pid, host_pid, host_birth, host_generation);
    if (*process_handle < 0) {
        (void)close((int)channel);
        *host_pid = 0;
        return -1;
    }
#else
    if (channel != HL_ACTIVATION_DESCRIPTOR_NONE && channel <= INT32_MAX) (void)close((int)channel);
    *host_pid = 0;
    return -1;
#endif
#if defined(__linux__)
    *host_birth = process.start_time_ns;
#endif
    return (int32_t)channel;
}

HL_API int32_t hl_c_backend_checkpoint_peer_authenticate_test(int32_t descriptor, uint64_t claimed_pid,
                                                              uint64_t *host_pid, uint64_t *host_birth) {
    hl_host_process_info process;
    if (host_pid != NULL) *host_pid = 0;
    if (host_birth != NULL) *host_birth = 0;
    if (descriptor < 0 || host_pid == NULL || host_birth == NULL ||
        HL_BRIDGE_CKPT(channel_authenticate_peer)(descriptor, claimed_pid, host_pid) != 0 || *host_pid > INT64_MAX ||
        !hl_host_process_read((int64_t)*host_pid, &process) || process.start_time_ns == 0) {
        if (host_pid != NULL) *host_pid = 0;
        return -1;
    }
    *host_birth = process.start_time_ns;
    return 0;
}

#if defined(HL_NATIVE_TEST_HOOKS) && !defined(_WIN32)
HL_API int32_t hl_c_backend_checkpoint_channel_connect_test(int32_t broker_child) {
    if (broker_child < 0) {
        errno = EINVAL;
        return -1;
    }
    HL_BRIDGE_CKPT(channel_publish)(broker_child);
    /* Each call mints a channel the caller then owns and closes, so the per-process cache must not
       hand the next caller the descriptor the previous one already closed. */
    HL_BRIDGE_CKPT(channel_forget_for_test)();
    return HL_BRIDGE_CKPT(channel_acquire)();
}
#else
HL_API int32_t hl_c_backend_checkpoint_channel_connect_test(int32_t broker_child) {
    (void)broker_child;
    errno = ENOTSUP;
    return -1;
}
#endif
HL_API int32_t hl_c_backend_checkpoint_process_identity_open_test(int32_t pid, uint64_t expected_birth,
                                                                  uint64_t expected_generation, uint64_t *actual_birth,
                                                                  uint64_t *actual_generation) {
#if defined(__APPLE__)
    return hl_host_process_identity_open((pid_t)pid, expected_birth, expected_generation, actual_birth,
                                         actual_generation);
#else
    (void)pid;
    (void)expected_birth;
    (void)expected_generation;
    if (actual_birth != NULL) *actual_birth = 0;
    if (actual_generation != NULL) *actual_generation = 0;
    return -1;
#endif
}

HL_API int32_t hl_c_backend_checkpoint_peer_identity_open_test(int32_t descriptor, uint64_t claimed_pid,
                                                               uint64_t *actual_pid, uint64_t *actual_birth,
                                                               uint64_t *actual_generation) {
#if defined(__APPLE__)
    return hl_host_process_peer_identity_open(descriptor, claimed_pid, actual_pid, actual_birth, actual_generation);
#else
    (void)descriptor;
    (void)claimed_pid;
    if (actual_pid != NULL) *actual_pid = 0;
    if (actual_birth != NULL) *actual_birth = 0;
    if (actual_generation != NULL) *actual_generation = 0;
    return -1;
#endif
}

HL_API int32_t hl_c_backend_checkpoint_trigger_create(int32_t *descriptor, void **mapping) {
    hl_activation_descriptor native_descriptor = HL_ACTIVATION_DESCRIPTOR_NONE;
    if (descriptor == NULL || mapping == NULL) return HL_STATUS_INVALID_ARGUMENT;
    *descriptor = -1;
    *mapping = NULL;
    if (HL_BRIDGE_CKPT(trigger_create)(&native_descriptor, mapping) != 0 || native_descriptor > INT32_MAX)
        return HL_STATUS_PLATFORM_FAILURE;
    *descriptor = (int32_t)native_descriptor;
    return HL_STATUS_OK;
}

HL_API uint32_t hl_c_backend_checkpoint_trigger_bump(void *mapping) {
    return HL_BRIDGE_CKPT(trigger_bump)(mapping);
}

HL_API void hl_c_backend_checkpoint_trigger_destroy(void *mapping, int32_t descriptor) {
    HL_BRIDGE_CKPT(trigger_destroy)(mapping, descriptor < 0 ? HL_ACTIVATION_DESCRIPTOR_NONE
                                                            : (hl_activation_descriptor)descriptor);
}

extern int hl_aarch64_ckpt_channel_adopt(const char *broker, const char *trigger);
extern int hl_x86_64_ckpt_channel_adopt(const char *broker, const char *trigger);

HL_API int32_t hl_c_backend_checkpoint_adopt(uint32_t isa, int32_t broker, int32_t trigger) {
#if defined(_WIN32)
    if ((isa != 1 && isa != 2) || broker < 0 || trigger < 0) return HL_STATUS_INVALID_ARGUMENT;
    /* The Windows checkpoint channel is deliberately unavailable until its
     * named-pipe and DuplicateHandle transport exists.  Keep the ABI present,
     * but do not pretend POSIX descriptor adoption succeeded. */
    return HL_STATUS_PLATFORM_FAILURE;
#else
    char broker_text[32];
    char trigger_text[32];
    int broker_copy;
    int trigger_copy;
    if ((isa != 1 && isa != 2) || broker < 0 || trigger < 0) return HL_STATUS_INVALID_ARGUMENT;
    broker_copy = fcntl(broker, F_DUPFD_CLOEXEC, 3);
    if (broker_copy < 0) return HL_STATUS_PLATFORM_FAILURE;
    trigger_copy = fcntl(trigger, F_DUPFD_CLOEXEC, 3);
    if (trigger_copy < 0) {
        (void)close(broker_copy);
        return HL_STATUS_PLATFORM_FAILURE;
    }
    (void)snprintf(broker_text, sizeof(broker_text), "%d", broker_copy);
    (void)snprintf(trigger_text, sizeof(trigger_text), "%d", trigger_copy);
#if defined(HL_BUILD_TARGET_X86_64_ONLY)
    if (isa == 2 && hl_x86_64_ckpt_channel_adopt(broker_text, trigger_text) == 0)
#else
    if ((isa == 1 ? hl_aarch64_ckpt_channel_adopt(broker_text, trigger_text)
                  : hl_x86_64_ckpt_channel_adopt(broker_text, trigger_text)) == 0)
#endif
        return HL_STATUS_OK;
    (void)close(broker_copy);
    (void)close(trigger_copy);
    return HL_STATUS_PLATFORM_FAILURE;
#endif
}

extern int HL_BRIDGE_CKPT(interrupt_signal)(void);

/* The engine-owned per-terminal termios is `static` inside a per-guest-ISA namespaced translation
 * unit, so there are genuinely two stores and neither is authoritative on its own: a process runs one
 * engine, but which of the two holds an entry follows the guest's ISA. Both are asked, in a fixed
 * order, and the first hit answers -- the stores are keyed by terminal identity, so a terminal that is
 * in both would carry the same image anyway. */
extern uint64_t hl_x86_64_terminal_termios_generation(void);
extern int hl_x86_64_terminal_termios_image(int32_t native_fd, uint8_t *out);
extern int hl_x86_64_terminal_termios_capture(int32_t native_fd, uint8_t *out);
extern int hl_x86_64_terminal_termios_adopt(int32_t native_fd, const uint8_t *image);
extern uint64_t hl_x86_64_terminal_termios_flush_generation(int32_t native_fd);
extern int hl_x86_64_terminal_termios_flush_register(int32_t native_fd);
extern void hl_x86_64_terminal_termios_flush_unregister(int32_t native_fd);
extern uint64_t hl_x86_64_terminal_termios_flush_mark_test(int32_t native_fd, uint64_t request);
#if !defined(HL_BUILD_TARGET_X86_64_ONLY)
extern uint64_t hl_aarch64_terminal_termios_generation(void);
extern int hl_aarch64_terminal_termios_image(int32_t native_fd, uint8_t *out);
extern int hl_aarch64_terminal_termios_capture(int32_t native_fd, uint8_t *out);
extern int hl_aarch64_terminal_termios_adopt(int32_t native_fd, const uint8_t *image);
extern uint64_t hl_aarch64_terminal_termios_flush_generation(int32_t native_fd);
extern int hl_aarch64_terminal_termios_flush_register(int32_t native_fd);
extern void hl_aarch64_terminal_termios_flush_unregister(int32_t native_fd);
extern uint64_t hl_aarch64_terminal_termios_flush_mark_test(int32_t native_fd, uint64_t request);
#endif

HL_API uint64_t hl_c_backend_terminal_termios_generation(void) {
    /* A sum, not a max: either store advancing must move the total, and both only ever increase. */
#if defined(HL_BUILD_TARGET_X86_64_ONLY)
    return hl_x86_64_terminal_termios_generation();
#else
    return hl_aarch64_terminal_termios_generation() + hl_x86_64_terminal_termios_generation();
#endif
}

HL_API int32_t hl_c_backend_terminal_termios(int32_t native_fd, uint8_t *out) {
    if (out == NULL) return 0;
#if !defined(HL_BUILD_TARGET_X86_64_ONLY)
    if (hl_aarch64_terminal_termios_image(native_fd, out)) return 1;
#endif
    return hl_x86_64_terminal_termios_image(native_fd, out) ? 1 : 0;
}

/* The host's own termios as a Linux image. The two per-ISA translation units compile the identical
 * host read, so either arm answers for any terminal; the aarch64 arm is asked first only to keep the
 * order the accessors above use, and the x86_64 arm is the fallback that also serves an x86-only
 * build. */
HL_API int32_t hl_c_backend_terminal_termios_capture(int32_t native_fd, uint8_t *out) {
    if (out == NULL) return 0;
#if !defined(HL_BUILD_TARGET_X86_64_ONLY)
    if (hl_aarch64_terminal_termios_capture(native_fd, out)) return 1;
#endif
    return hl_x86_64_terminal_termios_capture(native_fd, out) ? 1 : 0;
}

/* Adopt a guest image against the host projection as it stands now, in every store this build has.
 * Both are written rather than the first that succeeds: which store a terminal's entry lives in
 * follows the guest ISA, which the pump does not know and must not have to. */
HL_API int32_t hl_c_backend_terminal_termios_adopt(int32_t native_fd, const uint8_t *image) {
    int32_t adopted = 0;
    if (image == NULL) return 0;
#if !defined(HL_BUILD_TARGET_X86_64_ONLY)
    if (hl_aarch64_terminal_termios_adopt(native_fd, image)) adopted = 1;
#endif
    if (hl_x86_64_terminal_termios_adopt(native_fd, image)) adopted = 1;
    return adopted;
}

HL_API uint64_t hl_c_backend_terminal_termios_flush_generation(int32_t native_fd) {
#if defined(HL_BUILD_TARGET_X86_64_ONLY)
    return hl_x86_64_terminal_termios_flush_generation(native_fd);
#else
    return hl_aarch64_terminal_termios_flush_generation(native_fd) +
           hl_x86_64_terminal_termios_flush_generation(native_fd);
#endif
}

HL_API int32_t hl_c_backend_terminal_termios_flush_register(int32_t native_fd) {
#if !defined(HL_BUILD_TARGET_X86_64_ONLY)
    if (!hl_aarch64_terminal_termios_flush_register(native_fd)) return 0;
    if (!hl_x86_64_terminal_termios_flush_register(native_fd)) {
        hl_aarch64_terminal_termios_flush_unregister(native_fd);
        return 0;
    }
    return 1;
#else
    return hl_x86_64_terminal_termios_flush_register(native_fd) ? 1 : 0;
#endif
}

HL_API void hl_c_backend_terminal_termios_flush_unregister(int32_t native_fd) {
#if !defined(HL_BUILD_TARGET_X86_64_ONLY)
    hl_aarch64_terminal_termios_flush_unregister(native_fd);
#endif
    hl_x86_64_terminal_termios_flush_unregister(native_fd);
}

HL_API uint64_t hl_c_backend_terminal_termios_flush_mark_test(int32_t native_fd, uint64_t request) {
#if defined(HL_NATIVE_TEST_HOOKS)
    if (request >= UINT64_MAX - 2) return hl_x86_64_terminal_termios_flush_mark_test(native_fd, request);
#endif
#if !defined(HL_BUILD_TARGET_X86_64_ONLY)
    (void)hl_aarch64_terminal_termios_flush_mark_test(native_fd, request);
#endif
    return hl_x86_64_terminal_termios_flush_mark_test(native_fd, request);
}

HL_API int32_t hl_c_backend_checkpoint_interrupt_signal(uint32_t isa) {
#if defined(HL_BUILD_TARGET_X86_64_ONLY)
    return isa == 2 ? HL_BRIDGE_CKPT(interrupt_signal)() : -1;
#else
    return isa == 1 || isa == 2 ? HL_BRIDGE_CKPT(interrupt_signal)() : -1;
#endif
}

HL_API int32_t hl_c_backend_checkpoint_configure(hl_c_backend *backend, int32_t broker, int32_t trigger) {
    return backend == NULL ? HL_STATUS_INVALID_ARGUMENT
                           : hl_engine_checkpoint_configure(backend->engine, broker, trigger);
}

static uint32_t hl_c_backend_status_flags(uint64_t detail) {
    uint32_t flags;
    const uint64_t access = detail & (HL_HOST_FILE_READ | HL_HOST_FILE_WRITE);
    if (access == (HL_HOST_FILE_READ | HL_HOST_FILE_WRITE))
        flags = HL_LINUX_O_RDWR;
    else if (access == HL_HOST_FILE_WRITE)
        flags = HL_LINUX_O_WRONLY;
    else
        flags = HL_LINUX_O_RDONLY;
    if ((detail & HL_HOST_FILE_APPEND) != 0) flags |= HL_LINUX_O_APPEND;
    if ((detail & HL_HOST_FILE_NONBLOCK) != 0) flags |= HL_LINUX_O_NONBLOCK;
    return flags;
}

static int hl_c_validate_main_image_plan(int fd, const hl_c_main_image_plan *plan) {
    uint8_t header[64];
    struct stat metadata;
    if (plan == NULL || plan->abi != HL_C_MAIN_IMAGE_PLAN_ABI || plan->size < sizeof(*plan) ||
        plan->architecture == 0 || (plan->flags & ~HL_ENGINE_MAIN_IMAGE_PLAN_FORCE_DISPLACED) != 0 ||
        plan->link_end <= plan->link_start || (plan->flags != 0 && plan->kind != HL_C_IMAGE_EXECUTABLE))
        return 0;
    if (fstat(fd, &metadata) != 0 || metadata.st_size < (off_t)sizeof(header) ||
        hl_c_backend_pread(fd, header, sizeof(header), 0) != (ssize_t)sizeof(header))
        return 0;
    if (memcmp(header, "\177ELF", 4) != 0 || header[4] != 2 || header[5] != 1 || header[6] != 1 ||
        (header[7] != 0 && header[7] != 3))
        return 0;
    uint16_t type, machine;
    uint32_t version;
    memcpy(&type, header + 16, sizeof(type));
    memcpy(&machine, header + 18, sizeof(machine));
    memcpy(&version, header + 20, sizeof(version));
    uint32_t kind = type == 2 ? HL_C_IMAGE_EXECUTABLE : type == 3 ? HL_C_IMAGE_POSITION_INDEPENDENT : 0;
    uint16_t expected_machine = plan->architecture == 1 ? 0xb7 : plan->architecture == 2 ? 0x3e : 0;
    uint64_t entry, phoff;
    uint16_t ehsize, phentsize, phnum;
    memcpy(&entry, header + 24, sizeof(entry));
    memcpy(&phoff, header + 32, sizeof(phoff));
    memcpy(&ehsize, header + 52, sizeof(ehsize));
    memcpy(&phentsize, header + 54, sizeof(phentsize));
    memcpy(&phnum, header + 56, sizeof(phnum));
    uint64_t image_length = (uint64_t)metadata.st_size;
    uint64_t table_size = (uint64_t)phentsize * phnum;
    if (kind != plan->kind || machine != expected_machine || version != 1 || ehsize != 64 || phentsize != 56 ||
        phnum == 0 || phnum > 1024 || phoff > image_length || table_size > image_length - phoff ||
        (plan->architecture == 1 && (entry & 3) != 0))
        return 0;
    uint64_t first = UINT64_MAX, last = 0;
    uint32_t has_interpreter = 0;
    uint64_t interpreter_identity = 0;
    uint16_t load_count = 0;
    int entry_is_executable = 0;
    for (uint16_t index = 0; index < phnum; ++index) {
        uint8_t ph[56];
        uint64_t offset = phoff + (uint64_t)index * phentsize;
        if (offset < phoff || hl_c_backend_pread(fd, ph, sizeof(ph), (off_t)offset) != (ssize_t)sizeof(ph)) return 0;
        uint32_t ph_type;
        memcpy(&ph_type, ph, sizeof(ph_type));
        if (ph_type == 3) {
            uint64_t file_offset, file_size;
            if (has_interpreter) return 0;
            memcpy(&file_offset, ph + 8, sizeof(file_offset));
            memcpy(&file_size, ph + 32, sizeof(file_size));
            if (file_size == 0 || file_size > 4096 || file_offset > image_length ||
                file_size > image_length - file_offset)
                return 0;
            uint8_t interpreter[4096];
            if (hl_c_backend_pread(fd, interpreter, (size_t)file_size, (off_t)file_offset) != (ssize_t)file_size)
                return 0;
            size_t length = (size_t)file_size - 1;
            if (interpreter[length] != 0 || memchr(interpreter, 0, length) != NULL) return 0;
            interpreter_identity = UINT64_C(0xcbf29ce484222325);
            for (size_t byte = 0; byte < length; ++byte)
                interpreter_identity = (interpreter_identity ^ interpreter[byte]) * UINT64_C(0x100000001b3);
            has_interpreter = 1;
        }
        if (ph_type != 1) continue;
        uint32_t flags;
        uint64_t file_offset, address, file_size, size, alignment;
        if (++load_count > 128) return 0;
        memcpy(&flags, ph + 4, sizeof(flags));
        memcpy(&file_offset, ph + 8, sizeof(file_offset));
        memcpy(&address, ph + 16, sizeof(address));
        memcpy(&file_size, ph + 32, sizeof(file_size));
        memcpy(&size, ph + 40, sizeof(size));
        memcpy(&alignment, ph + 48, sizeof(alignment));
        if (file_size > size ||
            (file_size != 0 && (file_offset > image_length || file_size > image_length - file_offset)) ||
            (alignment > 1 && ((alignment & (alignment - 1)) != 0 || address % alignment != file_offset % alignment)) ||
            address + size < address)
            return 0;
        if (address < first) first = address;
        if (address + size > last) last = address + size;
        if ((flags & 1) != 0 && entry >= address && entry < address + size) entry_is_executable = 1;
    }
    if (first == UINT64_MAX || !entry_is_executable) return 0;
    uint64_t start = first & ~UINT64_C(0xfff);
    if (last < start || last - start > UINT64_MAX - UINT64_C(0xffff)) return 0;
    uint64_t end = start + ((last - start + UINT64_C(0xffff)) & ~UINT64_C(0xffff));
    return start == plan->link_start && end == plan->link_end && has_interpreter == plan->has_interpreter &&
           interpreter_identity == plan->interpreter_identity;
}

HL_API int32_t hl_c_backend_create(uint32_t isa, const char *rootfs, const char *executable_host, int32_t executable_fd,
                                   const hl_c_main_image_plan *image_plan, const void *interpreter_image,
                                   size_t interpreter_size, uint32_t option_count, const char *const *option_names,
                                   const char *const *option_values, const hl_engine_box_config *box_config,
                                   const int32_t standard_fds[3], int32_t provider_fd,
                                   void *syscall_context, hl_syscall_trap_fn syscall_dispatch, hl_c_backend **output) {
    hl_c_backend *backend;
    hl_engine_config config;
    hl_status status;
    uint32_t index;
    hl_engine_fd_binding bindings[3];
    hl_host_result imported[3];
    hl_engine_executable executable;
    hl_c_backend_leak_check_probe();
    if (output == NULL) {
        if (provider_fd >= 0) close(provider_fd);
        return HL_STATUS_INVALID_ARGUMENT;
    }
    *output = NULL;
    if (provider_fd >= 0) {
        close(provider_fd);
        return HL_STATUS_NOT_SUPPORTED;
    }
    if (provider_fd != -1) return HL_STATUS_INVALID_ARGUMENT;
    int validation_fd = executable_fd;
    int validation_owned = 0;
    if (validation_fd < 0 && executable_host != NULL) {
        validation_fd = open(executable_host, O_RDONLY | HL_C_OPEN_CLOEXEC);
        validation_owned = validation_fd >= 0;
    }
    int validation_ok = validation_fd >= 0 && hl_c_validate_main_image_plan(validation_fd, image_plan);
    if (validation_owned) close(validation_fd);
    if (!validation_ok) return HL_STATUS_INVALID_ARGUMENT;
    backend = calloc(1, sizeof(*backend));
    if (backend == NULL) return HL_STATUS_OUT_OF_MEMORY;
    atomic_init(&backend->result_lock, false);
    backend->result.abi = HL_ENGINE_ABI;
    backend->result.size = sizeof(backend->result);
    status = hl_c_bridge_host_create(&backend->host, &backend->services);
    if (status != HL_STATUS_OK) {
        free(backend);
        return status;
    }
    memset(&config, 0, sizeof(config));
#if !defined(HL_BUILD_TARGET_X86_64_ONLY)
    hl_aarch64_target_register_backend();
#endif
    hl_x86_64_target_register_backend();
    config.abi = HL_ENGINE_ABI;
    config.size = sizeof(config);
    config.guest_isa = isa;
    config.rootfs = rootfs;
    config.box = box_config;
    memset(bindings, 0, sizeof(bindings));
    memset(imported, 0, sizeof(imported));
    memset(&executable, 0, sizeof(executable));
    if (standard_fds != NULL) {
        for (index = 0; index < 3; ++index) {
            uint32_t access = index == 0 ? HL_HOST_FILE_READ : HL_HOST_FILE_WRITE;
            imported[index] = hl_c_bridge_host_import_file(backend->host, standard_fds[index], access);
            if (imported[index].status != HL_STATUS_OK) {
                uint32_t close_index;
                for (close_index = 0; close_index < index; ++close_index)
                    (void)backend->services.file->close(backend->services.context, imported[close_index].value);
                hl_c_bridge_host_destroy(backend->host);
                free(backend);
                return imported[index].status;
            }
            bindings[index].abi = HL_ENGINE_ABI;
            bindings[index].size = sizeof(bindings[index]);
            bindings[index].guest_fd = index;
            bindings[index].status_flags = hl_c_backend_status_flags(imported[index].detail);
            bindings[index].ownership = HL_ENGINE_FD_TRANSFER;
            bindings[index].host_handle = imported[index].value;
        }
        config.fd_bindings = bindings;
        config.fd_binding_count = 3;
    }
    if (hl_options_init_records(&backend->options, option_count, option_names, option_values) != 0) {
        hl_c_bridge_host_destroy(backend->host);
        free(backend);
        return HL_STATUS_INVALID_ARGUMENT;
    }
    backend->options_initialized = 1;
    config.main_image_plan = image_plan;
    if (executable_fd >= 0) {
        hl_host_result imported_executable =
            hl_c_bridge_host_import_file(backend->host, executable_fd, HL_HOST_FILE_READ);
        if (imported_executable.status != HL_STATUS_OK || imported_executable.value == HL_HOST_HANDLE_INVALID) {
            hl_options_destroy(&backend->options);
            if (standard_fds != NULL)
                for (index = 0; index < 3; ++index)
                    (void)backend->services.file->close(backend->services.context, imported[index].value);
            hl_c_bridge_host_destroy(backend->host);
            free(backend);
            return imported_executable.status == HL_STATUS_OK ? HL_STATUS_PLATFORM_FAILURE : imported_executable.status;
        }
        executable = (hl_engine_executable){
            .abi = HL_ENGINE_ABI,
            .size = sizeof(executable),
            .ownership = HL_ENGINE_FD_TRANSFER,
            .reserved = 0,
            .host_handle = imported_executable.value,
            .image = NULL,
            .image_size = 0,
        };
        config.executable = &executable;
    } else if (executable_host != NULL) {
        status = hl_c_backend_executable_open(&backend->services, executable_host, &executable);
        if (status != HL_STATUS_OK) {
            hl_options_destroy(&backend->options);
            hl_c_bridge_host_destroy(backend->host);
            free(backend);
            return status;
        }
        config.executable = &executable;
    }
    status = hl_engine_create_with_borrowed_options_and_syscall_trap_and_interpreter(
        &config, &backend->services, &backend->options, syscall_context, syscall_dispatch, interpreter_image,
        interpreter_size, &backend->engine);
    if (status != HL_STATUS_OK) {
        hl_c_backend_executable_discard(&backend->services, &executable);
        if (standard_fds != NULL)
            for (index = 0; index < 3; ++index)
                (void)backend->services.file->close(backend->services.context, imported[index].value);
        hl_c_bridge_host_destroy(backend->host);
        hl_options_destroy(&backend->options);
        free(backend);
        return status;
    }
    backend->result.abi = HL_ENGINE_ABI;
    backend->result.size = sizeof(backend->result);
    *output = backend;
    return HL_STATUS_OK;
}

HL_API int32_t hl_c_backend_run(hl_c_backend *backend, int32_t argc, const char *const *argv) {
    hl_engine_exit result = {.abi = HL_ENGINE_ABI, .size = sizeof(result)};
    int32_t status;
    if (backend == NULL) return HL_STATUS_INVALID_ARGUMENT;
    status = hl_engine_run(backend->engine, argc, argv, &result);
    hl_c_backend_result_lock(backend);
    backend->result = result;
    hl_c_backend_result_unlock(backend);
    return status;
}

enum { HL_C_BACKEND_REQUEST_CHECKPOINT_PRIVATE = 4u };

HL_API int32_t hl_c_backend_request(hl_c_backend *backend, uint32_t request, int32_t signal) {
    if (backend == NULL) return HL_STATUS_INVALID_ARGUMENT;
    if (request == HL_ENGINE_REQUEST_SIGNAL || request == HL_C_BACKEND_REQUEST_CHECKPOINT_PRIVATE)
        return hl_engine_request(backend->engine, request, &signal, sizeof(signal));
    return hl_engine_request(backend->engine, request, NULL, 0);
}

HL_API int32_t hl_c_backend_exit(hl_c_backend *backend, hl_engine_exit *result) {
    if (backend == NULL || result == NULL || result->abi != HL_ENGINE_ABI || result->size < sizeof(*result))
        return HL_STATUS_INVALID_ARGUMENT;
    hl_c_backend_result_lock(backend);
    *result = backend->result;
    hl_c_backend_result_unlock(backend);
    return HL_STATUS_OK;
}

HL_API uint32_t hl_c_backend_exit_kind(const hl_c_backend *backend) {
    hl_engine_exit result = {.abi = HL_ENGINE_ABI, .size = sizeof(result)};
    return hl_c_backend_exit((hl_c_backend *)backend, &result) == HL_STATUS_OK ? result.kind : 0;
}

HL_API int32_t hl_c_backend_exit_status(const hl_c_backend *backend) {
    hl_engine_exit result = {.abi = HL_ENGINE_ABI, .size = sizeof(result)};
    return hl_c_backend_exit((hl_c_backend *)backend, &result) == HL_STATUS_OK ? result.guest_status : -1;
}

HL_API int32_t hl_c_backend_process_identity_signal(int32_t handle, uint64_t host_pid, int32_t signal) {
    if (handle < 0 || host_pid == 0 || host_pid > INT64_MAX || signal < 0 || signal > 64) return -1;
#if defined(__linux__)
    /* The pidfd names one incarnation, so the kernel itself refuses a reused pid. */
    return syscall(SYS_pidfd_send_signal, handle, signal, NULL, 0) == 0 ? 0 : -1;
#elif defined(__APPLE__)
    {
        /* No pidfd: the capability is a NOTE_EXIT watch, so a readable handle means this incarnation
           is already gone and the pid may belong to someone else. Refuse rather than retarget. */
        struct pollfd waiting = {.fd = handle, .events = POLLIN, .revents = 0};
        int ready;
        do {
            ready = poll(&waiting, 1, 0);
        } while (ready < 0 && errno == EINTR);
        if (ready != 0 || waiting.revents != 0) return -1;
        return kill((pid_t)host_pid, signal) == 0 ? 0 : -1;
    }
#else
    (void)signal;
    return -1;
#endif
}

HL_API int32_t hl_c_backend_guest_pid(const hl_c_backend *backend) {
    return backend == NULL ? 0 : hl_engine_guest_pid(((hl_c_backend *)backend)->engine);
}

HL_API uint64_t hl_c_backend_exit_detail(const hl_c_backend *backend) {
    hl_engine_exit result = {.abi = HL_ENGINE_ABI, .size = sizeof(result)};
    return hl_c_backend_exit((hl_c_backend *)backend, &result) == HL_STATUS_OK ? result.detail : 0;
}

HL_API uint64_t hl_c_backend_translation_count(const hl_c_backend *backend) {
    return backend == NULL ? 0 : hl_engine_translation_count(backend->engine);
}

HL_API void hl_c_backend_destroy(hl_c_backend *backend) {
    if (backend == NULL) return;
    hl_engine_destroy(backend->engine);
    if (backend->options_initialized) hl_options_destroy(&backend->options);
    hl_c_bridge_host_destroy(backend->host);
    free(backend);
    hl_c_backend_leak_check_verdict();
}

#ifndef HL_NATIVE_BUILD_FINGERPRINT
#define HL_NATIVE_BUILD_FINGERPRINT unfingerprinted
#endif
#define HL_FINGERPRINT_TEXT_(value) #value
#define HL_FINGERPRINT_TEXT(value) HL_FINGERPRINT_TEXT_(value)

HL_API const char *hl_c_backend_build_fingerprint(void) {
    return HL_FINGERPRINT_TEXT(HL_NATIVE_BUILD_FINGERPRINT);
}
