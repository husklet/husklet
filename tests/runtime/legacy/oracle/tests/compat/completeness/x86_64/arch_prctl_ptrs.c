// The guest pointers the x86-only legacy syscall layer dereferences ITSELF, before the shared per-syscall
// rebase table ever runs: arch_prctl(ARCH_GET_FS/GS)'s destination slot, time()'s tloc, select()'s timeval,
// and the utime/utimes/futimesat times buffers.
//
// Two independent failures per site, and this fixture is built -static (ET_EXEC, non-PIE) so both are live:
//   * BIAS -- a non-PIE keeps every `&static` at its LOW link vaddr while the image is mapped at +bias, so
//     an unfolded store lands on an unmapped low address.
//   * ACCESSIBILITY -- Linux answers EFAULT for a destination the guest cannot write. An unguarded engine
//     dereference instead kills the ENGINE (exit 139) on what is only a guest bug, on EVERY linkage.
// arch_prctl had neither: `*(uint64_t *)rsi = fs_base` with no fold and no guard. select's timeval had
// neither. time/utime/utimes/futimesat had the fold but no guard.
//
// Every verdict is environment-independent, so native and both engine hosts print the same line.
#define _GNU_SOURCE
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/select.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <unistd.h>

#define TOK(name, cond) printf(" %s=%s", (name), (cond) ? "ok" : "BAD")

#define ARCH_SET_GS 0x1001
#define ARCH_SET_FS 0x1002
#define ARCH_GET_FS 0x1003
#define ARCH_GET_GS 0x1004

// Every destination below is one of these: a .bss address, i.e. a low link vaddr in this ET_EXEC.
static uint64_t g_slot;
static int64_t g_tloc;
static struct timeval g_timeout;

static long raw(long nr, long a, long b, long c) {
    long r = syscall(nr, a, b, c);
    return r < 0 ? -errno : r;
}

int main(void) {
    // -- arch_prctl GET_FS/GET_GS into a static: the fold half --
    long fs_before = 0, gs_before = 0;
    g_slot = 0xdeadbeefu;
    int got_fs = raw(SYS_arch_prctl, ARCH_GET_FS, (long)&g_slot, 0) == 0 && g_slot != 0xdeadbeefu;
    fs_before = (long)g_slot;
    g_slot = 0xdeadbeefu;
    int got_gs = raw(SYS_arch_prctl, ARCH_GET_GS, (long)&g_slot, 0) == 0;
    gs_before = (long)g_slot;
    printf("arch_prctl");
    TOK("get_fs_static", got_fs);
    TOK("get_gs_static", got_gs && g_slot != 0xdeadbeefu);
    // A SET followed by a GET must round-trip through the same static slot.
    g_slot = 0;
    int roundtrip = raw(SYS_arch_prctl, ARCH_SET_GS, 0x1234000, 0) == 0 &&
                    raw(SYS_arch_prctl, ARCH_GET_GS, (long)&g_slot, 0) == 0 && g_slot == 0x1234000u;
    TOK("set_get_gs", roundtrip);
    (void)raw(SYS_arch_prctl, ARCH_SET_GS, gs_before, 0); // put it back before anything else runs
    (void)fs_before;

    // -- the accessibility half: EFAULT, never an engine death. NULL is EFAULT too. --
    TOK("get_fs_minus1", raw(SYS_arch_prctl, ARCH_GET_FS, -1L, 0) == -EFAULT);
    TOK("get_fs_null", raw(SYS_arch_prctl, ARCH_GET_FS, 0L, 0) == -EFAULT);
    TOK("get_gs_minus1", raw(SYS_arch_prctl, ARCH_GET_GS, -1L, 0) == -EFAULT);
    TOK("get_fs_unmapped", raw(SYS_arch_prctl, ARCH_GET_FS, 0x0000700000000000L, 0) == -EFAULT);
    TOK("bogus_code", raw(SYS_arch_prctl, 0x9999, (long)&g_slot, 0) == -EINVAL);
    printf("\n");

    // -- time(tloc): the engine writes the static itself, so a bad tloc is EFAULT --
    printf("legacy");
    g_tloc = 0;
    long now = raw(SYS_time, (long)&g_tloc, 0, 0);
    TOK("time_static", now > 0 && g_tloc == now);
    TOK("time_bad", raw(SYS_time, -1L, 0, 0) == -EFAULT);

    // -- select's timeout is a `struct timeval`, converted here rather than by a host syscall --
    g_timeout.tv_sec = 0;
    g_timeout.tv_usec = 1000;
    TOK("select_static", syscall(SYS_select, 0, 0, 0, 0, &g_timeout) == 0);
    TOK("select_bad", syscall(SYS_select, 0, 0, 0, 0, (void *)-1L) == -1 && errno == EFAULT);

    // -- the legacy times buffers (utimbuf / timeval[2]) --
    TOK("utimes_bad", raw(SYS_utimes, (long)"/", -1L, 0) == -EFAULT);
    TOK("utime_bad", raw(SYS_utime, (long)"/", -1L, 0) == -EFAULT);
    TOK("futimesat_bad", syscall(SYS_futimesat, -100L, "/", (void *)-1L) == -1 && errno == EFAULT);
    printf("\n");
    return 0;
}
