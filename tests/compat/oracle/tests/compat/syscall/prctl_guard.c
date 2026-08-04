// Permanent prctl(2) guard for the hl matrix. Mirrors the option/argument validation matrix the LTP
// binaries prctl02 (the EINVAL/EFAULT/EPERM cases) and prctl03 (PR_SET/GET_CHILD_SUBREAPER round-trip)
// assert, driven straight at the syscall so it is engine-independent. SELF-CHECKING: every assertion
// compares the observed return/errno against the real-Linux contract and the program exits 0 and prints
// PRCTL_GUARD_OK only if all pass (so it is NOT diffed against the oracle -- qemu-user lacks THP_DISABLE
// and some speculation state, and the initial THP/dumpable flags are host-specific; the point is that hl's
// prctl matches the Linux *contract*, which the native run confirms by also printing PRCTL_GUARD_OK).
// Owner: prctl guard.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <errno.h>
#include <unistd.h>
#include <stdint.h>
#include <sys/prctl.h>
#include <sys/mman.h>
#include <sys/syscall.h>

#define CAP_SETPCAP 8

static int fails;
#define CK(name, cond) do { if (!(cond)) { printf("FAIL %s\n", name); fails++; } } while (0)

/* prctl option numbers (avoid depending on libc header coverage) */
enum {
    P_SET_PDEATHSIG = 1, P_GET_PDEATHSIG = 2, P_GET_DUMPABLE = 3, P_SET_DUMPABLE = 4,
    P_GET_KEEPCAPS = 7, P_SET_KEEPCAPS = 8, P_SET_TIMING = 14, P_SET_NAME = 15, P_GET_NAME = 16,
    P_CAPBSET_DROP = 24, P_SET_SECUREBITS = 28, P_SET_CHILD_SUBREAPER = 36, P_GET_CHILD_SUBREAPER = 37,
    P_SET_NO_NEW_PRIVS = 38, P_GET_NO_NEW_PRIVS = 39, P_SET_THP_DISABLE = 41, P_GET_THP_DISABLE = 42,
    P_CAP_AMBIENT = 47, P_GET_SPECULATION_CTRL = 52,
};
enum { AMB_IS_SET = 1, AMB_RAISE = 2, AMB_LOWER = 3, AMB_CLEAR_ALL = 4 };

static long pr(int opt, unsigned long a2, unsigned long a3, unsigned long a4, unsigned long a5) {
    errno = 0;
    return syscall(SYS_prctl, opt, a2, a3, a4, a5);
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    void *BAD = mmap(NULL, 4096, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);

    /* PR_SET_NAME / PR_GET_NAME round-trip + EFAULT */
    char nm[32] = {0};
    CK("set_name", pr(P_SET_NAME, (unsigned long)"prctlguard", 0, 0, 0) == 0);
    CK("get_name", pr(P_GET_NAME, (unsigned long)nm, 0, 0, 0) == 0 && !strcmp(nm, "prctlguard"));
    CK("set_name_efault", pr(P_SET_NAME, (unsigned long)BAD, 0, 0, 0) == -1 && errno == EFAULT);

    /* invalid option -> EINVAL */
    CK("opt_invalid", pr(999, 1, 0, 0, 0) == -1 && errno == EINVAL);

    /* PR_SET_PDEATHSIG: invalid signal -> EINVAL; valid -> 0; GET round-trips */
    CK("pdeathsig_bad", pr(P_SET_PDEATHSIG, (unsigned long)-1, 0, 0, 0) == -1 && errno == EINVAL);
    CK("pdeathsig_ok", pr(P_SET_PDEATHSIG, 9, 0, 0, 0) == 0);
    { int v = -1; CK("pdeathsig_get", pr(P_GET_PDEATHSIG, (unsigned long)&v, 0, 0, 0) == 0 && v == 9); }

    /* PR_SET_DUMPABLE: 2 is invalid; 0/1 round-trip via GET */
    CK("dumpable_2_einval", pr(P_SET_DUMPABLE, 2, 0, 0, 0) == -1 && errno == EINVAL);
    CK("dumpable_set0", pr(P_SET_DUMPABLE, 0, 0, 0, 0) == 0);
    CK("dumpable_get0", pr(P_GET_DUMPABLE, 0, 0, 0, 0) == 0);
    CK("dumpable_set1", pr(P_SET_DUMPABLE, 1, 0, 0, 0) == 0);
    CK("dumpable_get1", pr(P_GET_DUMPABLE, 0, 0, 0, 0) == 1);

    /* PR_SET/GET_KEEPCAPS round-trip */
    CK("keepcaps_set", pr(P_SET_KEEPCAPS, 1, 0, 0, 0) == 0);
    CK("keepcaps_get", pr(P_GET_KEEPCAPS, 0, 0, 0, 0) == 1);
    CK("keepcaps_clr", pr(P_SET_KEEPCAPS, 0, 0, 0, 0) == 0);
    CK("keepcaps_get0", pr(P_GET_KEEPCAPS, 0, 0, 0, 0) == 0);

    /* PR_SET_NO_NEW_PRIVS: arg2!=1 -> EINVAL; arg3 nonzero -> EINVAL. GET: any nonzero arg -> EINVAL */
    CK("nnp_set_arg0", pr(P_SET_NO_NEW_PRIVS, 0, 0, 0, 0) == -1 && errno == EINVAL);
    CK("nnp_set_arg3", pr(P_SET_NO_NEW_PRIVS, 1, 1, 0, 0) == -1 && errno == EINVAL);
    CK("nnp_get_arg2", pr(P_GET_NO_NEW_PRIVS, 1, 0, 0, 0) == -1 && errno == EINVAL);
    CK("nnp_get_ok", pr(P_GET_NO_NEW_PRIVS, 0, 0, 0, 0) >= 0);

    /* PR_SET_TIMING: only PR_TIMING_STATISTICAL(0) is allowed; arg2=1 -> EINVAL */
    CK("timing_einval", pr(P_SET_TIMING, 1, 0, 0, 0) == -1 && errno == EINVAL);

    /* PR_{SET,GET}_THP_DISABLE: probe must not be EINVAL; nonzero unused args -> EINVAL; round-trip */
    CK("thp_get_ok", pr(P_GET_THP_DISABLE, 0, 0, 0, 0) >= 0);
    CK("thp_get_arg2", pr(P_GET_THP_DISABLE, 1, 1, 0, 0) == -1 && errno == EINVAL);
    CK("thp_set_arg3", pr(P_SET_THP_DISABLE, 0, 1, 0, 0) == -1 && errno == EINVAL);
    CK("thp_set1", pr(P_SET_THP_DISABLE, 1, 0, 0, 0) == 0);
    CK("thp_get1", pr(P_GET_THP_DISABLE, 0, 0, 0, 0) == 1);
    CK("thp_set0", pr(P_SET_THP_DISABLE, 0, 0, 0, 0) == 0);
    CK("thp_get0", pr(P_GET_THP_DISABLE, 0, 0, 0, 0) == 0);

    /* PR_CAP_AMBIENT: CLEAR_ALL ok; bad sub-command -> EINVAL; CLEAR_ALL+arg3 -> EINVAL; IS_SET+badcap -> EINVAL */
    CK("capamb_clearall", pr(P_CAP_AMBIENT, AMB_CLEAR_ALL, 0, 0, 0) == 0);
    CK("capamb_badcmd", pr(P_CAP_AMBIENT, (unsigned long)-1, 0, 0, 0) == -1 && errno == EINVAL);
    CK("capamb_clr_arg3", pr(P_CAP_AMBIENT, AMB_CLEAR_ALL, 1, 0, 0) == -1 && errno == EINVAL);
    CK("capamb_isset_badcap", pr(P_CAP_AMBIENT, AMB_IS_SET, (unsigned long)-1, 0, 0) == -1 && errno == EINVAL);
    CK("capamb_isset_ok", pr(P_CAP_AMBIENT, AMB_IS_SET, 0 /*CAP_CHOWN*/, 0, 0) == 0);

    /* PR_GET_SPECULATION_CTRL: probe not EINVAL; nonzero arg3 -> EINVAL */
    CK("spec_get_ok", pr(P_GET_SPECULATION_CTRL, 0, 0, 0, 0) >= 0);
    CK("spec_get_arg3", pr(P_GET_SPECULATION_CTRL, 0, (unsigned long)-1, 0, 0) == -1 && errno == EINVAL);

    /* PR_SET/GET_CHILD_SUBREAPER round-trip (prctl03) */
    CK("subreaper_set1", pr(P_SET_CHILD_SUBREAPER, 1, 0, 0, 0) == 0);
    { int v = -1; CK("subreaper_get1", pr(P_GET_CHILD_SUBREAPER, (unsigned long)&v, 0, 0, 0) == 0 && v == 1); }
    CK("subreaper_set0", pr(P_SET_CHILD_SUBREAPER, 0, 0, 0, 0) == 0);
    { int v = -1; CK("subreaper_get0", pr(P_GET_CHILD_SUBREAPER, (unsigned long)&v, 0, 0, 0) == 0 && v == 0); }

    /* Capability-gated options: after dropping CAP_SETPCAP, PR_SET_SECUREBITS and PR_CAPBSET_DROP -> EPERM.
       (The container starts as full root; the native oracle shell has no caps -- either way the drop leaves
       CAP_SETPCAP clear and both options must EPERM, matching LTP prctl02's CAP_SETPCAP-drop subtests.) */
    {
        struct { uint32_t version; int pid; } hdr = {0x20080522u, 0};
        struct { uint32_t eff, perm, inh; } data[2] = {{0}};
        syscall(SYS_capget, &hdr, data);
        data[0].eff &= ~(1u << CAP_SETPCAP); /* CAP_SETPCAP = 8, index 0 */
        syscall(SYS_capset, &hdr, data);     /* ignore result: no-op if already unset */
        CK("securebits_eperm", pr(P_SET_SECUREBITS, 0, 0, 0, 0) == -1 && errno == EPERM);
        CK("capbset_drop_eperm", pr(P_CAPBSET_DROP, 1, 0, 0, 0) == -1 && errno == EPERM);
    }

    if (fails == 0) printf("PRCTL_GUARD_OK\n");
    else printf("PRCTL_GUARD_FAILED %d\n", fails);
    return fails ? 1 : 0;
}
