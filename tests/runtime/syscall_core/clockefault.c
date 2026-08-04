// clockefault.c — guest RESULT-pointer validation for the time syscalls (#218/#215). A bad or NULL guest
// buffer to clock_gettime/gettimeofday must return -EFAULT to the guest (never fault the engine), byte-exact
// with the kernel's access_ok() contract — INCLUDING through the x86 vDSO fast-syscall inline path
// (translate/x86_64/emit.c emit_fast_syscall), which previously stored the result UNGUARDED and crashed the
// engine on a bad pointer. RAW syscalls (SYS_*) so we hit that fast path directly, bypassing any libc
// pointer check. The registered sibling case runs this same binary under HL_JIT_NOFASTSYS=1 to also cover the
// slow (svc_time) path. Per-syscall NULL policy matches Linux exactly: clock_gettime(NULL) = EFAULT (the
// kernel always copies the result out), gettimeofday(NULL,·) = 0 (a legal no-op). Diffed vs the native/qemu
// oracle (.oracle()).
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <unistd.h>

static const char *ev(long r) {
    return (r == -1 && errno == EFAULT) ? "EFAULT" : (r == 0 ? "ok" : "?");
}

int main(void) {
    void *bad = (void *)0x0000123400000000ULL; // far, unmapped
    struct timeval tv;                          // a valid buffer for the control case

    errno = 0; const char *cg_bad   = ev(syscall(SYS_clock_gettime, 0 /*REALTIME*/, bad));
    errno = 0; const char *cg_mono  = ev(syscall(SYS_clock_gettime, 1 /*MONOTONIC*/, bad));
    errno = 0; const char *cg_null  = ev(syscall(SYS_clock_gettime, 0, (void *)0));      // -> EFAULT
    errno = 0; const char *gtod_bad = ev(syscall(SYS_gettimeofday, bad, (void *)0));
    errno = 0; const char *gtod_null = ev(syscall(SYS_gettimeofday, (void *)0, (void *)0)); // -> ok(0)
    errno = 0; const char *gtod_ok  = ev(syscall(SYS_gettimeofday, &tv, (void *)0));      // -> ok(0)

    printf("clockefault cg_bad=%s cg_mono=%s cg_null=%s gtod_bad=%s gtod_null=%s gtod_ok=%s survived=1\n",
           cg_bad, cg_mono, cg_null, gtod_bad, gtod_null, gtod_ok);
    return 0;
}
