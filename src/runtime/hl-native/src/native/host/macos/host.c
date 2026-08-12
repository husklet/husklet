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

#include "handles.c"
#include "memory/mapping.c"
#include "time/clock.c"
#include "time/sleep.c"
#include "fs/file.c"
#include "fs/stream.c"
#include "fs/storage.c"
#include "fs/namespace.c"
#include "memory/shared.c"
#include "io/directory.c"
#include "io/transfer.c"
#include "io/counter.c"
#include "fs/watch.c"
#include "io/event.c"
#include "process/child.c"
#include "sync/futex.c"
#include "process/terminal.c"
#include "sync/fork.c"
#include "logging.c"
#include "fs/private.c"
#include "context.c"
