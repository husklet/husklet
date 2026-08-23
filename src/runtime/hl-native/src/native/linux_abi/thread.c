// hl/linux_abi -- thread runtime assembled from cohesive capability fragments.
#include "thread/futex_mapping.c"
#include "thread/file_mapping.c"
#include "thread/bus_ranges.c"
#include "thread/fault_wait.c"
static int thread_directed_signal_publish(struct cpu *target, int signal, int tag, int error, int code, uint64_t value,
                                          int pid, int uid, uint64_t address);
static void sigq_drop_target_tid(int tid);
#include "thread/lifecycle.c"
