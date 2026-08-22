#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include "hl/linux.h"
#include "probe.h"
#include "../cpu.h"
#include "../range.h"
#include "../system.h"
#include "../resolve.h"
#include "../sync.h"

#include <errno.h>
#include <dirent.h>
#include <fcntl.h>
#include <limits.h>
#include <pthread.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/vfs.h>
#include <sys/eventfd.h>
#include <sys/inotify.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/timerfd.h>
#include <sys/types.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <termios.h>
#include <netinet/in.h>
#include <netinet/tcp.h> /* the TCP_* option names the network group's table maps onto */
#include <sys/un.h>
#include <time.h>
#include <unistd.h>

#define HL_LINUX_HANDLE_CAPACITY 4096u
#define HL_LINUX_TIMER_CAPACITY 256u
#define HL_LINUX_COUNTER_SUBSCRIPTIONS_INITIAL 128u

typedef enum hl_linux_handle_kind {
    HL_LINUX_HANDLE_NONE = 0,
    HL_LINUX_HANDLE_MAPPING = 1,
    HL_LINUX_HANDLE_FILE = 2,
    HL_LINUX_HANDLE_SOCKET = 3,
    HL_LINUX_HANDLE_POLLSET = 4,
    HL_LINUX_HANDLE_SHARED_MEMORY = 5,
    HL_LINUX_HANDLE_PROCESS = 6,
    HL_LINUX_HANDLE_COUNTER = 7,
    HL_LINUX_HANDLE_TRANSFER = 8,
    HL_LINUX_HANDLE_DIRECTORY = 9,
    HL_LINUX_HANDLE_WATCH = 10,
    HL_LINUX_HANDLE_STREAM = 11
} hl_linux_handle_kind;

typedef struct hl_linux_handle_entry {
    uint32_t generation;
    uint16_t kind;
    uint16_t reserved;
    int descriptor;
    void *address;
    void *executable_address;
    uint64_t size;
    /* Mapping handles only: the subranges of [address, address + size) a partial unmap gave back.
     * address and size stay the addressing frame every offset-keyed call is measured against, so
     * what the handle still holds has to be recorded beside them rather than folded into them. */
    hl_host_hole_set retired;
    int wake_descriptor;
    uint32_t process_reaped;
    uint32_t process_waiting;
    uint32_t process_waiters;
    uint32_t process_exit_kind;
    uint32_t process_exit_value;
} hl_linux_handle_entry;

typedef struct hl_linux_timer_entry {
    hl_host_handle pollset;
    uint64_t token;
    int descriptor;
} hl_linux_timer_entry;

typedef struct hl_linux_watch {
    int watch_id;
    uint64_t delivered_generation;
    uint64_t modified_ns;
    uint64_t changed_ns;
    nlink_t links;
    hl_host_watch_record record;
} hl_linux_watch;

typedef struct hl_linux_counter_subscription {
    struct hl_host_linux *host;
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
} hl_linux_counter_subscription;

#define HL_LINUX_DIRECTORY_WATCHES 256u

typedef struct hl_linux_directory_watch {
    int watch;
    uint64_t token;
    uint32_t interests;
    uint32_t active;
} hl_linux_directory_watch;

typedef struct hl_linux_directory_object {
    uint32_t references;
    uint32_t pending_count;
    uint32_t pending_capacity;
    hl_linux_directory_watch *watches;
    uint32_t watch_capacity;
    hl_host_directory_record *pending;
} hl_linux_directory_object;

struct hl_host_linux {
    pthread_mutex_t lock;
    pthread_mutex_t fork_gate;
    pthread_cond_t process_changed;
    uint32_t destroying;
    hl_host_sync_registry *sync;
    hl_linux_handle_entry *handles;
    uint32_t handle_capacity;
    hl_linux_timer_entry *timers;
    uint32_t timer_capacity;
    hl_linux_counter_subscription **counter_subscriptions;
    uint32_t counter_subscription_capacity;
};

uint32_t hl_host_linux_active_mappings(hl_host_linux *host) {
    uint32_t active = 0;
    uint32_t index;
    if (host == NULL) return 0;
    pthread_mutex_lock(&host->lock);
    for (index = 0; index < host->handle_capacity; ++index)
        if (host->handles[index].kind == HL_LINUX_HANDLE_MAPPING) ++active;
    pthread_mutex_unlock(&host->lock);
    return active;
}

static hl_host_result hl_linux_fork_complete(void *context);
static hl_host_result hl_linux_fork_child(void *context);
static hl_host_result hl_linux_counter_unsubscribe(void *context, hl_host_handle subscription);
static hl_host_result hl_linux_close_descriptor(void *context, hl_host_handle handle);
static hl_host_result hl_linux_close_descriptor_kind(void *context, hl_host_handle handle,
                                                     hl_linux_handle_kind expected);
static void hl_linux_counter_unsubscribe_all(hl_host_linux *host, hl_host_handle counter);
static int hl_linux_descriptor(hl_host_linux *host, hl_host_handle handle, hl_linux_handle_kind first,
                               hl_linux_handle_kind second);
