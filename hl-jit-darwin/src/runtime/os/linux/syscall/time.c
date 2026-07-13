// Extracted from service(): Time — clock_gettime/nanosleep/gettimeofday syscalls. Returns 1 if nr was handled, 0
// otherwise. Included by service.c after service/helpers.c, before service() — same TU scope (globals + helpers).

// ===================== POSIX per-process timers (timer_create/_settime/_gettime/_delete/_getoverrun)
// macOS has no timer_create(2). Emulate Linux POSIX timers with a single shared kqueue holding one
// EVFILT_TIMER per armed timer (kevent ident = our small-int timer id) plus ONE background thread
// blocked in kevent(). On expiry that thread raises the guest signal through the engine's own
// async-signal path -- set the g_pending bit + poke the signalfd self-pipe, byte-for-byte what
// host_sigh() does for a real host signal -- so maybe_deliver_signal() builds the rt_sigframe at the
// next dispatcher boundary. We NEVER raise a real host signal into the MAP_JIT thread. kqueue can't
// express "first fire at it_value, then every it_interval" in one entry, so when those differ we arm
// a one-shot for it_value and the thread re-arms it periodic on the first fire (two-phase). Remaining
// time (timer_gettime) and overrun are tracked in software from the recorded deadline + kevent .data.
#define GTIMER_MAX 32
#define DD_SI_TIMER (-2) // Linux si_code SI_TIMER (the value the guest's siginfo expects)

struct gtimer {
    int used;                   // slot allocated by timer_create, not yet timer_delete'd
    int clockid;                // guest clockid (only used to read "now" for a TIMER_ABSTIME arm)
    int notify;                 // sigev_notify: SIGEV_SIGNAL(0)/SIGEV_NONE(1)/SIGEV_THREAD(2)/SIGEV_THREAD_ID(4)
    int signo;                  // Linux signal number to raise on expiry (SIGEV_SIGNAL/_THREAD_ID)
    uint64_t sigval;            // sigev_value (carried into the delivered siginfo si_value)
    uint64_t interval_ns;       // it_interval (0 => one-shot)
    uint64_t next_ns;           // absolute CLOCK_MONOTONIC ns of the next expiry (0 => disarmed)
    uint64_t first_ns;          // absolute CLOCK_MONOTONIC ns of expiry #0 -- FIXED for the armed lifetime (may be in
                                // the PAST for a TIMER_ABSTIME arm whose deadline already elapsed). The whole overrun
                                // count is derived from elapsed monotonic time against this anchor, so it is correct
                                // regardless of whether the guest (or the drain thread) was running between expiries.
    uint64_t reported_expiries; // # expirations already folded into prior timer_getoverrun deliveries; the next
                                // delivery's overrun is the count of expirations that piled up since this mark.
    int periodic_armed;         // the kqueue entry is already periodic (no re-arm needed on fire)
};

// Total expirations that have occurred by absolute monotonic time `now` for an armed timer: expiry #0 at
// first_ns, then one every interval_ns. A disarmed timer (first_ns==0) or a not-yet-reached one-shot => 0.
// Independent of the guest/drain-thread execution -- this is Linux's in-kernel "elapsed periods" count.
static uint64_t gtimer_expiries_elapsed(const struct gtimer *t, uint64_t now) {
    if (!t->first_ns || now < t->first_ns) return 0;
    if (t->interval_ns == 0) return 1;               // one-shot: exactly one expiry once its deadline passes
    return (now - t->first_ns) / t->interval_ns + 1; // periodic: expiry #0 plus every whole interval since
}

// Overrun to report at a delivery happening "now": the number of EXTRA expirations that piled up since the
// last delivery (reported_expiries), i.e. all currently-elapsed expirations minus the one being delivered.
// Advances reported_expiries past this delivery. Capped at INT_MAX exactly like the kernel (CVE-2018-12896 /
// LTP timer_settime03). Caller holds g_gtimer_lk.
static int gtimer_take_overrun(struct gtimer *t, uint64_t now) {
    uint64_t elapsed = gtimer_expiries_elapsed(t, now);
    if (elapsed <= t->reported_expiries) return 0;       // nothing new since the last delivery
    uint64_t extra = elapsed - t->reported_expiries - 1; // subtract the expiry that this signal delivers
    t->reported_expiries = elapsed;
    return extra > 2147483647ull ? 2147483647 : (int)extra;
}
static struct gtimer g_gtimer[GTIMER_MAX];
static int g_gtimer_kq = -1;
static pthread_t g_gtimer_thr;
static int g_gtimer_thr_up;
static pthread_mutex_t g_gtimer_lk = PTHREAD_MUTEX_INITIALIZER;

static uint64_t gtimer_now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ull + (uint64_t)ts.tv_nsec;
}

// guest clockid -> macOS clockid (only REALTIME vs MONOTONIC matters for the abstime "now" read)
static clockid_t gtimer_hostclock(int clk) {
    return (clk == 0 || clk == 5) ? CLOCK_REALTIME : CLOCK_MONOTONIC;
}

// arm/re-arm slot `id`'s EVFILT_TIMER. Caller holds g_gtimer_lk. value_ns = delay to the next fire
// (0 => fire asap), interval_ns 0 => one-shot. When value==interval we arm a native periodic timer;
// otherwise a one-shot the drain thread promotes to periodic on the first fire.
static int gtimer_arm(int id, uint64_t value_ns, uint64_t interval_ns) {
    struct kevent kv;
    int periodic = (interval_ns > 0 && value_ns == interval_ns);
    uint16_t fl = EV_ADD | (periodic ? 0 : EV_ONESHOT);
    int64_t d = (int64_t)value_ns;
    if (d < 0) d = 0;
    EV_SET(&kv, (uintptr_t)id, EVFILT_TIMER, fl, NOTE_NSECONDS, d, NULL);
    if (kevent(g_gtimer_kq, &kv, 1, NULL, 0, NULL) < 0) return -errno;
    g_gtimer[id].periodic_armed = periodic;
    return 0;
}

static void gtimer_disarm(int id) {
    struct kevent kv;
    EV_SET(&kv, (uintptr_t)id, EVFILT_TIMER, EV_DELETE, 0, 0, NULL);
    kevent(g_gtimer_kq, &kv, 1, NULL, 0, NULL); // ENOENT if the one-shot already fired -> ignore
    g_gtimer[id].next_ns = 0;
    g_gtimer[id].first_ns = 0;
    g_gtimer[id].reported_expiries = 0;
    g_gtimer[id].periodic_armed = 0;
}

// fill an itimerspec at `out`: it_interval [0..16), it_value=remaining [16..32). A disarmed timer
// (next_ns==0) reports it_value 0 (POSIX), and a periodic timer past its deadline folds into the period.
static void gtimer_fill_curr(struct gtimer *t, void *out) {
    uint64_t iv = t->interval_ns, rem = 0;
    if (t->next_ns) {
        uint64_t now = gtimer_now_ns();
        if (t->next_ns > now)
            rem = t->next_ns - now;
        else if (iv) {
            uint64_t past = now - t->next_ns;
            rem = iv - (past % iv);
        }
    }
    uint64_t *o = (uint64_t *)out;
    o[0] = iv / 1000000000ull;
    o[1] = iv % 1000000000ull; // it_interval
    o[2] = rem / 1000000000ull;
    o[3] = rem % 1000000000ull; // it_value (remaining)
}

static void *gtimer_loop(void *arg) {
    (void)arg;
    for (;;) {
        struct kevent ev;
        int n = kevent(g_gtimer_kq, NULL, 0, &ev, 1, NULL);
        if (n < 0) {
            if (errno == EINTR) continue;
            break;
        } // kq closed -> thread exits
        if (n == 0) continue;
        int id = (int)ev.ident;
        if (id < 0 || id >= GTIMER_MAX) continue;
        struct gtimer *t = &g_gtimer[id];
        pthread_mutex_lock(&g_gtimer_lk);
        if (!t->used || t->next_ns == 0) {
            pthread_mutex_unlock(&g_gtimer_lk);
            continue;
        } // raced delete/disarm
        // The overrun COUNT is not computed here: it is derived from elapsed monotonic time against the fixed
        // first_ns anchor at delivery/timer_getoverrun (gtimer_take_overrun), so it stays correct even when the
        // drain thread is starved or the guest is descheduled in a blocking sleep and misses whole periods.
        // This loop just advances the gettime bookkeeping deadline and (re)delivers the signal. For a one-shot
        // (interval 0) that means disarm; for a two-phase periodic (it_value != it_interval) it promotes the
        // kqueue entry to native periodic on the first fire.
        uint64_t now = gtimer_now_ns();
        if (!t->periodic_armed && t->interval_ns > 0) {
            gtimer_arm(id, t->interval_ns, t->interval_ns); // two-phase: promote to periodic now
            t->next_ns = now + t->interval_ns;
        } else if (t->periodic_armed) {
            t->next_ns = now + t->interval_ns; // advance bookkeeping deadline
        } else {
            t->next_ns = 0; // pure one-shot is done -> disarmed
        }
        int signo = t->signo;
        int notify = t->notify;
        uint64_t sv = t->sigval;
        pthread_mutex_unlock(&g_gtimer_lk);
        // SIGEV_NONE: pollable only (timer_gettime/_getoverrun) -- the bookkeeping above is enough.
        if (notify == 1) continue;
        if (signo >= 1 && signo <= 64) {
            // carry SI_TIMER + sigev_value into the handler's siginfo (consumed on delivery)
            g_sigcode[signo] = DD_SI_TIMER;
            g_sigval[signo] = sv;
            __atomic_or_fetch(&g_pending, 1ull << signo, __ATOMIC_SEQ_CST);
            sfd_deliver(signo); // wake signalfd/epoll (per-OFD mask)
        }
    }
    return NULL;
}

// POSIX: per-process timers are NOT inherited across fork(). Drop the inherited table + the now-dead
// kqueue/drain thread so a forked child starts clean (lazy-recreated on its own first timer_create).
static void gtimer_atfork_child(void) {
    memset(g_gtimer, 0, sizeof g_gtimer);
    g_gtimer_kq = -1;
    g_gtimer_thr_up = 0;
    pthread_mutex_init(&g_gtimer_lk, NULL);
}

// lazily bring up the shared kqueue + drain thread. Caller holds g_gtimer_lk.
static int gtimer_init(void) {
    static int reg = 0;
    if (!reg) {
        pthread_atfork(NULL, NULL, gtimer_atfork_child);
        reg = 1;
    }
    if (g_gtimer_kq < 0) {
        g_gtimer_kq = kqueue();
        if (g_gtimer_kq < 0) return -errno;
        fcntl(g_gtimer_kq, F_SETFD, FD_CLOEXEC);
    }
    if (!g_gtimer_thr_up) {
        if (pthread_create(&g_gtimer_thr, NULL, gtimer_loop, NULL) != 0) return -EAGAIN;
        g_gtimer_thr_up = 1;
    }
    return 0;
}

static int svc_time(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                    uint64_t a5) {
    switch (nr) {
    // ===================== Time — clock_gettime/nanosleep/gettimeofday (Linux clock-id translation)
    // =====================
    case 101: {
        // nanosleep(req, rem). Validate the pointers (EFAULT), then PROPAGATE the host result -- the old
        // code swallowed everything and always returned 0, so an out-of-range tv_nsec / negative tv_sec
        // (EINVAL) or a signal interruption (EINTR + remaining) was lost. macOS nanosleep already rejects
        // an out-of-range/negative timespec with EINVAL, matching Linux. Retry in place only on a
        // SPURIOUS/internal EINTR (nothing deliverable to the guest); surface a real EINTR so the
        // dispatcher runs the pending handler, exactly like poll/read. (LTP nanosleep02)
        if (!host_range_mapped((uintptr_t)a0, sizeof(struct timespec))) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        const struct timespec *req = (const struct timespec *)a0;
        if (req->tv_sec < 0 || req->tv_nsec < 0 || req->tv_nsec >= 1000000000L) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL); // out-of-range/negative timespec (LTP nanosleep02)
            break;
        }
        // Sleep against a FIXED absolute deadline. dd preempts a blocking syscall with an internal async
        // signal (block-back-edge preemption) that is invisible to the guest; the old code retried
        // with `nanosleep(a0, a1)`, so when rem(a1) was NULL every such interruption RESTARTED the whole
        // duration -- one extra full sleep per preemption (LTP nanosleep01 "slept for too long", ~request +
        // a full re-sleep). Compute the deadline once and re-sleep only the true remainder, so an internal
        // wakeup never extends the sleep and a deliverable guest signal returns EINTR with rem correctly set.
        struct timespec now, deadline;
        clock_gettime(CLOCK_MONOTONIC, &deadline);
        deadline.tv_sec += req->tv_sec;
        deadline.tv_nsec += req->tv_nsec;
        if (deadline.tv_nsec >= 1000000000L) {
            deadline.tv_sec++;
            deadline.tv_nsec -= 1000000000L;
        }
        sleep_precise_begin(); // uncoalesced (precise) wakeup for this sleep; restored below
        int r = 0;
        for (;;) {
            struct timespec d;
            clock_gettime(CLOCK_MONOTONIC, &now);
            d.tv_sec = deadline.tv_sec - now.tv_sec;
            d.tv_nsec = deadline.tv_nsec - now.tv_nsec;
            if (d.tv_nsec < 0) {
                d.tv_sec--;
                d.tv_nsec += 1000000000L;
            }
            if (d.tv_sec < 0 || (d.tv_sec == 0 && d.tv_nsec <= 0)) {
                r = 0;
                break;
            } // deadline reached
            r = nanosleep(&d, NULL);
            if (r == 0) break;
            if (svc_poll_retry(c)) continue; // internal/spurious wakeup -> re-sleep the true remainder
            // A deliverable guest signal (or a real error): surface it, writing the remaining time to rem.
            if (a1 && host_range_mapped((uintptr_t)a1, sizeof(struct timespec))) {
                struct timespec rem;
                clock_gettime(CLOCK_MONOTONIC, &now);
                rem.tv_sec = deadline.tv_sec - now.tv_sec;
                rem.tv_nsec = deadline.tv_nsec - now.tv_nsec;
                if (rem.tv_nsec < 0) {
                    rem.tv_sec--;
                    rem.tv_nsec += 1000000000L;
                }
                if (rem.tv_sec < 0) {
                    rem.tv_sec = 0;
                    rem.tv_nsec = 0;
                }
                *(struct timespec *)a1 = rem;
            }
            break;
        }
        sleep_precise_end(); // drop back to normal timeshare scheduling
        G_RET(c) = r < 0 ? (uint64_t)(int64_t)(-errno) : 0;
        break;
    }
    case 113: {
        // clock_gettime -- Linux clockid -> macOS
        clockid_t mc;
        switch ((int)a0) {
        case 0:
        // REALTIME(_COARSE)
        case 5: mc = CLOCK_REALTIME; break;
        case 1:
        case 6:
        // MONOTONIC(_COARSE)/BOOTTIME
        case 7: mc = CLOCK_MONOTONIC; break;
        case 2: mc = CLOCK_PROCESS_CPUTIME_ID; break;
        case 3: mc = CLOCK_THREAD_CPUTIME_ID; break;
        case 4: mc = CLOCK_MONOTONIC_RAW; break;
        default: mc = CLOCK_MONOTONIC; break;
        }
        struct timespec ts;
        clock_gettime(mc, &ts);
        // clock_gettime(clk, NULL) is -EFAULT on Linux -- UNLIKE gettimeofday(NULL)/clock_getres(clk,NULL),
        // which are legal no-ops that return 0. The kernel unconditionally copies the result out, so a NULL
        // or otherwise-bad buffer faults. Validate the full 16 bytes; host_range_mapped(NULL,16) probes addr
        // 0 -> unmapped -> EFAULT, so the NULL case falls out here too (the old `if (g)` wrongly returned 0).
        if (!host_range_mapped((uintptr_t)a1, 16)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        uint64_t *g = (uint64_t *)a1;
        g[0] = ts.tv_sec;
        g[1] = ts.tv_nsec;
        G_RET(c) = 0;
        break;
    }
    case 114: {
        // clock_getres(clockid, res) -> 1ns. validate the clockid FIRST -- an unknown/negative clock
        // is -EINVAL on Linux EVEN with a NULL res (unlike gettimeofday(NULL), the kernel rejects the clock
        // before it would have copied anything out). The POSIX clocks REALTIME(0)..BOOTTIME_ALARM(9) plus
        // TAI(11) all report a 1ns resolution here; clk_id=-1 (LTP clock_getres01) -> EINVAL.
        if ((int)a0 < 0 || (int)a0 > 11) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        if (a1) {
            if (!host_range_mapped((uintptr_t)a1, 16)) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            *(uint64_t *)a1 = 0;
            *(uint64_t *)(a1 + 8) = 1;
        }
        G_RET(c) = 0;
        break;
    }
    case 115: {
        // clock_nanosleep(clockid, flags, request, remain). macOS has no clock_nanosleep, and TIMER_ABSTIME
        // means "sleep UNTIL the absolute deadline" -- treating it as relative would sleep for ~uptime
        // seconds and hang. Emulate ABSTIME by sleeping (deadline - now); relative falls back to nanosleep.
        int flags = (int)a1;
        int clk = (int)a0;
        const struct timespec *req = (const struct timespec *)a2;
        // validate the clockid before touching anything (LTP clock_nanosleep01). An unknown/negative
        // clock is -EINVAL; CLOCK_THREAD_CPUTIME_ID(3) has no kernel nsleep so the raw syscall returns
        // -EOPNOTSUPP (Linux errno 95 -- svc_time is NOT run through svc_done's m2l map, so hard-code the
        // Linux value; glibc's wrapper remaps this to EINVAL, which the libc variant of the test expects).
        if (clk < 0 || clk > 11) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        if (clk == 3) {
            G_RET(c) = (uint64_t)(int64_t)(-95);
            break;
        }
        // The request timespec is dereferenced by both paths below -> a bad pointer must EFAULT, not fault
        // the engine (glibc's nanosleep() lands here as clock_nanosleep(CLOCK_REALTIME,0,req,rem)).
        // guest_bad_ptr (not host_range_mapped) so a PROT_NONE guard page (LTP tst_get_bad_addr) faults too.
        if (guest_bad_ptr((uintptr_t)a2, sizeof(struct timespec))) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        // timespec range validity -> EINVAL (tv_nsec out of [0,1e9) or negative tv_sec). LTP passes
        // tv_nsec=-1 and tv_nsec=1000000000; macOS nanosleep is lax about the upper bound, so check here.
        if ((unsigned long)req->tv_nsec >= 1000000000ul || req->tv_sec < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        if (flags & 1) { // TIMER_ABSTIME
            clockid_t mc;
            switch ((int)a0) {
            case 2: mc = CLOCK_PROCESS_CPUTIME_ID; break;
            case 3: mc = CLOCK_THREAD_CPUTIME_ID; break;
            case 0:
            case 5: mc = CLOCK_REALTIME; break;
            default: mc = CLOCK_MONOTONIC; break; // 1/4/6/7 -> monotonic
            }
            struct timespec now, d;
            int abs_intr = 0;
            // Loop on EINTR re-reading `now` each pass so a signal can't make the guest under-sleep:
            // recompute the remaining (deadline - now) against the ABSOLUTE deadline, not nanosleep's
            // own relative remainder, so accumulated scheduling slop never shortens the sleep.
            for (;;) {
                clock_gettime(mc, &now);
                d.tv_sec = req->tv_sec - now.tv_sec;
                d.tv_nsec = req->tv_nsec - now.tv_nsec;
                if (d.tv_nsec < 0) {
                    d.tv_sec--;
                    d.tv_nsec += 1000000000L;
                }
                if (d.tv_sec < 0 || (d.tv_sec == 0 && d.tv_nsec <= 0)) break; // deadline passed
                if (nanosleep(&d, NULL) == 0) break;
                if (errno != EINTR) break; // genuine host error -> stop
                if (__atomic_load_n(&c->exited, __ATOMIC_SEQ_CST)) break; // execve teardown: stop re-sleeping
                // A deliverable guest signal must surface EINTR so the dispatcher runs the handler (Linux
                // clock_nanosleep returns EINTR here); only a spurious/internal wakeup re-sleeps the true
                // remainder. The old code always re-slept, swallowing the interrupt and sleeping the full
                // deadline with the handler never run.
                if (svc_poll_retry(c)) continue;
                abs_intr = 1;
                break;
            }
            ts_wait_leave();
            G_RET(c) = abs_intr ? (uint64_t)(int64_t)(-EINTR) : 0; // ABSTIME sleep has no remainder to report
            break;
        }
        // Relative sleep (flags==0): back it with the host nanosleep and PROPAGATE its result. macOS
        // nanosleep rejects an out-of-range/negative timespec with EINVAL and returns EINTR on a delivered
        // signal (writing the unslept remainder into `rem`), matching Linux clock_nanosleep -- the old code
        // swallowed all of that and always returned 0. Retry only on a spurious/internal EINTR (nothing
        // deliverable to the guest); surface a real EINTR so the dispatcher runs the pending handler. (LTP
        // nanosleep02: an out-of-range tv_nsec / negative tv_sec is EINVAL, not a silent success.)
        if (a3 && !host_range_mapped((uintptr_t)a3, sizeof(struct timespec))) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        if (req->tv_sec < 0 || req->tv_nsec < 0 || req->tv_nsec >= 1000000000L) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL); // out-of-range/negative request (LTP nanosleep02)
            break;
        }
        // Relative sleep against a FIXED monotonic deadline (glibc's nanosleep() arrives here). Two host
        // realities are corrected: (1) dd's internal block-preemption interrupts the sleep with a
        // signal invisible to the guest -- re-sleep only the TRUE remainder, never restart the duration
        // (the old `nanosleep(req,rem)` retry restarted the full sleep when rem was NULL, +1 period each
        // preemption); (2) macOS coalesces timer wakeups by ~1-2.5ms, blowing LTP nanosleep01's 450us
        // threshold, so a scoped real-time policy makes the wakeup precise. (LTP nanosleep01/nanosleep02.)
        struct timespec now, deadline;
        clock_gettime(CLOCK_MONOTONIC, &deadline);
        deadline.tv_sec += req->tv_sec;
        deadline.tv_nsec += req->tv_nsec;
        if (deadline.tv_nsec >= 1000000000L) {
            deadline.tv_sec++;
            deadline.tv_nsec -= 1000000000L;
        }
        sleep_precise_begin();
        int rr = 0;
        for (;;) {
            struct timespec d;
            clock_gettime(CLOCK_MONOTONIC, &now);
            d.tv_sec = deadline.tv_sec - now.tv_sec;
            d.tv_nsec = deadline.tv_nsec - now.tv_nsec;
            if (d.tv_nsec < 0) {
                d.tv_sec--;
                d.tv_nsec += 1000000000L;
            }
            if (d.tv_sec < 0 || (d.tv_sec == 0 && d.tv_nsec <= 0)) {
                rr = 0;
                break;
            } // deadline reached
            rr = nanosleep(&d, NULL);
            if (rr == 0) break;
            if (svc_poll_retry(c)) continue; // internal/spurious wakeup -> re-sleep the true remainder
            if (a3) {                        // deliverable guest signal: report the remaining time in rem
                struct timespec rem;
                clock_gettime(CLOCK_MONOTONIC, &now);
                rem.tv_sec = deadline.tv_sec - now.tv_sec;
                rem.tv_nsec = deadline.tv_nsec - now.tv_nsec;
                if (rem.tv_nsec < 0) {
                    rem.tv_sec--;
                    rem.tv_nsec += 1000000000L;
                }
                if (rem.tv_sec < 0) {
                    rem.tv_sec = 0;
                    rem.tv_nsec = 0;
                }
                *(struct timespec *)a3 = rem;
            }
            break;
        }
        sleep_precise_end();
        G_RET(c) = rr < 0 ? (uint64_t)(int64_t)(-errno) : 0;
        break;
    }
    // times(struct tms*): real CPU accounting. The Linux + macOS struct tms layouts match (4 clock_t
    // fields), and both count in sysconf(_SC_CLK_TCK) ticks, so the host result drops straight in.
    case 153: {
        struct tms tb;
        clock_t r = times(&tb);
        if (a0) {
            if (!host_range_mapped((uintptr_t)a0, sizeof(struct tms))) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            *(struct tms *)a0 = tb;
        }
        G_RET(c) = (uint64_t)r;
        break;
    }
    case 169: {
        // gettimeofday(tv, tz). LTP gettimeofday01 exercises the RAW syscall (not the vDSO fast path) with
        // a bad `tv` AND/OR a bad `tz`: EITHER unmapped pointer must return -EFAULT, matching the kernel,
        // which copy_to_user()s tv first and then tz. The old handler validated only `tv` and silently
        // ignored `tz`, so a valid-tv/bad-tz call wrongly succeeded. Validate both (tz is obsolete but the
        // kernel still writes the 8-byte struct timezone when the pointer is non-NULL).
        struct timeval tv;
        gettimeofday(&tv, 0);
        uint64_t *g = (uint64_t *)a0;
        if (g) {
            if (!host_range_mapped((uintptr_t)a0, 16)) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            g[0] = tv.tv_sec;
            g[1] = tv.tv_usec;
        }
        if (a1) { // struct timezone (deprecated). Validate for EFAULT like the kernel's copy_to_user, but do
                  // NOT write it: modern Linux fills it with zeros and no caller reads it, while writing a
                  // caller-supplied-but-read-only tz page would fault the engine (host_range_mapped only
                  // read-probes). Validation alone satisfies LTP gettimeofday01's bad-tz EFAULT case.
            if (!host_range_mapped((uintptr_t)a1, 8)) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
        }
        G_RET(c) = 0;
        break;
    }
    // ===================== POSIX per-process timers (aarch64 nrs; x86 normalized to these) ==========
    // 107 timer_create(clockid, sigevent*, timer_t*) -- allocate a slot, record clock + sigevent.
    case 107: {
        pthread_mutex_lock(&g_gtimer_lk);
        int rc = gtimer_init();
        if (rc < 0) {
            pthread_mutex_unlock(&g_gtimer_lk);
            G_RET(c) = (uint64_t)rc;
            break;
        }
        int id = -1;
        for (int i = 0; i < GTIMER_MAX; i++)
            if (!g_gtimer[i].used) {
                id = i;
                break;
            }
        if (id < 0) {
            pthread_mutex_unlock(&g_gtimer_lk);
            G_RET(c) = (uint64_t)(-EAGAIN);
            break;
        }
        struct gtimer *t = &g_gtimer[id];
        memset(t, 0, sizeof *t);
        t->used = 1;
        t->clockid = (int)a0;
        // an unknown/negative clockid is -EINVAL on Linux (no such POSIX clock). The mappable set is
        // REALTIME(0)..MONOTONIC_RAW(4), the COARSE pair (5/6), BOOTTIME(7), the ALARM clocks (8/9) and
        // TAI(11); anything outside [0,11] is rejected. (LTP timer_create: bad clockid case.)
        if ((int)a0 < 0 || (int)a0 > 11) {
            t->used = 0;
            pthread_mutex_unlock(&g_gtimer_lk);
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // EFAULT (after the clockid EINVAL, matching the kernel: clockid_to_kclock before copy_from_user):
        // a non-NULL sigevent must be a readable 16-byte struct (the engine memcpys it below), and the
        // timerid out-pointer must be writable -- a bad address is -EFAULT, not an engine fault (LTP
        // timer_create02: bad sigevent / bad timer-id addr via tst_get_bad_addr). NULL sigevent is the POSIX
        // default (checked separately below); NULL timerid, however, has nowhere to store the id -> EFAULT.
        if (a1 && guest_bad_ptr((uintptr_t)a1, 16)) {
            t->used = 0;
            pthread_mutex_unlock(&g_gtimer_lk);
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        if (guest_bad_ptr((uintptr_t)a2, 4)) {
            t->used = 0;
            pthread_mutex_unlock(&g_gtimer_lk);
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        if (a1) {
            // struct sigevent: sigev_value [0..8), sigev_signo [8..12), sigev_notify [12..16)
            uint64_t sigval;
            int signo, notify;
            memcpy(&sigval, (void *)a1, 8);
            memcpy(&signo, (void *)(a1 + 8), 4);
            memcpy(&notify, (void *)(a1 + 12), 4);
            t->sigval = sigval;
            t->signo = signo;
            t->notify = notify;
        } else {
            // POSIX default: SIGEV_SIGNAL, SIGALRM(14), si_value = timer id
            t->notify = 0;
            t->signo = 14;
            t->sigval = (uint64_t)id;
        }
        // / CVE-2017-18344: the kernel accepts ONLY these sigev_notify values -- SIGEV_SIGNAL(0),
        // SIGEV_NONE(1), SIGEV_THREAD(2) and SIGEV_SIGNAL|SIGEV_THREAD_ID(4). Any other value (e.g. the
        // test's SIGEV_SIGNAL|54321) is -EINVAL; before the fix this field went unverified and let a caller
        // read arbitrary kernel memory. For a signal-bearing notify the signo must be a real signal (1..64,
        // SIGRTMAX); SIGEV_NONE carries no signal so its signo is not checked. (LTP timer_create03.)
        if (t->notify != 0 && t->notify != 1 && t->notify != 2 && t->notify != 4) {
            t->used = 0;
            pthread_mutex_unlock(&g_gtimer_lk);
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        if (t->notify != 1 && (t->signo < 1 || t->signo > 64)) {
            t->used = 0;
            pthread_mutex_unlock(&g_gtimer_lk);
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // SIGEV_THREAD(2) needs to run a guest callback in a fresh guest thread -- not expressible
        // from the host syscall layer. glibc lowers SIGEV_THREAD to SIGEV_THREAD_ID(4)+a real-time
        // signal BEFORE the syscall, so we normally never see raw 2; refuse it honestly rather than
        // accept a timer that would silently never fire.
        if (t->notify == 2) {
            t->used = 0;
            pthread_mutex_unlock(&g_gtimer_lk);
            G_RET(c) = (uint64_t)(-ENOSYS);
            break;
        }
        pthread_mutex_unlock(&g_gtimer_lk);
        if (a2) memcpy((void *)a2, &id, 4); // kernel writes the int timer id back
        G_RET(c) = 0;
        break;
    }
    // 110 timer_settime(timerid, flags, new*, old*) -- arm/disarm via the itimerspec.
    case 110: {
        int id = (int)a0;
        if (id < 0 || id >= GTIMER_MAX) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        pthread_mutex_lock(&g_gtimer_lk);
        struct gtimer *t = &g_gtimer[id];
        if (!t->used) {
            pthread_mutex_unlock(&g_gtimer_lk);
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // timer_settime(2) error surface, matching the kernel + LTP timer_settime02:
        //   NULL new_value        -> EINVAL (the kernel's `if (!new_setting) return -EINVAL`)
        //   bad (non-NULL) new    -> EFAULT (copy_from_user)
        //   bad (non-NULL) old    -> EFAULT (copy_to_user of the previous setting)
        //   tv_nsec out of [0,1e9) or negative tv_sec -> EINVAL (itimerspec64_valid)
        // Validate BEFORE reporting old / re-arming so a bad pointer never faults the engine or half-applies.
        if (a2 == 0) {
            pthread_mutex_unlock(&g_gtimer_lk);
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        if (guest_bad_ptr((uintptr_t)a2, 32)) {
            pthread_mutex_unlock(&g_gtimer_lk);
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        if (a3 && guest_bad_ptr((uintptr_t)a3, 32)) {
            pthread_mutex_unlock(&g_gtimer_lk);
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        // itimerspec: it_interval [0..16), it_value [16..32)
        uint64_t ivs = 0, ivn = 0, vls = 0, vln = 0;
        memcpy(&ivs, (void *)a2, 8);
        memcpy(&ivn, (void *)(a2 + 8), 8);
        memcpy(&vls, (void *)(a2 + 16), 8);
        memcpy(&vln, (void *)(a2 + 24), 8);
        if (ivn >= 1000000000ull || vln >= 1000000000ull || (int64_t)ivs < 0 || (int64_t)vls < 0) {
            pthread_mutex_unlock(&g_gtimer_lk);
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        if (a3) gtimer_fill_curr(t, (void *)a3); // report the current setting before re-arming
        if (vls == 0 && vln == 0) {              // it_value all-zero => disarm (regardless of it_interval)
            gtimer_disarm(id);
            t->interval_ns = 0;
            pthread_mutex_unlock(&g_gtimer_lk);
            G_RET(c) = 0;
            break;
        }
        uint64_t interval_ns = ivs * 1000000000ull + ivn;
        uint64_t value_ns;
        // Track the SIGNED offset to the first expiry too: a TIMER_ABSTIME deadline already in the past has a
        // negative offset, which the kqueue arm clamps to 0 (fire asap) but the overrun count must NOT clamp
        // -- a periodic timer whose start is far in the past has already "overrun" (now-deadline)/interval
        // times by its first delivery (LTP timer_settime03: overrun capped at INT_MAX). first_ns preserves
        // that true (possibly-past) absolute deadline so overrun counts every elapsed period from it.
        int64_t offset_ns;
        if (a1 & 1) { // TIMER_ABSTIME: it_value is an absolute deadline in the timer's clock
            struct timespec cn;
            clock_gettime(gtimer_hostclock(t->clockid), &cn);
            uint64_t cnow = (uint64_t)cn.tv_sec * 1000000000ull + (uint64_t)cn.tv_nsec;
            uint64_t deadline = vls * 1000000000ull + vln;
            offset_ns = (int64_t)deadline - (int64_t)cnow;      // may be negative (deadline already passed)
            value_ns = offset_ns > 0 ? (uint64_t)offset_ns : 0; // past deadline -> fire asap
        } else {
            value_ns = vls * 1000000000ull + vln;
            offset_ns = (int64_t)value_ns;
        }
        t->interval_ns = interval_ns;
        uint64_t mono = gtimer_now_ns();
        t->next_ns = mono + value_ns;
        t->first_ns = (uint64_t)((int64_t)mono + offset_ns); // fixed expiry-#0 anchor (may be < mono for ABSTIME)
        t->reported_expiries = 0;                            // a fresh arming resets the overrun accounting
        int rc = gtimer_arm(id, value_ns, interval_ns);
        pthread_mutex_unlock(&g_gtimer_lk);
        G_RET(c) = rc < 0 ? (uint64_t)rc : 0;
        break;
    }
    // 108 timer_gettime(timerid, curr*) -- remaining time + interval.
    case 108: {
        int id = (int)a0;
        if (id < 0 || id >= GTIMER_MAX) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        pthread_mutex_lock(&g_gtimer_lk);
        struct gtimer *t = &g_gtimer[id];
        if (!t->used) {
            pthread_mutex_unlock(&g_gtimer_lk);
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // The kernel copy_to_user()s the itimerspec, so a NULL/bad curr pointer is -EFAULT (checked AFTER
        // the timerid EINVAL, matching LTP timer_gettime01: gettime(-1)->EINVAL, gettime(valid,NULL)->EFAULT).
        // guest_bad_ptr catches NULL and a PROT_NONE guard page; gtimer_fill_curr writes 32 bytes.
        if (guest_bad_ptr((uintptr_t)a1, 32)) {
            pthread_mutex_unlock(&g_gtimer_lk);
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        gtimer_fill_curr(t, (void *)a1);
        pthread_mutex_unlock(&g_gtimer_lk);
        G_RET(c) = 0;
        break;
    }
    // 109 timer_getoverrun(timerid) -- overrun count of the most recently delivered expiry. Computed from
    // ELAPSED MONOTONIC TIME (gtimer_take_overrun), so a signal-blocked periodic timer whose expirations
    // elapsed while the guest slept still reports the true count -- it does NOT depend on the guest executing
    // translated blocks or on the drain thread having processed every fire. This call is Linux's delivery
    // point for the pending signal the guest just dequeued: it reports the extras piled up since the last
    // delivery and advances the reported mark past them (capped at INT_MAX; one-shot / unexpired => 0).
    case 109: {
        int id = (int)a0;
        if (id < 0 || id >= GTIMER_MAX) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        pthread_mutex_lock(&g_gtimer_lk);
        struct gtimer *t = &g_gtimer[id];
        if (!t->used) {
            pthread_mutex_unlock(&g_gtimer_lk);
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        int ov = gtimer_take_overrun(t, gtimer_now_ns());
        pthread_mutex_unlock(&g_gtimer_lk);
        G_RET(c) = (uint64_t)(uint32_t)ov;
        break;
    }
    // 111 timer_delete(timerid) -- disarm + free the slot.
    case 111: {
        int id = (int)a0;
        if (id < 0 || id >= GTIMER_MAX) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        pthread_mutex_lock(&g_gtimer_lk);
        struct gtimer *t = &g_gtimer[id];
        if (!t->used) {
            pthread_mutex_unlock(&g_gtimer_lk);
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        gtimer_disarm(id);
        t->used = 0;
        pthread_mutex_unlock(&g_gtimer_lk);
        G_RET(c) = 0;
        break;
    }
    default: return 0;
    }
    return 1;
}
