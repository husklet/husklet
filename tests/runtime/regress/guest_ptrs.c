// Non-PIE guest-pointer surfaces the per-syscall rebase allowlist did not name.
//
// A non-PIE ET_EXEC is mapped at +bias while every address baked into it -- every `&static_object` below --
// keeps its LOW link vaddr. dispatch.c rebases pointer arguments from a hand-maintained per-syscall,
// per-argument table, so anything missing from that table reached the engine as an unmapped low address:
// silently wrong (the robust list was skipped whole), -EFAULT on a valid pointer, or a SIGSEGV of the
// engine itself. This walks the surfaces that table does not cover -- thread-id slots, the robust list, the
// futex word, epoll/timer/ioctl/prctl structs, the auxv the loader planted, and the image's own protection
// -- with EVERY pointer a static. nonpie_ptrargs.c (compat/process) is the fs/ipc/signal half of the same
// audit; this is the thread, memory and discovery half.
//
// Every verdict is environment-independent -- "the valid low pointer was not rejected and the value that
// came back is the one Linux produces" -- so native and both engines print the same line. A regression
// flips a token, or loses the run entirely.
#define _GNU_SOURCE
#include <elf.h>
#include <errno.h>
#include <fcntl.h>
#include <linux/futex.h>
#include <setjmp.h>
#include <pthread.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/epoll.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <sys/timerfd.h>
#include <unistd.h>

#define TOK(name, cond) printf(" %s=%s", (name), (cond) ? "ok" : "BAD")

// ---- every pointer handed to the kernel below is the address of one of these ----
struct robust_head {
    void *list;
    long futex_offset;
    void *list_op_pending;
};
static struct robust_head g_robust;
static int g_tidslot;
static int g_futexword;
static struct epoll_event g_ev, g_out[4];
static struct itimerspec g_its, g_itsq;
static struct winsize g_ws;
static char g_name[32];
static struct timespec g_ts;
static unsigned long g_auxrnd;
static volatile int g_faulted;
static sigjmp_buf g_jb;

// A page of the image's own writable data: the RELRO shape. mprotect names it by the LOW link vaddr while
// the bytes live at +bias, which is exactly where the protection registries disagreed.
static long g_page[8192] __attribute__((aligned(4096)));

// A .rodata address: read-only from the moment the ELF loader applied the program headers, which is the
// half of the read-only registry the LOADER writes (the guest's mprotect writes the other half).
static const char g_rodata[64] = "hl nonpie rodata";

static volatile int g_peer_ran;

// The peer registers its own robust list at a static head too, so the thread-exit walk runs against an
// in-image head on a thread that really terminates.
static void *peer(void *unused) {
    (void)unused;
    static struct robust_head peer_head;
    peer_head.list = &peer_head;
    (void)syscall(SYS_set_robust_list, &peer_head, sizeof peer_head);
    g_peer_ran = 1;
    return NULL;
}

static volatile void *g_si_addr;

static void segv(int s, siginfo_t *si, void *uc) {
    (void)s;
    (void)uc;
    g_faulted = 1;
    g_si_addr = si->si_addr;
    siglongjmp(g_jb, 1);
}

// read(2) into `dst` must be rejected with EFAULT, not silently accepted, because the destination is
// read-only. /dev/zero always has a byte ready, so nothing but the buffer check can decide this.
static int read_efaults(void *dst) {
    int fd = open("/dev/zero", O_RDONLY);
    if (fd < 0) return 0;
    ssize_t n = read(fd, dst, 8);
    int ok = n < 0 && errno == EFAULT;
    close(fd);
    return ok;
}

// Does /proc/self/maps report `address` as writable? Reads the file the way pmap/libunwind/jemalloc do.
// -1 when no row covers it, which is itself a failure for an address the guest just used.
static int maps_writable(unsigned long address) {
    FILE *f = fopen("/proc/self/maps", "r");
    if (!f) return -1;
    char line[512];
    int verdict = -1;
    while (fgets(line, sizeof line, f)) {
        unsigned long lo, hi;
        char perms[8];
        if (sscanf(line, "%lx-%lx %4s", &lo, &hi, perms) != 3) continue;
        if (address >= lo && address < hi) {
            verdict = perms[1] == 'w';
            break;
        }
    }
    fclose(f);
    return verdict;
}

int main(void) {
    long page = sysconf(_SC_PAGESIZE);

    // -- thread-id slots and the robust list: stored raw by the engine, dereferenced much later --
    long tidrc = syscall(SYS_set_tid_address, &g_tidslot);
    TOK("set_tid_address", tidrc == (long)syscall(SYS_gettid));
    g_robust.list = &g_robust; // empty list: `list` points back at the head
    g_robust.futex_offset = 0;
    g_robust.list_op_pending = NULL;
    long setrl = syscall(SYS_set_robust_list, &g_robust, sizeof g_robust);
    TOK("set_robust_list", setrl == 0);
    {
        // get_robust_list must hand back the pointer the guest gave, in the guest's own coordinates --
        // returning the +bias storage address would leak the loader's fold into the guest.
        void *head = NULL;
        size_t len = 0;
        long getrl = syscall(SYS_get_robust_list, 0, &head, &len);
        TOK("get_robust_list", getrl != 0 ? errno != EFAULT : (head == (void *)&g_robust && len == sizeof g_robust));
    }

    // -- futex on a static word: uaddr is the hash key AND is dereferenced --
    g_futexword = 0;
    TOK("futex_wake", syscall(SYS_futex, &g_futexword, FUTEX_WAKE_PRIVATE, 1) == 0);
    g_futexword = 1;
    g_ts.tv_sec = 0;
    g_ts.tv_nsec = 1000000;
    // val != *uaddr, so the kernel returns EAGAIN without sleeping; a low uaddr it could not read
    // would come back EFAULT instead.
    TOK("futex_wait", syscall(SYS_futex, &g_futexword, FUTEX_WAIT_PRIVATE, 0, &g_ts) == -1 && errno == EAGAIN);

    // -- epoll: the event struct in and the event array out --
    {
        int ep = epoll_create1(0);
        int pfd[2];
        int piped = pipe(pfd) == 0;
        g_ev.events = EPOLLIN;
        g_ev.data.u64 = 0x5eed;
        int ctl = piped && epoll_ctl(ep, EPOLL_CTL_ADD, pfd[0], &g_ev) == 0;
        TOK("epoll_ctl", ctl);
        if (piped) {
            char b = 'x';
            ssize_t w = write(pfd[1], &b, 1);
            (void)w;
        }
        int n = ctl ? epoll_wait(ep, g_out, 4, 1000) : -1;
        TOK("epoll_wait", n == 1 && g_out[0].data.u64 == 0x5eed);
        if (piped) {
            close(pfd[0]);
            close(pfd[1]);
        }
        close(ep);
    }

    // -- timerfd: itimerspec in and out --
    {
        int tf = timerfd_create(CLOCK_MONOTONIC, 0);
        g_its.it_value.tv_sec = 100;
        g_its.it_interval.tv_sec = 0;
        int set = tf >= 0 && timerfd_settime(tf, 0, &g_its, NULL) == 0;
        int got = set && timerfd_gettime(tf, &g_itsq) == 0 && g_itsq.it_value.tv_sec > 0;
        TOK("timerfd", got);
        if (tf >= 0) close(tf);
    }

    // -- ioctl: the third argument is a struct pointer the allowlist never names --
    {
        int rc = ioctl(STDIN_FILENO, TIOCGWINSZ, &g_ws);
        TOK("ioctl_winsize", rc == 0 || errno != EFAULT); // ENOTTY under a pipe is the honest answer
    }

    // -- prctl: PR_GET_NAME writes 16 bytes through a static pointer --
    memset(g_name, 0, sizeof g_name);
    TOK("prctl_getname", prctl(PR_SET_NAME, "hlnonpie") == 0 && prctl(PR_GET_NAME, g_name) == 0 &&
                             !strcmp(g_name, "hlnonpie"));

    // -- the auxv the loader planted: AT_PHDR must be a pointer the GUEST can dereference, and the table
    //    it names must describe this image (a PT_LOAD covering main's own address). Bugs three and four
    //    of the family were both a consumer dereferencing AT_PHDR without folding it. --
    {
        unsigned long at_phdr = getauxval(AT_PHDR), at_phnum = getauxval(AT_PHNUM);
        unsigned long at_phent = getauxval(AT_PHENT), at_entry = getauxval(AT_ENTRY);
        unsigned long here = (unsigned long)(uintptr_t)&g_page[0];
        int covers = 0, loads = 0, entry_in_text = 0;
        if (at_phdr && at_phent == sizeof(Elf64_Phdr)) {
            for (unsigned long i = 0; i < at_phnum; i++) {
                const Elf64_Phdr *ph = (const Elf64_Phdr *)(at_phdr + i * at_phent);
                if (ph->p_type != PT_LOAD) continue;
                loads++;
                if (here >= ph->p_vaddr && here < ph->p_vaddr + ph->p_memsz) covers = 1;
                if (at_entry >= ph->p_vaddr && at_entry < ph->p_vaddr + ph->p_memsz && (ph->p_flags & PF_X))
                    entry_in_text = 1;
            }
        }
        TOK("at_phdr", loads > 0 && covers);
        TOK("at_entry", entry_in_text);
        g_auxrnd = getauxval(AT_RANDOM);
        // AT_RANDOM is 16 readable bytes; the engine puts them on the guest stack, never in the image.
        unsigned char r[16];
        memcpy(r, (const void *)g_auxrnd, sizeof r);
        int nz = 0;
        for (int i = 0; i < 16; i++) nz |= r[i];
        TOK("at_random", g_auxrnd != 0 && nz != 0);
    }

    // -- personality: no pointer, but it is on the audit list and must round-trip --
    {
        long p = syscall(SYS_personality, 0xffffffffUL);
        TOK("personality", p >= 0);
    }

    // -- rseq: unimplemented, and must say so rather than dereference the registration --
    {
        long r = syscall(334 /*SYS_rseq on x86-64; 293 on aarch64 -- both route to the same handler*/, NULL, 0u, 0u,
                         0u);
        TOK("rseq", r == -1 && errno != EFAULT);
    }

    // -- the image's own protection, by its LOW link vaddr: the two-coordinate-system defect. mprotect
    //    keyed the guest address while the ELF loader keyed the +bias storage, so a page could be
    //    read-only to one registry and writable to the other. --
    {
        struct sigaction sa, old;
        memset(&sa, 0, sizeof sa);
        sa.sa_sigaction = segv;
        sa.sa_flags = SA_SIGINFO;
        sigemptyset(&sa.sa_mask);
        sigaction(SIGSEGV, &sa, &old);
        g_page[0] = 1;
        unsigned long lo = (unsigned long)(uintptr_t)g_page & ~(unsigned long)(page - 1);
        TOK("mprotect_ro", mprotect((void *)lo, (size_t)page, PROT_READ) == 0);
        g_faulted = 0;
        if (sigsetjmp(g_jb, 1) == 0) g_page[0] = 2;
        TOK("ro_store_faults", g_faulted == 1 && g_page[0] == 1);
        // si_addr is guest-visible: the handler compares it with its own pointers. A hardware fault reports
        // the +bias storage address, so an in-image fault used to hand over an address the guest has no
        // name for -- the same half of the rule as the LEA and mov materialisations.
        TOK("segv_si_addr", g_si_addr == (void *)&g_page[0]);
        // The sharp form of the two-coordinate defect: a syscall DESTINATION buffer in a read-only page
        // must be -EFAULT. That answer comes from the read-only registry, which the guest's own mprotect
        // and the ELF loader used to key in different coordinates -- so the same question got two answers
        // depending on which of them last touched the page.
        TOK("read_into_ro", read_efaults((void *)lo));
        TOK("read_into_rodata", read_efaults((void *)(uintptr_t)g_rodata));
        TOK("maps_ro", maps_writable((unsigned long)lo) == 0);
        TOK("maps_rodata", maps_writable((unsigned long)(uintptr_t)g_rodata) == 0);
        TOK("mprotect_rw", mprotect((void *)lo, (size_t)page, PROT_READ | PROT_WRITE) == 0);
        g_faulted = 0;
        if (sigsetjmp(g_jb, 1) == 0) g_page[0] = 3;
        TOK("rw_store_lands", g_faulted == 0 && g_page[0] == 3);
        sigaction(SIGSEGV, &old, NULL);
    }

    // -- a thread's clear-child-tid: pthread_join waits on the word the engine zeroes at thread exit,
    //    which for a guest that set it through set_tid_address is a static in this image. --
    {
        pthread_t t;
        int joined = pthread_create(&t, NULL, peer, NULL) == 0 && pthread_join(t, NULL) == 0;
        TOK("thread_join", joined && g_peer_ran == 1);
    }

    printf("\n");
    return 0;
}
