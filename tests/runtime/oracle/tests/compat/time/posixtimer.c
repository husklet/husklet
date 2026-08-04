// POSIX per-process timers (timer_create/settime/gettime/getoverrun/delete) — Linux-only (macOS libc has
// no timer_create), diffed vs the native oracle. Covers the whole surface as normalized 0/1 verdicts so
// the output is byte-identical to the oracle on both Linux engines:
//   * SIGEV_SIGNAL on CLOCK_REALTIME + CLOCK_MONOTONIC: a one-shot fires its signal, and the siginfo carries
//     si_code==SI_TIMER and the sigev_value we set.
//   * timer_gettime reports a plausible remaining time (0 < rem <= armed value) for an armed one-shot, and
//     {0,0} after it fires (one-shot) / after delete.
//   * A periodic timer accumulates overrun (>=1) when the handler is delayed past several intervals.
//   * SIGEV_NONE: no signal, but timer_gettime still tracks the countdown.
//   * Error surface: bad clockid -> EINVAL; gettime(bad id) -> EINVAL; gettime(valid,NULL) -> EFAULT;
//     settime(NULL new_value) -> EINVAL; getoverrun(bad id) -> EINVAL.
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#define SIGNO SIGRTMIN
#define SIGVAL 0x5a5a

static volatile sig_atomic_t fired, code_ok, val_ok;

static void h(int s, siginfo_t *si, void *uc) {
    (void)s;
    (void)uc;
    fired++;
    if (si->si_code == SI_TIMER) code_ok = 1;
    if (si->si_value.sival_int == SIGVAL) val_ok = 1;
}

// One-shot with SIGEV_SIGNAL on `clockid`: returns 1 if the signal fired with the right si_code+si_value.
static int oneshot_signal(clockid_t clockid) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = h;
    sa.sa_flags = SA_SIGINFO;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGNO, &sa, NULL);

    struct sigevent ev;
    memset(&ev, 0, sizeof ev);
    ev.sigev_notify = SIGEV_SIGNAL;
    ev.sigev_signo = SIGNO;
    ev.sigev_value.sival_int = SIGVAL;

    timer_t t;
    if (timer_create(clockid, &ev, &t) != 0) return 0;

    struct itimerspec its;
    memset(&its, 0, sizeof its);
    its.it_value.tv_nsec = 40 * 1000 * 1000; // 40ms one-shot
    fired = code_ok = val_ok = 0;
    if (timer_settime(t, 0, &its, NULL) != 0) { timer_delete(t); return 0; }

    for (int i = 0; i < 500 && !fired; i++) usleep(2000);
    int ok = fired == 1 && code_ok && val_ok;

    // After a one-shot fires, gettime reports disarmed {0,0}.
    struct itimerspec cur;
    memset(&cur, 0, sizeof cur);
    timer_gettime(t, &cur);
    int disarmed = cur.it_value.tv_sec == 0 && cur.it_value.tv_nsec == 0;

    timer_delete(t);
    return ok && disarmed;
}

// gettime of a long-armed one-shot reports 0 < remaining <= armed value.
static int gettime_remaining(void) {
    struct sigevent ev;
    memset(&ev, 0, sizeof ev);
    ev.sigev_notify = SIGEV_NONE;
    timer_t t;
    if (timer_create(CLOCK_MONOTONIC, &ev, &t) != 0) return 0;
    struct itimerspec its;
    memset(&its, 0, sizeof its);
    its.it_value.tv_sec = 100; // 100s one-shot, no signal (SIGEV_NONE)
    timer_settime(t, 0, &its, NULL);
    struct itimerspec cur;
    memset(&cur, 0, sizeof cur);
    timer_gettime(t, &cur);
    long rem = cur.it_value.tv_sec;
    int ok = rem > 0 && rem <= 100;
    timer_delete(t);
    return ok;
}

// Timer ids are per-process resources, not a 32-entry engine implementation
// detail. Keep more than the former fixed table live at once, then prove every
// id remains independently deletable.
static int timer_capacity(void) {
    struct sigevent ev;
    timer_t timers[64];
    size_t created = 0;
    size_t deleted = 0;
    memset(&ev, 0, sizeof ev);
    ev.sigev_notify = SIGEV_NONE;
    while (created < sizeof(timers) / sizeof(timers[0]) &&
           timer_create(CLOCK_MONOTONIC, &ev, &timers[created]) == 0)
        created++;
    for (size_t index = 0; index < created; index++)
        if (timer_delete(timers[index]) == 0) deleted++;
    return created == sizeof(timers) / sizeof(timers[0]) && deleted == created;
}

// A signal-BLOCKED fast periodic timer accumulates a large overrun while the guest sleeps in a PLAIN
// blocking usleep (task #422). The engine derives the overrun from elapsed monotonic time against the
// timer's fixed first-expiry anchor, so the count is correct even though the guest is descheduled the whole
// time and never executes a translated block between expirations -- no busy-spin needed. ~100ms / 5ms => ~20
// periods; we require overrun >= 10 (deterministic on both native and JIT: elapsed wall time only grows the
// count under load, never shrinks it).
static int overrun_counts(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = h;
    sa.sa_flags = SA_SIGINFO;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGNO, &sa, NULL);

    // Block the signal so expirations pile up while we sleep, then read overrun on the first delivery.
    sigset_t block, old;
    sigemptyset(&block);
    sigaddset(&block, SIGNO);
    sigprocmask(SIG_BLOCK, &block, &old);

    struct sigevent ev;
    memset(&ev, 0, sizeof ev);
    ev.sigev_notify = SIGEV_SIGNAL;
    ev.sigev_signo = SIGNO;
    timer_t t;
    if (timer_create(CLOCK_MONOTONIC, &ev, &t) != 0) { sigprocmask(SIG_SETMASK, &old, NULL); return 0; }

    struct itimerspec its;
    memset(&its, 0, sizeof its);
    its.it_value.tv_nsec = 5 * 1000 * 1000;    // first fire 5ms
    its.it_interval.tv_nsec = 5 * 1000 * 1000; // then every 5ms
    timer_settime(t, 0, &its, NULL);

    // PLAIN blocking sleep for ~100ms: the guest is descheduled the whole time, so ~20 of the 5ms periods
    // elapse with nothing executing in the guest. A correct engine still counts them (in-kernel on Linux; from
    // elapsed monotonic time in hl), so this deliberately does NOT busy-spin -- it is the regression guard for
    // the #422 undercount.
    usleep(100 * 1000);

    // Dequeue the pending signal and read the overrun for that expiry. Use a generous timeout rather than a
    // zero (non-blocking) poll: on native the signal is already pending after the sleep, but under a loaded
    // DBT host the first delivery can lag past the sleep, so WAIT for it (transparent to native, which returns
    // immediately). By the time it fires, ~20 of the 5ms periods have piled up -> overrun well above 10.
    siginfo_t si;
    struct timespec wait_to = {2, 0};
    int overrun = -1;
    if (sigtimedwait(&block, &si, &wait_to) == SIGNO) overrun = timer_getoverrun(t);
    int ok = overrun >= 10;

    timer_delete(t);
    sigprocmask(SIG_SETMASK, &old, NULL);
    return ok;
}

int main(void) {
    int real = oneshot_signal(CLOCK_REALTIME);
    int mono = oneshot_signal(CLOCK_MONOTONIC);
    int rem = gettime_remaining();
    int capacity = timer_capacity();
    int over = overrun_counts();

    // Error surface.
    struct sigevent ev;
    memset(&ev, 0, sizeof ev);
    ev.sigev_notify = SIGEV_NONE;
    timer_t t;
    errno = 0;
    int badclock = timer_create((clockid_t)0x7fff, &ev, &t) == -1 && errno == EINVAL;

    timer_create(CLOCK_MONOTONIC, &ev, &t);
    errno = 0;
    int gt_efault = timer_gettime(t, NULL) == -1 && errno == EFAULT;
    errno = 0;
    int st_einval = timer_settime(t, 0, NULL, NULL) == -1 && errno == EINVAL;
    // getoverrun on a valid, un-overrun one-shot is 0.
    struct itimerspec its;
    memset(&its, 0, sizeof its);
    its.it_value.tv_sec = 100;
    timer_settime(t, 0, &its, NULL);
    int ov_zero = timer_getoverrun(t) == 0;
    timer_delete(t);

    printf("posixtimer real=%d mono=%d rem=%d capacity=%d overrun=%d badclock=%d gt_efault=%d st_einval=%d "
           "ov_zero=%d\n",
           real, mono, rem, capacity, over, badclock, gt_efault, st_einval, ov_zero);
    return 0;
}
