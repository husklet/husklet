/* Modern process/thread/fd startup syscalls that glibc and language runtimes issue: dup3, close_range,
   close edges, name_to_handle_at/open_by_handle_at, execveat(AT_EMPTY_PATH)=fexecve, and the rseq/getcpu
   per-cpu fallback. Every line is a derived boolean so the golden is captured from the native kernel and
   the engine must reproduce it byte-for-byte. Arch-neutral. */
#define _GNU_SOURCE
#include <sys/syscall.h>
#include <unistd.h>
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <fcntl.h>
#include <sched.h>
#include <sys/wait.h>

#ifndef __NR_close_range
#define __NR_close_range 436
#endif
#ifndef CLOSE_RANGE_CLOEXEC
#define CLOSE_RANGE_CLOEXEC 4
#endif

int main(int argc, char **argv) {
    if (argc > 1) return 0; /* re-exec'd via fexecve: exit 0 without touching the parent's stdout */

    /* dup3: old==new is EINVAL (the defining difference from dup2's no-op); a valid target with O_CLOEXEC
       lands FD_CLOEXEC; an unknown flag bit is EINVAL. */
    int fd = dup(1);
    long same = syscall(SYS_dup3, fd, fd, 0);
    int hi = fd + 40;
    long ok = syscall(SYS_dup3, fd, hi, O_CLOEXEC);
    int cf = fcntl(hi, F_GETFD);
    long badf = syscall(SYS_dup3, fd, hi, 0x12345);
    printf("dup3_same_einval=%d dup3_ok=%d dup3_cloexec=%d dup3_badflag_einval=%d\n",
           (same < 0 && errno == EINVAL), (ok == hi), ((cf & FD_CLOEXEC) != 0),
           (badf < 0 && errno == EINVAL));
    close(hi);
    close(fd);

    /* close_range: CLOSE_RANGE_CLOEXEC sets FD_CLOEXEC (does not close); a plain call closes every fd in
       the span while 0/1/2 survive; lo>hi and an unknown flag are EINVAL. */
    int a = dup(1), b = dup(1), c = dup(1);
    int lo = a < b ? a : b; lo = lo < c ? lo : c;
    long crc = syscall(__NR_close_range, lo, c, CLOSE_RANGE_CLOEXEC);
    int all_ce = 1;
    for (int f = lo; f <= c; f++) { int g = fcntl(f, F_GETFD); if (g < 0 || !(g & FD_CLOEXEC)) all_ce = 0; }
    long cr = syscall(__NR_close_range, lo, c, 0);
    int all_closed = 1;
    for (int f = lo; f <= c; f++) if (fcntl(f, F_GETFD) >= 0) all_closed = 0;
    int std_alive = (fcntl(0, F_GETFD) >= 0 && fcntl(1, F_GETFD) >= 0 && fcntl(2, F_GETFD) >= 0);
    long lohi = syscall(__NR_close_range, 10, 5, 0);
    long crbad = syscall(__NR_close_range, 3, 3, 0x40);
    printf("cr_cloexec=%d cr_all_cloexec=%d cr_close=%d cr_all_closed=%d cr_std_alive=%d "
           "cr_lohi_einval=%d cr_badflag_einval=%d\n",
           (crc == 0), all_ce, (cr == 0), all_closed, std_alive,
           (lohi < 0 && errno == EINVAL), (crbad < 0 && errno == EINVAL));

    /* close: reclosing a closed fd and close(-1) are both EBADF. */
    int x = dup(1); close(x);
    long rec = close(x);
    printf("close_reclosed_ebadf=%d close_neg1_ebadf=%d\n",
           (rec < 0 && errno == EBADF), (close(-1) < 0 && errno == EBADF));

    /* name_to_handle_at succeeds unprivileged; open_by_handle_at needs CAP_DAC_READ_SEARCH so it is EPERM
       for an unprivileged task (must NOT masquerade as ENOSYS). */
    struct { unsigned handle_bytes; int handle_type; unsigned char f[128]; } h;
    memset(&h, 0, sizeof h); h.handle_bytes = 128;
    int mid = 0;
    long n2h = syscall(SYS_name_to_handle_at, AT_FDCWD, "/", &h, &mid, 0);
    long obh = syscall(SYS_open_by_handle_at, AT_FDCWD, &h, O_RDONLY);
    printf("name_to_handle_ok=%d open_by_handle_eperm=%d\n",
           (n2h == 0), (obh < 0 && errno == EPERM));

    /* execveat(fd, "", AT_EMPTY_PATH) == fexecve: an O_PATH fd of this executable re-execs and the child
       runs with the passed argv. */
    int pfd = open("/proc/self/exe", O_PATH | O_CLOEXEC);
    int child_ok = 0;
    pid_t p = fork();
    if (p == 0) {
        char *av[] = { argv[0], "GO", NULL }, *ev[] = { NULL };
        syscall(SYS_execveat, pfd, "", av, ev, 0x1000);
        _exit(3);
    }
    int st; waitpid(p, &st, 0);
    child_ok = WIFEXITED(st) && WEXITSTATUS(st) == 0;
    printf("opath_ok=%d fexecve_child_ran=%d\n", (pfd >= 0), child_ok);

    /* rseq per-cpu fallback: glibc auto-registers rseq at startup where supported and falls back to
       getcpu()/sched_getcpu() where it is ENOSYS. Either way the reported cpu must be a valid cpu in
       range and getcpu() must agree with sched_getcpu() so per-cpu allocators stay self-consistent. */
    long np = sysconf(_SC_NPROCESSORS_ONLN);
    int sc = sched_getcpu();
    unsigned gcpu = 999, gnode = 999;
    long gc = syscall(SYS_getcpu, &gcpu, &gnode, NULL);
    printf("sched_getcpu_valid=%d getcpu_ok=%d getcpu_consistent=%d cpu_in_range=%d\n",
           (sc >= 0 && sc < np), (gc == 0), ((int)gcpu == sc), ((int)gcpu >= 0 && (int)gcpu < np));
    return 0;
}
