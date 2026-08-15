// faccessat / faccessat2: F_OK / R_OK / W_OK / X_OK checks, AT_SYMLINK_NOFOLLOW on a
// dangling link, and ENOENT / EACCES resolution against mode bits.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/fsuid.h>
#include <sys/prctl.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef __NR_faccessat2
#define __NR_faccessat2 439
#endif

struct identity_checks {
    int mode_zero_exists;
    int execute_only_executes;
    int execute_only_directory;
    int real_id_denied;
    int effective_id_allowed;
    int keepcaps_denied;
};

static struct identity_checks check_identities(int dfd) {
    int results[2];
    struct identity_checks checks = {0};
    if (pipe(results) != 0) return checks;
    pid_t child = fork();
    if (child == 0) {
        close(results[0]);
        struct identity_checks child_checks = {0};
        if (setresuid(1000, 0, 0) == 0) {
            (void)setfsuid(1234);
            errno = 0;
            child_checks.real_id_denied =
                syscall(__NR_faccessat2, dfd, "owner-only", R_OK, 0) == -1 && errno == EACCES;
            child_checks.effective_id_allowed =
                syscall(__NR_faccessat2, dfd, "owner-only", R_OK, AT_EACCESS) == 0;
        }
        (void)write(results[1], &child_checks, sizeof child_checks);
        _exit(0);
    }
    close(results[1]);
    if (child > 0) {
        (void)read(results[0], &checks, sizeof checks);
        (void)waitpid(child, NULL, 0);
    }
    close(results[0]);

    if (pipe(results) != 0) return checks;
    child = fork();
    if (child == 0) {
        close(results[0]);
        struct identity_checks child_checks = checks;
        if (prctl(PR_SET_KEEPCAPS, 1, 0, 0, 0) == 0 && setresuid(1000, 1000, 1000) == 0) {
            child_checks.mode_zero_exists = faccessat(dfd, "mode-zero", F_OK, 0) == 0;
            child_checks.execute_only_executes = faccessat(dfd, "execute-only", X_OK, 0) == 0;
            child_checks.execute_only_directory = faccessat(dfd, "search-only/inside", F_OK, 0) == 0;
            errno = 0;
            child_checks.keepcaps_denied = faccessat(dfd, "mode-zero", R_OK, 0) == -1 && errno == EACCES;
        }
        (void)write(results[1], &child_checks, sizeof child_checks);
        _exit(0);
    }
    close(results[1]);
    if (child > 0) {
        (void)read(results[0], &checks, sizeof checks);
        (void)waitpid(child, NULL, 0);
    }
    close(results[0]);
    return checks;
}

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_faccess_%d", (int)getpid());
    mkdir(dir, 0755);
    int dfd = open(dir, O_RDONLY | O_DIRECTORY);

    int fd = openat(dfd, "rx", O_CREAT | O_WRONLY, 0644);
    close(fd);
    fchmodat(dfd, "rx", 0555, 0);   // read+execute, no write

    int exists = faccessat(dfd, "rx", F_OK, 0) == 0;
    int readable = faccessat(dfd, "rx", R_OK, 0) == 0;
    int executable = faccessat(dfd, "rx", X_OK, 0) == 0;

    // Missing name -> ENOENT.
    errno = 0;
    int miss = faccessat(dfd, "gone", F_OK, 0);
    int enoent = miss != 0 && errno == ENOENT;

    // Dangling symlink: following -> ENOENT, NOFOLLOW on the link itself -> exists.
    symlinkat("nonexistent-target", dfd, "dangling");
    errno = 0;
    int follow_dangling = faccessat(dfd, "dangling", F_OK, 0);
    int dangling_enoent = follow_dangling != 0 && errno == ENOENT;
    int nofollow_exists = faccessat(dfd, "dangling", F_OK, AT_SYMLINK_NOFOLLOW) == 0;

    // faccessat2 with the same flag mirrors faccessat.
    int a2 = (int)syscall(__NR_faccessat2, dfd, "rx", R_OK, 0);
    int faccessat2_ok = a2 == 0;

    fd = openat(dfd, "mode-zero", O_CREAT | O_WRONLY, 0000);
    close(fd);
    fd = openat(dfd, "execute-only", O_CREAT | O_WRONLY, 0111);
    close(fd);
    fd = openat(dfd, "owner-only", O_CREAT | O_WRONLY, 0700);
    close(fd);
    mkdirat(dfd, "search-only", 0755);
    int search = openat(dfd, "search-only", O_RDONLY | O_DIRECTORY);
    fd = openat(search, "inside", O_CREAT | O_WRONLY, 0000);
    close(fd);
    close(search);
    fchmodat(dfd, "search-only", 0111, 0);
    struct identity_checks identities = check_identities(dfd);

    errno = 0;
    int invalid_flags = syscall(__NR_faccessat2, dfd, "rx", F_OK, 0x80000000u) == -1 && errno == EINVAL;

    const char *noexec_path = "/dev/shm/hl-faccessat-noexec";
    fd = open(noexec_path, O_CREAT | O_WRONLY | O_TRUNC, 0755);
    int noexec_denied = fd >= 0;
    if (fd >= 0) {
        close(fd);
        errno = 0;
        noexec_denied = access(noexec_path, X_OK) == -1 && errno == EACCES;
        unlink(noexec_path);
    }

    fchmodat(dfd, "rx", 0644, 0);
    unlinkat(dfd, "rx", 0);
    unlinkat(dfd, "dangling", 0);
    unlinkat(dfd, "mode-zero", 0);
    unlinkat(dfd, "execute-only", 0);
    unlinkat(dfd, "owner-only", 0);
    fchmodat(dfd, "search-only", 0755, 0);
    unlinkat(dfd, "search-only/inside", 0);
    unlinkat(dfd, "search-only", AT_REMOVEDIR);
    close(dfd);
    rmdir(dir);
    printf("faccessat-flags exists=%d readable=%d executable=%d enoent=%d dangling-enoent=%d nofollow-exists=%d "
           "faccessat2=%d mode-zero=%d execute-only=%d search-only=%d real-denied=%d effective-allowed=%d "
           "keepcaps-denied=%d "
           "invalid-flags=%d noexec=%d\n",
           exists, readable, executable, enoent, dangling_enoent, nofollow_exists, faccessat2_ok,
           identities.mode_zero_exists, identities.execute_only_executes, identities.execute_only_directory,
           identities.real_id_denied, identities.effective_id_allowed, identities.keepcaps_denied, invalid_flags,
           noexec_denied);
    return !(exists && readable && executable && enoent && dangling_enoent && nofollow_exists && faccessat2_ok &&
             identities.mode_zero_exists && identities.execute_only_executes && identities.execute_only_directory &&
             identities.real_id_denied && identities.effective_id_allowed && identities.keepcaps_denied &&
             invalid_flags && noexec_denied);
}
