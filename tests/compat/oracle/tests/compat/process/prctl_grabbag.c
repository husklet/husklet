// prctl grab-bag: the machine-check kill policy (PR_MCE_KILL/PR_MCE_KILL_GET), the per-task perf-events
// toggle (PR_TASK_PERF_EVENTS_DISABLE/ENABLE), and PR_SET_MM. All three are arch-neutral, so the printed
// return-class + errno match native on both aarch64 and x86_64:
//   - PR_MCE_KILL_GET defaults to PR_MCE_KILL_DEFAULT (2); a nonzero unused arg is EINVAL; PR_MCE_KILL_SET
//     stores EARLY/LATE/DEFAULT and GET reads it back; a bad sub-op or value is EINVAL; PR_MCE_KILL_CLEAR
//     resets to DEFAULT.
//   - PR_TASK_PERF_EVENTS_DISABLE/ENABLE always succeed (0), ignoring the remaining args.
//   - PR_SET_MM needs CAP_SYS_RESOURCE, which the container's default cap set lacks, so every sub-op is
//     EPERM (the engine used to silently return 0, claiming to relayout the mm while doing nothing).
// Deterministic verdict (return-class + errno only), so the JIT stdout is byte-identical to native.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <unistd.h>

// prctl option numbers (arch-independent)
#define PR_TASK_PERF_EVENTS_DISABLE 31
#define PR_TASK_PERF_EVENTS_ENABLE 32
#define PR_MCE_KILL 33
#define PR_MCE_KILL_GET 34
#define PR_SET_MM 35

static void show(const char *name, unsigned long op, unsigned long a2, unsigned long a3) {
    errno = 0;
    long r = syscall(SYS_prctl, op, a2, a3, 0UL, 0UL);
    printf("%-18s r=%ld e=%d\n", name, r < 0 ? -1L : r, r < 0 ? errno : 0);
}

int main(void) {
    show("mce_get_default", PR_MCE_KILL_GET, 0, 0);
    show("mce_get_arg_nz", PR_MCE_KILL_GET, 1, 0);
    show("mce_clear", PR_MCE_KILL, 0 /*PR_MCE_KILL_CLEAR*/, 0);
    show("mce_set_early", PR_MCE_KILL, 1 /*PR_MCE_KILL_SET*/, 1 /*EARLY*/);
    show("mce_get_after", PR_MCE_KILL_GET, 0, 0);
    show("mce_set_badsub", PR_MCE_KILL, 7, 0);
    show("mce_set_badval", PR_MCE_KILL, 1, 7);
    show("perf_disable", PR_TASK_PERF_EVENTS_DISABLE, 0, 0);
    show("perf_disable_argnz", PR_TASK_PERF_EVENTS_DISABLE, 1, 2);
    show("perf_enable", PR_TASK_PERF_EVENTS_ENABLE, 0, 0);
    show("set_mm_brk", PR_SET_MM, 6 /*PR_SET_MM_START_BRK*/, 0x1000);
    show("set_mm_badsub", PR_SET_MM, 99, 0);
    return 0;
}
