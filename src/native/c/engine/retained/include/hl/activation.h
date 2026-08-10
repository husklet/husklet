#ifndef HL_ACTIVATION_H
#define HL_ACTIVATION_H

#include "base.h"
#include "engine.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct hl_activation_process hl_activation_process;

/* A borrowed host descriptor as this API carries it, and the one spelling of "absent".
 *
 * The older int32_t entry points below spell absence as -1. That is a fine non-descriptor on a POSIX
 * host, and an unusable one elsewhere: on Win32 the same bit pattern, (HANDLE)-1, is simultaneously
 * INVALID_HANDLE_VALUE and the pseudo-handle GetCurrentProcess() returns. An "absent" value that is also
 * a live handle to the calling process cannot be made safe by documenting it: the first caller that
 * forwards the value unchecked has handed something a handle to the embedder itself. So the sentinel
 * moves off -1 entirely rather than being reinterpreted per host.
 *
 * HL_ACTIVATION_DESCRIPTOR_NONE is 0, which no host in this API's reach ever hands out as a live
 * descriptor: NULL is never a valid Win32 handle, and a POSIX caller holding descriptor 0 duplicates it
 * before passing it here. That costs nothing in practice -- absence already means "inherit this
 * application's stream", which for `input` is descriptor 0 anyway -- and it makes a zeroed structure
 * mean "nothing attached" instead of "attach descriptor 0", which is the safer of the two defaults.
 *
 * The width is for the type, not the range. Descriptor values remain 32-bit significant by contract;
 * 64 bits only keeps the type from being mistaken for, or silently interconverted with, the signed
 * descriptor numbers it replaces. Values above INT32_MAX are rejected, not truncated. */
typedef uint64_t hl_activation_descriptor;
#define HL_ACTIVATION_DESCRIPTOR_NONE ((hl_activation_descriptor)0)

/* Borrowed host descriptors for the three process streams. HL_ACTIVATION_DESCRIPTOR_NONE inherits the
 * application's stream; any other value is duplicated into the reexecuted child during start.
 * Ownership never transfers, and the caller may close a descriptor as soon as start returns, whether
 * start succeeds or fails. */
typedef struct hl_activation_streams {
    hl_activation_descriptor input;
    hl_activation_descriptor output;
    hl_activation_descriptor error;
} hl_activation_streams;

/* The pre-hl_activation_descriptor form of hl_activation_streams, retained unchanged for existing
 * callers. -1 in a field inherits the application's stream; a nonnegative descriptor is duplicated into
 * the reexecuted child. Same ownership rules. New code should prefer hl_activation_streams. */
typedef struct hl_activation_stdio {
    int32_t input;
    int32_t output;
    int32_t error;
} hl_activation_stdio;

typedef struct hl_terminal_size {
    uint16_t rows;
    uint16_t columns;
} hl_terminal_size;

typedef struct hl_process_domain {
    uint64_t identity[2];
} hl_process_domain;

typedef struct hl_activation_process_info {
    uint64_t host_id;
    uint32_t initial;
    uint32_t reserved;
} hl_activation_process_info;

HL_API hl_status hl_activation_start(const char *executable, uint32_t guest_isa, const char *config_path,
                                     hl_activation_process **out_process);
HL_API hl_status hl_activation_start_with_stdio(const char *executable, uint32_t guest_isa, const char *config_path,
                                                const hl_activation_stdio *stdio, hl_activation_process **out_process);
/* Experimental activation plumbing for an engine/provider transport. The descriptor is borrowed and
 * transferred with SCM_RIGHTS; it is not a guest descriptor and is never exposed through discovery. */
HL_API hl_status hl_activation_start_with_transport(const char *executable, uint32_t guest_isa, const char *config_path,
                                                    const hl_activation_stdio *stdio, int32_t transport,
                                                    hl_activation_process **out_process);
/* Starts the child in a new session with a controlling terminal. The returned
 * master descriptor is owned by the caller; stdin/stdout/stderr are merged on
 * it. The initial size is applied before the child can execute. */
HL_API hl_status hl_activation_start_terminal(const char *executable, uint32_t guest_isa, const char *config_path,
                                              hl_terminal_size size, int32_t *out_master,
                                              hl_activation_process **out_process);
/* Starts with both a controlling terminal and a provider transport. The
 * transport descriptor is borrowed under the same rules as
 * hl_activation_start_with_transport. */
HL_API hl_status hl_activation_start_terminal_with_transport(const char *executable, uint32_t guest_isa,
                                                             const char *config_path, hl_terminal_size size,
                                                             int32_t transport, int32_t *out_master,
                                                             hl_activation_process **out_process);
/* General activation entry point: any combination of process streams OR a controlling terminal, an optional
 * provider transport, and an optional checkpoint broker (include/hl/checkpoint_stream.h). Every descriptor
 * is borrowed and transferred with SCM_RIGHTS; none of them is a guest descriptor. `size` non-NULL selects
 * the terminal form and requires `out_master`; `streams` and `size` are mutually exclusive. `transport`,
 * `checkpoint` and `trigger` are HL_ACTIVATION_DESCRIPTOR_NONE when not requested. `out_master` receives the
 * caller-owned terminal master, or HL_ACTIVATION_DESCRIPTOR_NONE when no terminal was requested and on every
 * failure path. */
HL_API hl_status hl_activation_start_with_streams(const char *executable, uint32_t guest_isa, const char *config_path,
                                                  const hl_activation_streams *streams, const hl_terminal_size *size,
                                                  hl_activation_descriptor transport,
                                                  hl_activation_descriptor checkpoint, hl_activation_descriptor trigger,
                                                  hl_activation_descriptor *out_master,
                                                  hl_activation_process **out_process);
HL_API hl_status hl_activation_terminal_resize(hl_activation_descriptor master, hl_terminal_size size);
/* The pre-hl_activation_descriptor forms of the two calls above, retained unchanged for existing callers.
 * They are the same operations with -1 in place of HL_ACTIVATION_DESCRIPTOR_NONE. */
HL_API hl_status hl_activation_start_with_channels(const char *executable, uint32_t guest_isa, const char *config_path,
                                                   const hl_activation_stdio *stdio, const hl_terminal_size *size,
                                                   int32_t transport, int32_t checkpoint, int32_t trigger,
                                                   int32_t *out_master, hl_activation_process **out_process);
HL_API hl_status hl_terminal_resize(int32_t master, hl_terminal_size size);
/* Returns the native child process identifier while the opaque handle exists. */
HL_API hl_status hl_activation_process_id(const hl_activation_process *process, uint64_t *out_process_id);
/* Takes a deterministic snapshot of verified live process-domain members. Set
 * capacity to zero to query the required count. A racing exit may make a
 * subsequent snapshot smaller. */
HL_API hl_status hl_activation_domain_processes(hl_process_domain domain, uint64_t initial_process_id,
                                                hl_activation_process_info *processes, uint32_t capacity,
                                                uint32_t *out_count);
/* wait is idempotent: completed status/result values are cached in the handle. */
HL_API hl_status hl_activation_wait(hl_activation_process *process, hl_engine_exit *out_exit);
/* try_wait sets out_ready=0 without modifying out_exit while the child runs. */
HL_API hl_status hl_activation_try_wait(hl_activation_process *process, uint32_t *out_ready, hl_engine_exit *out_exit);
HL_API hl_status hl_activation_kill(hl_activation_process *process);
/* Terminates every live member whose native PID and birth identity match this domain registry.
 * Repeated termination is successful; zero identities are rejected. */
HL_API hl_status hl_activation_domain_terminate(hl_process_domain domain);
/* Destroying a live handle force-stops and reaps it; destroying a waited handle
 * only releases cached parent-side state. */
HL_API void hl_activation_process_destroy(hl_activation_process *process);

/* Reexecutes the current application and activates the embedded guest backend
 * before application main. config_path names an ABI5 launch file and is
 * consumed by the child. Both paths must be absolute. */
HL_API hl_status hl_activation_spawn(const char *executable, uint32_t guest_isa, const char *config_path,
                                     hl_engine_exit *out_exit);

#ifdef __cplusplus
}
#endif
#endif
