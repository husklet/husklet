#include "target/namespace.h"
#include "backend.h"
#include "result.h"
#include "options.h"
#include "executable_authority.h"
#include "hl/syscall_trap.h"
#include "../host/system.h"
#if defined(__APPLE__)
#include "../linux_abi/dns.h"
#endif

#include <stdatomic.h>
#include <signal.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>
#include <stdint.h>
#include <pthread.h>
#include <errno.h>
#include <fcntl.h>
#if !defined(_WIN32)
#include <sys/ioctl.h>
#include <termios.h>
#endif
#include <unistd.h>

extern int hl_ckpt_channel_adopt(const char *broker, const char *trigger);
extern int hl_ckpt_interrupt_executors(void);
extern void hl_ckpt_interrupt_block(void);

#ifndef HL_PRODUCTION_GUEST_ISA
#error HL_PRODUCTION_GUEST_ISA is required
#endif

int hl_run_linux_guest(const hl_host_services *host, hl_linux_abi *box, const char *rootfs, hl_host_handle executable,
                       const void *executable_image, size_t executable_size,
                       const hl_executable_authority *executable_authority,
                       const hl_engine_main_image_plan *main_image_plan, const void *interpreter_image,
                       size_t interpreter_size, uint32_t argc, char *const argv[]);
hl_status hl_run_linux_guest_status(void);
uint64_t hl_run_linux_guest_translations(void);

typedef struct hl_production_result_state {
    hl_host_memory_mapping mapping;
    hl_engine_child_result *record;
} hl_production_result_state;

static hl_engine_child_result *active_result;
static _Atomic int result_published;

static int hl_engine_child_result_claim(void) {
    int expected = 0;
    if (active_result == NULL) return 0;
    if (atomic_compare_exchange_strong_explicit(&result_published, &expected, 1, memory_order_acq_rel,
                                                memory_order_acquire))
        return 1;
    while (atomic_load_explicit(&result_published, memory_order_acquire) == 1) {}
    return 0;
}

void hl_engine_child_result_publish(int32_t guest_status, hl_status engine_status, uint64_t detail) {
    hl_engine_child_result record = {
        0,      HL_ENGINE_CHILD_RESULT_VERSION,   guest_status, engine_status, HL_ENGINE_CHILD_RESULT_EXIT, 0,
        detail, hl_run_linux_guest_translations()};
    if (!hl_engine_child_result_claim()) return;
    memcpy(active_result, &record, sizeof(record));
    atomic_store_explicit((_Atomic uint32_t *)&active_result->magic, HL_ENGINE_CHILD_RESULT_MAGIC,
                          memory_order_release);
    atomic_store_explicit(&result_published, 2, memory_order_release);
}

void hl_engine_child_result_publish_signal(int32_t guest_signal) {
    if (!hl_engine_child_result_claim()) return;
    active_result->version = HL_ENGINE_CHILD_RESULT_VERSION;
    active_result->guest_status = guest_signal;
    active_result->engine_status = HL_STATUS_OK;
    active_result->kind = HL_ENGINE_CHILD_RESULT_SIGNAL;
    active_result->reserved = 0;
    active_result->detail = 0;
    active_result->translations = hl_run_linux_guest_translations();
    atomic_store_explicit((_Atomic uint32_t *)&active_result->magic, HL_ENGINE_CHILD_RESULT_MAGIC,
                          memory_order_release);
    atomic_store_explicit(&result_published, 2, memory_order_release);
}

void hl_engine_child_result_after_fork(void) {
    active_result = NULL;
    atomic_store_explicit(&result_published, 2, memory_order_release);
}

typedef struct hl_production_entry_context {
    const hl_engine_config *config;
    uint32_t argc;
    const char *const *argv;
    const hl_host_services *host;
    hl_linux_abi *box;
    hl_options *options;
    hl_engine_child_result *result;
    void *syscall_context;
    hl_syscall_trap_fn syscall_dispatch;
    int checkpoint_broker;
    int checkpoint_trigger;
    int checkpoint_control;
    const void *interpreter_image;
    size_t interpreter_size;
    int activation_ready_read;
    int activation_ready_write;
    const hl_host_process_fd_private_plan *child_descriptor_plan;
} hl_production_entry_context;

#if defined(HL_NATIVE_TEST_HOOKS)
void hl_host_activation_ready_test_wait(void);
#endif

#if defined(_WIN32)
/*
 * The same launch, for a host whose child does not inherit an address space.
 *
 * Everywhere else the child is a fork: it already holds the config, the argv,
 * the option store and the executable image the parent built, so the context
 * above -- a struct of pointers into the parent's heap -- is all that has to be
 * handed over. A Windows child is a fresh CreateProcess of this same image and
 * holds none of it, so the same launch has to be described in BYTES and rebuilt
 * on the far side. That is the whole of what this arm adds; the guest run it
 * ends in is the identical call.
 *
 * Four things make up a launch, and each is here for a reason the guest would
 * miss without it:
 *
 *   rootfs   -- names the filesystem the guest's paths are resolved against.
 *   argv     -- is the guest's own argv, argv[0] included; the loader opens it.
 *   image    -- the executable bytes the engine already read and authorised in
 *               the parent. It travels rather than being re-read because the
 *               parent checked the file's identity did not change under it
 *               while reading, and a child that opened the path again would be
 *               opening whatever is there now.
 *   options  -- the engine's option store, which by this point has absorbed the
 *               config's limits and the box settings, and is what every later
 *               hl_option_get() in the guest run reads.
 *
 * The option store travels as (table index, bytes) pairs rather than as
 * name=value text. The index is a position in the option definition table, which
 * is a compile-time constant of this image -- and the child is a re-execution of
 * this same image from the same file, so the two sides index the same table.
 * That is checked, not assumed: the header carries the table's length and the
 * child refuses a payload whose length disagrees with its own.
 */
#include "../host/windows/launch.h"
#include "hl/windows.h"

#define HL_PRODUCTION_LAUNCH_MAGIC UINT32_C(0x484c4357)
#define HL_PRODUCTION_LAUNCH_VERSION 2u

typedef struct hl_production_launch_header {
    uint32_t magic;
    uint32_t version;
    uint64_t total_size;
    uint32_t argc;
    uint32_t option_count;    /* records present, not the table's length */
    uint32_t option_capacity; /* the table's length in the producing image */
    uint32_t reserved;
    uint64_t rootfs_offset; /* 0 when the launch has no rootfs */
    uint64_t rootfs_size;
    uint64_t argv_offset; /* argc NUL-terminated strings, packed */
    uint64_t argv_size;
    uint64_t options_offset;
    uint64_t options_size;
    uint64_t image_offset; /* 0 when the engine carried no executable image */
    uint64_t image_size;
    hl_executable_authority executable_authority;
} hl_production_launch_header;

typedef struct hl_production_launch_option {
    uint32_t index;
    uint32_t size; /* the stored value's size, terminating NUL included */
} hl_production_launch_option;

/* Every string this walks is one the engine already copied and bounded, so the
 * only length that could surprise it is a missing terminator, which cannot
 * happen for a value hl_options_set stored. */
static size_t hl_production_launch_text(const char *text) {
    return text == NULL ? 0 : strlen(text) + 1u;
}

static void *hl_production_launch_encode(const hl_host_services *host, const hl_engine_config *config,
                                         const hl_options *options, uint32_t argc, const char *const argv[],
                                         size_t *out_size, hl_status *out_status) {
    hl_production_launch_header header;
    const hl_engine_executable *spec = config->executable;
    unsigned char *bytes;
    size_t offset;
    size_t index;
    *out_status = HL_STATUS_OK;
    memset(&header, 0, sizeof(header));
    header.magic = HL_PRODUCTION_LAUNCH_MAGIC;
    header.version = HL_PRODUCTION_LAUNCH_VERSION;
    header.argc = argc;
    header.option_capacity = options == NULL ? 0u : (uint32_t)options->value_count;
    header.rootfs_size = hl_production_launch_text(config->rootfs);
    for (index = 0; index < argc; ++index) {
        if (argv[index] == NULL) return NULL;
        header.argv_size += hl_production_launch_text(argv[index]);
    }
    if (options != NULL)
        for (index = 0; index < options->value_count; ++index) {
            if (options->value_sizes[index] == 0) continue;
            header.option_count++;
            header.options_size += sizeof(hl_production_launch_option) + options->value_sizes[index];
        }
    header.image_size = spec == NULL || spec->image == NULL ? 0u : (uint64_t)spec->image_size;
    if (spec != NULL && spec->host_handle != HL_HOST_HANDLE_INVALID && host != NULL && host->file != NULL &&
        host->file->metadata != NULL) {
        hl_host_file_metadata metadata = {0};
        /* The presence of an engine executable in this launch record is the
         * parent's explicit execute authority. Windows metadata deliberately
         * has no execute bits, so carry the policy separately from host DAC. */
        if (host->file->metadata(host->context, spec->host_handle, &metadata).status != HL_STATUS_OK ||
            !hl_executable_authority_from_metadata(&metadata, 1, &header.executable_authority)) {
            *out_status = HL_STATUS_PLATFORM_FAILURE;
            return NULL;
        }
    }
    if (header.image_size != 0 && !header.executable_authority.ready) {
        *out_status = HL_STATUS_PLATFORM_FAILURE;
        return NULL;
    }

    offset = sizeof(header);
    if (header.rootfs_size != 0) {
        header.rootfs_offset = offset;
        offset += (size_t)header.rootfs_size;
    }
    /* An absent region is offset 0, which is inside the header and therefore a
     * value no present region can take: the bounds test needs one spelling of
     * "not here" and this is it. */
    if (header.argv_size != 0) {
        header.argv_offset = offset;
        offset += (size_t)header.argv_size;
    }
    if (header.options_size != 0) {
        header.options_offset = offset;
        offset += (size_t)header.options_size;
    }
    if (header.image_size != 0) {
        header.image_offset = offset;
        offset += (size_t)header.image_size;
    }
    header.total_size = offset;

    bytes = malloc(offset);
    if (bytes == NULL) return NULL;
    memcpy(bytes, &header, sizeof(header));
    if (header.rootfs_offset != 0) memcpy(bytes + header.rootfs_offset, config->rootfs, (size_t)header.rootfs_size);
    offset = (size_t)header.argv_offset;
    for (index = 0; index < argc; ++index) {
        const size_t size = hl_production_launch_text(argv[index]);
        memcpy(bytes + offset, argv[index], size);
        offset += size;
    }
    offset = (size_t)header.options_offset;
    if (options != NULL)
        for (index = 0; index < options->value_count; ++index) {
            hl_production_launch_option record;
            if (options->value_sizes[index] == 0) continue;
            record.index = (uint32_t)index;
            record.size = (uint32_t)options->value_sizes[index];
            memcpy(bytes + offset, &record, sizeof(record));
            offset += sizeof(record);
            memcpy(bytes + offset, options->values[index], record.size);
            offset += record.size;
        }
    if (header.image_offset != 0) memcpy(bytes + header.image_offset, spec->image, (size_t)header.image_size);
    *out_size = (size_t)header.total_size;
    return bytes;
}

/* A region is usable only if it lies wholly inside the payload the child was
 * handed. Everything the decoder reads goes through here first, so a truncated
 * or mis-sized payload fails one bounds test instead of walking off the end. */
static int hl_production_launch_span(const hl_production_launch_header *header, uint64_t offset, uint64_t size) {
    if (size == 0) return offset == 0;
    return offset >= sizeof(*header) && offset <= header->total_size && size <= header->total_size - offset;
}

static hl_status hl_production_launch_options(hl_options *options, const unsigned char *bytes,
                                              const hl_production_launch_header *header) {
    uint64_t offset = header->options_offset;
    uint64_t remaining = header->options_size;
    uint32_t count;
    if (hl_options_init(options) != 0) return HL_STATUS_OUT_OF_MEMORY;
    if (header->option_capacity != (uint32_t)options->value_count) return HL_STATUS_ABI_MISMATCH;
    for (count = 0; count < header->option_count; ++count) {
        hl_production_launch_option record;
        char *value;
        if (remaining < sizeof(record)) return HL_STATUS_CORRUPT;
        memcpy(&record, bytes + offset, sizeof(record));
        offset += sizeof(record);
        remaining -= sizeof(record);
        if (record.index >= options->value_count || record.size == 0 || record.size > remaining)
            return HL_STATUS_CORRUPT;
        if (bytes[offset + record.size - 1u] != 0) return HL_STATUS_CORRUPT;
        value = malloc(record.size);
        if (value == NULL) return HL_STATUS_OUT_OF_MEMORY;
        memcpy(value, bytes + offset, record.size);
        free(options->values[record.index]);
        options->store_size -= options->value_sizes[record.index];
        options->values[record.index] = value;
        options->value_sizes[record.index] = record.size;
        options->store_size += record.size;
        offset += record.size;
        remaining -= record.size;
    }
    return HL_STATUS_OK;
}

/*
 * The box the cold child runs with, and the two tables it indexes.
 *
 * The tables are heap rather than static for the reason runtime.c allocates the
 * same pair that way: together they are several megabytes, almost all of which
 * a typical guest never touches, and a calloc leaves the untouched pages
 * demand-zero. hl_linux_abi_init requires them zeroed and deliberately does not
 * zero them itself, which is exactly what calloc promises.
 *
 * The hl_linux_abi itself is static because it must outlive this function and
 * every guest thread that will reach it through g_linux_box; a failure to build
 * it returns NULL, which is the state this host was in before and which
 * hl_run_linux_guest already accepts.
 */
static hl_linux_abi cold_box;

static hl_linux_abi *hl_production_cold_box(const hl_host_services *services) {
    hl_linux_fd_entry *fds = calloc(HL_LINUX_FD_LIMIT, sizeof(*fds));
    hl_linux_ofd_entry *ofds = calloc(HL_LINUX_OFD_LIMIT, sizeof(*ofds));
    if (fds == NULL || ofds == NULL ||
        hl_linux_abi_init(&cold_box, services, fds, HL_LINUX_FD_LIMIT, ofds, HL_LINUX_OFD_LIMIT) != HL_STATUS_OK) {
        free(fds);
        free(ofds);
        return NULL;
    }
    return &cold_box;
}

/*
 * The child side. Reached with a NULL context, because on this host a context is
 * a pointer and a pointer is the one thing that cannot cross; everything it
 * would have pointed at arrives through the launch channel instead.
 *
 * Host services are CREATED here rather than carried. They are a table of
 * function pointers over per-process kernel objects -- handles, a handle table,
 * resolved entry points -- and none of that is transferable; what the parent
 * would have handed over is precisely what a fresh hl_host_windows_create()
 * produces. The three standard streams the guest actually needs are the ones
 * the spawn already inherited, so this host is the same host, addressing the
 * same console or pipes.
 *
 * The box is BUILT HERE, and the reason it is built rather than carried is the
 * reason everything else on this path is: it is a graph of pointers -- host
 * services, two descriptor tables, a host mutex per open file description --
 * and not one of those pointers means anything in a second address space. What
 * makes it constructible anyway is that a box has no INPUTS a cold child does
 * not already have. hl_linux_abi_init takes host services and two zeroed arrays,
 * and this arm has just created the first and can allocate the second, so the
 * object the parent would have handed over is exactly the object this produces.
 *
 * It starts EMPTY, and that is the statement about what a cold child is. The
 * entries a box carries are open file descriptions, and those cross a FORK by
 * being duplicated into a child that already shares the address space they live
 * in. A CreateProcess child shares none of it; what it holds instead are the
 * three standard streams the spawn inherited, which are already reachable as
 * this process's own descriptors and are the descriptors the guest's fd 0, 1
 * and 2 resolve to. So the table starts with nothing in it and fills as the
 * guest asks for things.
 *
 * Which is the whole point of building it. The box is not only a descriptor
 * table -- it is the object the typed providers (eventfd, pipe, epoll, timerfd,
 * pidfd, inotify) install into, and the guest paths that reach them are all
 * guarded on its existence. With no box those paths are unreachable on this
 * host no matter what is wired behind them; with an empty one they behave
 * exactly as they do on a freshly forked guest that has not opened anything.
 *
 * Nothing is destroyed on the way out. hl_linux_abi_destroy refuses a box with
 * live descriptions in it, which is the normal state of a guest that ran, and
 * this frame is the last one before process exit -- so the tables are released
 * by exit, with the host handles in them, exactly as the guest's other
 * per-process state is.
 */
static int32_t hl_production_cold_entry(void *opaque) {
    hl_production_launch_header header;
    hl_host_windows *native = NULL;
    hl_host_services services = {0};
    hl_linux_abi *box;
    hl_options options;
    hl_options process_state;
    const unsigned char *bytes;
    size_t payload_size = 0;
    size_t shared_size = 0;
    char **argv = NULL;
    const char *rootfs = NULL;
    const void *image = NULL;
    hl_options *previous;
    hl_options *previous_state;
    hl_status status;
    uint64_t offset;
    uint32_t index;
    int32_t result;
    (void)opaque;

    bytes = hl_host_windows_launch_payload(&payload_size);
    if (bytes == NULL || payload_size < sizeof(header)) return 71;
    memcpy(&header, bytes, sizeof(header));
    if (header.magic != HL_PRODUCTION_LAUNCH_MAGIC || header.version != HL_PRODUCTION_LAUNCH_VERSION ||
        header.total_size != payload_size ||
        !hl_production_launch_span(&header, header.rootfs_offset, header.rootfs_size) ||
        !hl_production_launch_span(&header, header.argv_offset, header.argv_size) ||
        !hl_production_launch_span(&header, header.options_offset, header.options_size) ||
        !hl_production_launch_span(&header, header.image_offset, header.image_size))
        return 71;
    if (header.rootfs_size != 0) {
        rootfs = (const char *)(bytes + header.rootfs_offset);
        if (rootfs[header.rootfs_size - 1u] != 0) return 71;
    }
    if (header.image_size != 0) image = bytes + header.image_offset;

    /* argv is rebuilt as pointers INTO the payload, which stays mapped for the
     * life of this process -- so the strings outlive every use the guest run
     * makes of them without a second copy. */
    argv = calloc((size_t)header.argc + 1u, sizeof(*argv));
    if (argv == NULL) return 71;
    offset = header.argv_offset;
    for (index = 0; index < header.argc; ++index) {
        const uint64_t limit = header.argv_offset + header.argv_size;
        uint64_t cursor = offset;
        while (cursor < limit && bytes[cursor] != 0)
            cursor++;
        if (cursor >= limit) {
            free(argv);
            return 71;
        }
        argv[index] = (char *)(uintptr_t)(bytes + offset);
        offset = cursor + 1u;
    }

    status = hl_production_launch_options(&options, bytes, &header);
    if (status != HL_STATUS_OK) {
        hl_options_destroy(&options);
        free(argv);
        return 71;
    }
    memset(&process_state, 0, sizeof(process_state));
    if (hl_host_windows_create(&native, &services) != HL_STATUS_OK) {
        hl_options_destroy(&process_state);
        hl_options_destroy(&options);
        free(argv);
        return 71;
    }

    /* The parent's result page, mapped here at an address of this process's own
     * choosing. Absent or too small, the publish below is a no-op and the exit
     * code is the only thing the parent has to read -- which is why it is still
     * checked rather than assumed. */
    if (hl_host_windows_launch_shared(&shared_size) != NULL && shared_size >= sizeof(*active_result))
        active_result = hl_host_windows_launch_shared(NULL);
    atomic_store_explicit(&result_published, 0, memory_order_release);

    previous = hl_options_bind_process(&options);
    previous_state = hl_options_bind_process_state(&process_state);
    /* The initialisers the target TU's constructors would run. They are
     * idempotent, and calling them explicitly removes this launch's dependence
     * on where in the constructor list this image's spawn hook happens to sit. */
    hl_target_runtime_init();
    /* `services` outlives the guest run: this frame is the entry point and does
     * not return until the run is over, which is the same lifetime the call
     * below already relies on for the services pointer itself. */
    box = hl_production_cold_box(&services);
    result = hl_run_linux_guest(&services, box, rootfs, HL_HOST_HANDLE_INVALID, image, (size_t)header.image_size,
                                &header.executable_authority, NULL, NULL, 0, header.argc, argv);
    (void)hl_options_bind_process_state(previous_state);
    (void)hl_options_bind_process(previous);
    hl_options_destroy(&process_state);
    hl_engine_child_result_publish(result, hl_run_linux_guest_status(), 0);
    return result;
}
#endif

#if !defined(_WIN32)
/* A terminal binding is more than three readable/writable file descriptors.
 * The guest process must lead the terminal's session and foreground process
 * group too, otherwise an interactive shell's first tcgetpgrp() fails ENOTTY
 * even though isatty(0) succeeds.  Engine runs are fork children, so establish
 * that kernel relationship here, before any guest code observes fd 0. */
static hl_status hl_production_claim_terminal(const hl_production_entry_context *context) {
    hl_linux_fd_snapshot input;
    const hl_host_posix_attachment_services *attachments;
    hl_host_result borrowed;
    int descriptor;
    int saved_errno;
    if (context->box == NULL || hl_linux_fd_snapshot_get(context->box, 0, &input) != HL_STATUS_OK ||
        input.host_handle == HL_HOST_HANDLE_INVALID)
        return HL_STATUS_OK;
    attachments = context->host->posix_attachment;
    if (attachments == NULL || attachments->borrow_file == NULL || attachments->release == NULL) return HL_STATUS_OK;
    borrowed = attachments->borrow_file(context->host->context, input.host_handle);
    if (borrowed.status != HL_STATUS_OK) return (hl_status)borrowed.status;
    descriptor = (int)borrowed.value;
    if (!isatty(descriptor)) {
        (void)attachments->release(context->host->context, borrowed.value);
        return HL_STATUS_OK;
    }
    /* A directly launched engine may inherit an already-owned terminal.  Keep
     * that session intact; only an unattached slave (the container PTY path)
     * needs a new session and TIOCSCTTY. */
    if (tcgetsid(descriptor) >= 0) {
        (void)attachments->release(context->host->context, borrowed.value);
        return HL_STATUS_OK;
    }
    if (ioctl(descriptor, TIOCSCTTY, 0) < 0 || tcsetpgrp(descriptor, getpgrp()) < 0) {
        saved_errno = errno;
        (void)attachments->release(context->host->context, borrowed.value);
        errno = saved_errno;
        return HL_STATUS_PLATFORM_FAILURE;
    }
    (void)attachments->release(context->host->context, borrowed.value);
    return HL_STATUS_OK;
}

static void *hl_checkpoint_control_main(void *opaque) {
    int descriptor = (int)(intptr_t)opaque;
    unsigned char command;
    unsigned char ready = 0xa5;
    if (write(descriptor, &ready, sizeof(ready)) != (ssize_t)sizeof(ready)) return NULL;
    while (read(descriptor, &command, sizeof(command)) == (ssize_t)sizeof(command)) {
        if (command == 1) {
            int count = hl_ckpt_interrupt_executors();
            unsigned char interrupted = count > 255 ? 255 : (unsigned char)count;
            if (write(descriptor, &interrupted, sizeof(interrupted)) != (ssize_t)sizeof(interrupted)) return NULL;
        }
    }
    return NULL;
}

static int32_t hl_production_entry(void *opaque) {
    hl_production_entry_context *context = opaque;
    int checkpoint_control = context->checkpoint_control;
    int activation_ready_write = context->activation_ready_write;
    unsigned char ready = 1;
#if defined(HL_NATIVE_TEST_HOOKS)
    hl_host_activation_ready_test_wait();
#endif
    if (setsid() < 0) return HL_STATUS_PLATFORM_FAILURE;
    if (context->activation_ready_read >= 0) {
        int activation_read = context->activation_ready_read;
        if (close(activation_read) != 0) {
            int close_error = errno;
            if (close_error != EINTR || fcntl(activation_read, F_GETFD) >= 0 || errno != EBADF)
                return HL_STATUS_PLATFORM_FAILURE;
        }
    }
    hl_engine_checkpoint_fork_child(context->checkpoint_broker, context->checkpoint_trigger,
                                    context->checkpoint_control);
    /* The production worker is an exec-like boundary even though the host primitive is fork: only typed
     * bindings and the explicitly duplicated control channels cross. Caller/library ambient descriptors
     * remain untouched in the parent and cannot occupy or be named through the guest descriptor table. */
    if (hl_host_process_fd_private_plan_child(context->child_descriptor_plan) != 0)
        return HL_STATUS_PLATFORM_FAILURE;
    activation_ready_write = hl_host_process_fd_private_adopt(activation_ready_write);
    if (activation_ready_write < 0) return HL_STATUS_PLATFORM_FAILURE;
    /* Keep process-directed checkpoint kicks away from helper/control threads.
     * Guest executor registration selectively unblocks the reserved signal. */
    hl_ckpt_interrupt_block();
    hl_status terminal_status = hl_production_claim_terminal(context);
    if (terminal_status != HL_STATUS_OK) return terminal_status;
    active_result = context->result;
    atomic_store_explicit(&result_published, 0, memory_order_release);
    hl_options *previous = hl_options_bind_process(context->options);
    hl_options process_state;
    hl_options *previous_state;
    memset(&process_state, 0, sizeof(process_state));
    previous_state = hl_options_bind_process_state(&process_state);
    hl_host_handle executable =
        context->config->executable == NULL ? HL_HOST_HANDLE_INVALID : context->config->executable->host_handle;
    const hl_engine_executable *spec = context->config->executable;
    hl_target_syscall_trap_install(context->syscall_context, context->syscall_dispatch);
    if (context->checkpoint_broker >= 0) {
        char broker[32];
        char trigger[32];
        (void)snprintf(broker, sizeof(broker), "%d", context->checkpoint_broker);
        (void)snprintf(trigger, sizeof(trigger), "%d", context->checkpoint_trigger);
        if (hl_ckpt_channel_adopt(broker, trigger) != 0) return HL_STATUS_PLATFORM_FAILURE;
        if (checkpoint_control >= 0) {
            checkpoint_control = hl_host_process_fd_private_adopt(checkpoint_control);
            if (checkpoint_control < 0) return HL_STATUS_PLATFORM_FAILURE;
        }
    }
    if (context->checkpoint_broker >= 0) {
        if (checkpoint_control >= 0) {
            pthread_t control;
            if (pthread_create(&control, NULL, hl_checkpoint_control_main, (void *)(intptr_t)checkpoint_control) != 0)
                return HL_STATUS_PLATFORM_FAILURE;
            if (pthread_detach(control) != 0) return HL_STATUS_PLATFORM_FAILURE;
        }
    }
    if (write(activation_ready_write, &ready, sizeof(ready)) != (ssize_t)sizeof(ready))
        return HL_STATUS_PLATFORM_FAILURE;
    /* The readiness byte is the parent's success boundary. Retain this registered engine-private writer
     * until process exit instead of making success depend on close(2)'s ambiguous EINTR state. It is above
     * the guest interval, excluded from checkpoint descriptor capture, and has one explicit lifetime owner. */
    int32_t result = hl_run_linux_guest(
        context->host, context->box, context->config->rootfs, executable, spec == NULL ? NULL : spec->image,
        spec == NULL ? 0 : spec->image_size, NULL, context->config->main_image_plan, context->interpreter_image,
        context->interpreter_size, context->argc, (char *const *)(uintptr_t)context->argv);
    (void)hl_options_bind_process_state(previous_state);
    (void)hl_options_bind_process(previous);
    hl_options_destroy(&process_state);
    hl_engine_child_result_publish(result, hl_run_linux_guest_status(), 0);
    return result;
}
#endif

static void hl_production_result_release(const hl_host_services *host, hl_host_handle token) {
    hl_production_result_state *state = (hl_production_result_state *)(uintptr_t)token;
    if (state == NULL) return;
    if (state->mapping.handle != HL_HOST_HANDLE_INVALID)
        (void)host->memory->release(host->context, state->mapping.handle);
    free(state);
}

static hl_status hl_production_start_process(const hl_host_services *host, hl_linux_abi *box, hl_options *options,
                                             const hl_engine_config *config, uint32_t argc, const char *const argv[],
                                             void *syscall_context, hl_syscall_trap_fn syscall_dispatch,
                                             int checkpoint_broker, int checkpoint_trigger, int checkpoint_control,
                                             const void *interpreter_image, size_t interpreter_size,
                                             hl_host_handle *process, hl_host_handle *result_token) {
#if !defined(_WIN32)
    hl_production_entry_context entry = {0};
#endif
    hl_production_result_state *result;
    hl_host_result spawned;
    hl_host_result mapped;
    if (hl_host_services_validate(host, HL_HOST_CAP_PROCESS | HL_HOST_CAP_MEMORY) != HL_STATUS_OK)
        return HL_STATUS_NOT_SUPPORTED;
    *process = HL_HOST_HANDLE_INVALID;
    *result_token = HL_HOST_HANDLE_INVALID;
    result = calloc(1, sizeof(*result));
    if (result == NULL) return HL_STATUS_OUT_OF_MEMORY;
    result->mapping = (hl_host_memory_mapping){HL_HOST_MEMORY_MAPPING_ABI, sizeof(result->mapping), 0, 0, 0, 0};
    mapped = host->memory->map_anonymous(host->context, 0, sizeof(hl_engine_child_result),
                                         HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE, HL_HOST_MEMORY_SHARED,
                                         &result->mapping);
    if (mapped.status != HL_STATUS_OK) {
        free(result);
        return (hl_status)mapped.status;
    }
    result->record = (hl_engine_child_result *)(uintptr_t)result->mapping.address;
    memset(result->record, 0, sizeof(*result->record));
#if defined(__APPLE__)
    /* The production backend enters the Linux personality in a fork child.  Resolve
     * macOS' lazy Foundation-backed resolver state in the parent: doing this from
     * container_init is already too late for Objective-C's post-fork guard. */
    hl_linux_dns_prepare();
#endif
#if defined(_WIN32)
    /*
     * A cold child takes the serialised launch and nothing else, and it takes it
     * whether or not this engine has a box.
     *
     * hl_linux_abi_spawn is the fork path: it duplicates the box's open-file
     * descriptions so that the child, which shares this address space, ends up
     * owning its own copies of them. A CreateProcess child shares no address
     * space and has no box for those copies to land in, so running that plan
     * would duplicate a set of handles for nobody and hand the child an entry
     * context that is once again a parent pointer. What crosses instead is the
     * inherited standard streams, which is what those bindings were made from.
     */
    (void)box;
    {
        size_t payload_size = 0;
        hl_status encode_status;
        void *payload = hl_production_launch_encode(host, config, options, argc, argv, &payload_size, &encode_status);
        hl_status published;
        if (payload == NULL) {
            hl_production_result_release(host, (hl_host_handle)(uintptr_t)result);
            return encode_status == HL_STATUS_OK ? HL_STATUS_OUT_OF_MEMORY : encode_status;
        }
        published = hl_host_windows_launch_publish(payload, payload_size, result->mapping.handle);
        spawned = published == HL_STATUS_OK ? host->process->spawn_cloned(host->context, hl_production_cold_entry, NULL)
                                            : (hl_host_result){(int32_t)published, 0, 0, 0};
        free(payload);
    }
#else
    int activation_ready[2] = {-1, -1};
    hl_host_process_fd_private_plan *child_descriptor_plan = NULL;
    if (pipe(activation_ready) < 0) {
        hl_production_result_release(host, (hl_host_handle)(uintptr_t)result);
        return HL_STATUS_RESOURCE_LIMIT;
    }
    entry.config = config;
    entry.argc = argc;
    entry.argv = argv;
    entry.host = host;
    entry.box = box;
    entry.options = options;
    entry.result = result->record;
    entry.syscall_context = syscall_context;
    entry.syscall_dispatch = syscall_dispatch;
    entry.checkpoint_broker = checkpoint_broker;
    entry.checkpoint_trigger = checkpoint_trigger;
    entry.checkpoint_control = checkpoint_control;
    entry.interpreter_image = interpreter_image;
    entry.interpreter_size = interpreter_size;
    entry.activation_ready_read = activation_ready[0];
    entry.activation_ready_write = activation_ready[1];
    if (box != NULL) {
        const int retained_descriptors[] = {activation_ready[1], checkpoint_broker, checkpoint_trigger,
                                            checkpoint_control};
        if (hl_host_process_fd_private_plan_prepare(STDERR_FILENO + 1, retained_descriptors,
                                                    sizeof(retained_descriptors) / sizeof(*retained_descriptors),
                                                    &child_descriptor_plan) != 0) {
            close(activation_ready[0]);
            close(activation_ready[1]);
            hl_production_result_release(host, (hl_host_handle)(uintptr_t)result);
            return HL_STATUS_PLATFORM_FAILURE;
        }
    }
    if (box != NULL) {
        entry.checkpoint_broker = hl_host_process_fd_private_plan_descriptor(child_descriptor_plan, checkpoint_broker);
        entry.checkpoint_trigger = hl_host_process_fd_private_plan_descriptor(child_descriptor_plan, checkpoint_trigger);
        entry.checkpoint_control = hl_host_process_fd_private_plan_descriptor(child_descriptor_plan, checkpoint_control);
        entry.activation_ready_write =
            hl_host_process_fd_private_plan_descriptor(child_descriptor_plan, activation_ready[1]);
        if (entry.activation_ready_write < 0 ||
            (checkpoint_broker >= 0 && (entry.checkpoint_broker < 0 || entry.checkpoint_trigger < 0)) ||
            (checkpoint_control >= 0 && entry.checkpoint_control < 0)) {
            (void)hl_host_process_fd_private_plan_release(&child_descriptor_plan);
            close(activation_ready[0]);
            close(activation_ready[1]);
            hl_production_result_release(host, (hl_host_handle)(uintptr_t)result);
            return HL_STATUS_PLATFORM_FAILURE;
        }
    }
    entry.child_descriptor_plan = child_descriptor_plan;
    hl_engine_checkpoint_fork_prepare();
    if (box == NULL) {
        spawned = host->process->spawn_cloned(host->context, hl_production_entry, &entry);
    } else {
        hl_status status = hl_linux_abi_spawn(box, hl_production_entry, &entry, process);
        if (status != HL_STATUS_OK) {
            spawned = (hl_host_result){(int32_t)status, 0, HL_HOST_HANDLE_INVALID, 0};
        } else {
            spawned = (hl_host_result){HL_STATUS_OK, 0, *process, 0};
        }
    }
    hl_engine_checkpoint_fork_parent();
    if (hl_host_process_fd_private_plan_release(&child_descriptor_plan) != 0 && spawned.status == HL_STATUS_OK)
        spawned.status = HL_STATUS_PLATFORM_FAILURE;
    close(activation_ready[1]);
    if (spawned.status == HL_STATUS_OK) {
        unsigned char ready = 0;
        ssize_t received;
        do {
            received = read(activation_ready[0], &ready, sizeof(ready));
        } while (received < 0 && errno == EINTR);
        if (received != (ssize_t)sizeof(ready) || ready != 1) spawned.status = HL_STATUS_PLATFORM_FAILURE;
    }
    close(activation_ready[0]);
#endif
    if (spawned.status != HL_STATUS_OK) {
        if (spawned.value != HL_HOST_HANDLE_INVALID) {
            (void)host->process->terminate(host->context, spawned.value, HL_HOST_PROCESS_TERMINATE_FORCE);
            (void)host->process->wait(host->context, spawned.value, HL_HOST_DEADLINE_INFINITE);
            (void)host->process->close(host->context, spawned.value);
        }
        hl_production_result_release(host, (hl_host_handle)(uintptr_t)result);
        return (hl_status)spawned.status;
    }
    if (*process == HL_HOST_HANDLE_INVALID) *process = spawned.value;
    *result_token = (hl_host_handle)(uintptr_t)result;
    return HL_STATUS_OK;
}

static hl_status hl_production_finish_process(const hl_host_services *host, hl_host_handle token,
                                              const hl_host_result *waited, hl_engine_exit *result,
                                              uint64_t *translations) {
    hl_production_result_state *state = (hl_production_result_state *)(uintptr_t)token;
    hl_engine_child_result record;
    if (waited->detail == HL_HOST_PROCESS_EXIT_SIGNAL) {
        hl_production_result_release(host, token);
        result->kind = HL_ENGINE_EXIT_SIGNAL;
#if defined(__APPLE__)
        // A translated bad-address fault may reach waitpid as the raw macOS SIGBUS when the kernel does
        // not enter the POSIX guard. Linux reports that access as SIGSEGV. Genuine guest file-EOF SIGBUS
        // is published through hl_engine_child_result and therefore takes the record path below.
        result->guest_status = waited->value == SIGBUS ? 11 : (int32_t)waited->value;
#else
        result->guest_status = (int32_t)waited->value;
#endif
        result->detail = 0;
        return HL_STATUS_OK;
    }
    memset(&record, 0, sizeof(record));
    if (state != NULL && atomic_load_explicit((_Atomic uint32_t *)&state->record->magic, memory_order_acquire) ==
                             HL_ENGINE_CHILD_RESULT_MAGIC)
        memcpy(&record, state->record, sizeof(record));
    hl_production_result_release(host, token);
    result->kind = HL_ENGINE_EXIT_ENGINE_ERROR;
    result->guest_status = HL_STATUS_CORRUPT;
    result->detail = record.magic == HL_ENGINE_CHILD_RESULT_MAGIC ? sizeof(record) : 0;
    if (record.magic != HL_ENGINE_CHILD_RESULT_MAGIC || record.version != HL_ENGINE_CHILD_RESULT_VERSION ||
        waited->detail != HL_HOST_PROCESS_EXIT_CODE)
        return HL_STATUS_CORRUPT;
    if (translations != NULL) *translations = record.translations;
    if (record.engine_status != HL_STATUS_OK) {
        if (record.engine_status < HL_STATUS_INVALID_ARGUMENT || record.engine_status > HL_STATUS_ADDRESS_IN_USE)
            return HL_STATUS_CORRUPT;
        result->guest_status = record.engine_status;
        result->detail = record.detail;
        return (hl_status)record.engine_status;
    }
    if (record.kind == HL_ENGINE_CHILD_RESULT_SIGNAL) {
        if (record.guest_status < 1 || record.guest_status > 64 ||
            waited->value != (uint32_t)(128 + record.guest_status))
            return HL_STATUS_CORRUPT;
        result->kind = HL_ENGINE_EXIT_SIGNAL;
        result->guest_status = record.guest_status;
        result->detail = 0;
        return HL_STATUS_OK;
    }
    if (record.kind != HL_ENGINE_CHILD_RESULT_EXIT || (uint32_t)record.guest_status > 255u ||
        waited->value != (uint32_t)record.guest_status)
        return HL_STATUS_CORRUPT;
    result->kind = HL_ENGINE_EXIT_CODE;
    result->guest_status = record.guest_status;
    result->detail = 0;
    return HL_STATUS_OK;
}

static const hl_engine_backend backend = {HL_PRODUCTION_GUEST_ISA, hl_production_start_process,
                                          hl_production_finish_process, hl_production_result_release};

void hl_target_register_backend(void) {
    hl_engine_backend_register(&backend);
}

#ifndef HL_EMBEDDED_BUILD
__attribute__((constructor)) static void hl_production_register_backend(void) {
    hl_target_register_backend();
}
#endif
