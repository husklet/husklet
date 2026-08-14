#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <sys/mman.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <unistd.h>

static const char *error_name(int value) {
    if (value == EINVAL) return "EINVAL";
    if (value == ENOMEM) return "ENOMEM";
    if (value == EPERM) return "EPERM";
    return value == 0 ? "0" : "OTHER";
}

int main(void) {
    long page = sysconf(_SC_PAGESIZE);
    unsigned char *range = mmap(0, page * 2, PROT_READ | PROT_WRITE,
                                MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (range == MAP_FAILED) return 2;
    range[0] = 1; range[page] = 2;
    int lock_range = mlock(range, page * 2) == 0;
    int unlock_range = munlock(range, page * 2) == 0;
    errno = 0;
    int all_call = mlockall(MCL_CURRENT);
    const char *all = all_call == 0 ? "0" : error_name(errno);
    int unall = munlockall() == 0;
    errno = 0;
    int bad_call = mlockall(0x40);
    const char *bad = bad_call == 0 ? "0" : error_name(errno);

    struct rlimit limit = {(rlim_t)page, (rlim_t)page};
    int limit_set = setrlimit(RLIMIT_MEMLOCK, &limit) == 0;
    int future = mlockall(MCL_FUTURE | MCL_ONFAULT) == 0;
    unsigned char *one = mmap(0, page, PROT_READ | PROT_WRITE,
                              MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    int future_one = one != MAP_FAILED;
    if (future_one) one[0] = 3;
    int future_policy = munlockall() == 0 && mlockall(MCL_FUTURE) == 0;
    errno = 0;
    void *too_large = mmap(0, page * 2, PROT_READ | PROT_WRITE,
                           MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    int future_limit = future_policy && too_large == MAP_FAILED && errno == ENOMEM;
    munlockall();
    void *fixed_base = mmap(0, page * 2, PROT_READ | PROT_WRITE,
                            MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    int fixed_limit = 0;
    if (fixed_base != MAP_FAILED) {
        fixed_limit = mlockall(MCL_FUTURE) == 0 &&
            mmap(fixed_base, page * 2, PROT_READ | PROT_WRITE,
                 MAP_FIXED | MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) == MAP_FAILED && errno == ENOMEM;
        munmap(fixed_base, page * 2);
    }
    int clear = munlockall() == 0;
    void *after_clear = mmap(0, page * 2, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    int clear_map = after_clear != MAP_FAILED;
    if (after_clear != MAP_FAILED) munmap(after_clear, page * 2);
    if (one != MAP_FAILED) munmap(one, page);

    int fork_empty = mlockall(MCL_FUTURE | MCL_ONFAULT) == 0;
    pid_t child = fork();
    if (child == 0) {
        void *fresh = mmap(0, page * 2, PROT_READ | PROT_WRITE,
                           MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        _exit(fresh != MAP_FAILED ? 0 : 1);
    }
    int status = 0;
    fork_empty &= child > 0 && waitpid(child, &status, 0) == child
        && WIFEXITED(status) && WEXITSTATUS(status) == 0;
    munlockall();

    munmap(range + page, page);
    errno = 0;
    int hole_call = mlock(range, page * 2);
    const char *hole = hole_call == 0 ? "0" : error_name(errno);
    munmap(range, page);
    printf("lock_range=%d unlock_range=%d mlockall=%s munlockall=%d badflag=%s limit_set=%d future=%d future_one=%d future_limit=%d fixed_limit=%d clear=%d clear_map=%d fork_empty=%d hole=%s\n",
           lock_range, unlock_range, all, unall, bad, limit_set, future, future_one,
           future_limit, fixed_limit, clear, clear_map, fork_empty, hole);
    return 0;
}
