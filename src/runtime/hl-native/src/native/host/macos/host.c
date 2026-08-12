#define _DARWIN_C_SOURCE

#include "hl/macos.h"
#include "probe.h"
#include "../range.h"
#include "../system.h"
#include "../resolve.h"
#include "../sync.h"

#include <errno.h>
#include <sys/resource.h>
#include <dirent.h>
#include <fcntl.h>
#include <libkern/OSCacheControl.h>
#include <limits.h>
#include <mach/mach.h>
#include <mach/mach_time.h>
#include <mach/mach_vm.h>
#include <mach/thread_policy.h>
#include <pthread.h>
#include <poll.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/mount.h>
#include <sys/event.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/socket.h>
#include <sys/sem.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <termios.h>
#include <time.h>
#include <unistd.h>

#define HL_MACOS_MAPPING_CAPACITY 4096u
#define HL_MACOS_LINUX_PAGE_SIZE 4096u
#define HL_MACOS_FILE_CAPACITY 1024u
#define HL_MACOS_PROCESS_CAPACITY 1024u
#define HL_MACOS_EVENT_CAPACITY 64u
#define HL_MACOS_TIMER_CAPACITY 32u
#define HL_MACOS_COUNTER_CAPACITY 128u
#define HL_MACOS_TRANSFER_CAPACITY 64u
#define HL_MACOS_DIRECTORY_CAPACITY 128u
#define HL_MACOS_DIRECTORY_WATCH_CAPACITY 256u
#define HL_MACOS_WATCH_CAPACITY 128u
#define HL_MACOS_COUNTER_SUBSCRIPTIONS_INITIAL 128u

typedef struct hl_macos_mapping {
    uint32_t generation;
    uint32_t active;
    void *writable;
    void *executable;
    uint64_t size;
    /* The subranges of [writable, writable + size) a partial unmap gave back. writable and size
     * stay the addressing frame every offset-keyed call is measured against, so what the handle
     * still holds has to be recorded beside them rather than folded into them. */
    hl_host_hole_set retired;
} hl_macos_mapping;

typedef struct hl_macos_stream_shared {
    int semaphore;
    uint32_t references;
} hl_macos_stream_shared;

typedef struct hl_macos_directory_shared {
    pthread_mutex_t lock;
    uint32_t references;
    uint64_t position;
} hl_macos_directory_shared;

typedef struct hl_macos_file {
    uint32_t generation;
    uint32_t active;
    uint32_t shared;
    int descriptor;
    int append_descriptor;
    hl_macos_stream_shared *stream;
    uint32_t stream_endpoint;
    DIR *directory;
    uint64_t directory_position;
    hl_macos_directory_shared *directory_shared;
} hl_macos_file;

typedef struct hl_macos_process {
    uint32_t generation;
    uint32_t active;
    pid_t pid;
    uint32_t reaped;
    uint32_t waiting;
    uint32_t waiters;
    uint32_t exit_kind;
    uint32_t exit_value;
} hl_macos_process;

typedef struct hl_macos_timer {
    uint64_t token;
    uint64_t interval_ns;
    uint32_t active;
} hl_macos_timer;

typedef struct hl_macos_event {
    uint32_t generation;
    uint32_t active;
    int descriptor;
    hl_macos_timer *timers;
    uint32_t timer_capacity;
} hl_macos_event;

typedef struct hl_macos_watch {
    uint32_t generation;
    uint32_t active;
    int descriptor;
    uint64_t delivered_generation;
    uint64_t modified_ns;
    uint64_t changed_ns;
    nlink_t links;
    hl_host_watch_record record;
} hl_macos_watch;

typedef struct hl_macos_counter_shared {
    pthread_mutex_t lock;
    uint64_t value;
    uint32_t flags;
    uint32_t references;
} hl_macos_counter_shared;

typedef struct hl_macos_counter_object {
    hl_macos_counter_shared *shared;
    int backing;
    int readable;
    int signal;
} hl_macos_counter_object;

typedef struct hl_macos_counter {
    uint32_t generation;
    uint32_t active;
    hl_macos_counter_object *object;
    uint32_t rights;
} hl_macos_counter;

typedef struct hl_macos_counter_subscription {
    uint32_t generation;
    uint32_t active;
    uint32_t retiring;
    hl_host_handle counter;
    int descriptor;
    int wake[2];
    pthread_t thread;
    void (*notify)(void *, uint64_t);
    void *observer;
    uint64_t token;
} hl_macos_counter_subscription;

typedef struct hl_macos_transfer {
    uint32_t generation;
    uint32_t active;
    int descriptor;
} hl_macos_transfer;

typedef struct hl_macos_directory_watch {
    uint64_t token;
    uint32_t interests;
    int descriptor;
    uint32_t active;
} hl_macos_directory_watch;

typedef struct hl_macos_directory_object {
    uint32_t references;
    int descriptor;
    hl_macos_directory_watch *watches;
    uint32_t watch_capacity;
} hl_macos_directory_object;

typedef struct hl_macos_directory {
    uint32_t generation;
    uint32_t active;
    hl_macos_directory_object *object;
} hl_macos_directory;

struct hl_host_macos {
    hl_host_sync_registry *sync;
    hl_macos_mapping *mappings;
    hl_macos_file *files;
    hl_macos_process *processes;
    hl_macos_event *events;
    hl_macos_counter *counters;
    hl_macos_counter_subscription **counter_subscriptions;
    hl_macos_transfer *transfers;
    hl_macos_directory *directories;
    hl_macos_watch *watches;
    pthread_cond_t process_changed;
    pthread_mutex_t lock;
    pthread_mutex_t fork_gate;
    uint32_t destroying;
    uint32_t mapping_capacity;
    uint32_t file_capacity;
    uint32_t process_capacity;
    uint32_t event_capacity;
    uint32_t counter_capacity;
    uint32_t counter_subscription_capacity;
    uint32_t transfer_capacity;
    uint32_t directory_capacity;
    uint32_t watch_capacity;
};

#include "memory_clock.c"
#include "file.c"
#include "stream.c"
#include "file_storage.c"
#include "file_namespace.c"
#include "ipc.c"
#include "event_process.c"
#include "terminal_lifecycle.c"
