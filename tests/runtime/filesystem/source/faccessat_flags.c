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
    int denied_directory;
    int denied_missing;
    int denied_trailing;
    int denied_symlink;
    int denied_absolute;
    int denied_start;
    int dev_console_denied;
    int dev_noexec_denied;
    int dev_relative_noexec_denied;
    int dev_trailing_not_directory;
    int dev_aliases_noexec;
    int dev_alias_nofollow;
    int proc_exe_allowed;
    int proc_aliases_allowed;
    int trailing_nofollow;
    int proc_exe_unlinked_allowed;
    int real_id_denied;
    int effective_id_allowed;
    int keepcaps_denied;
};

static struct identity_checks check_identities(int dfd, int denied_dfd, const char *absolute_denied) {
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
            child_checks.real_id_denied = syscall(__NR_faccessat2, dfd, "owner-only", R_OK, 0) == -1 && errno == EACCES;
            child_checks.effective_id_allowed = syscall(__NR_faccessat2, dfd, "owner-only", R_OK, AT_EACCESS) == 0;
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
            child_checks.denied_directory = faccessat(dfd, "denied/inside", F_OK, 0) == -1 && errno == EACCES;
            errno = 0;
            child_checks.denied_missing = faccessat(dfd, "denied/missing", F_OK, 0) == -1 && errno == EACCES;
            errno = 0;
            child_checks.denied_trailing = faccessat(dfd, "denied/", F_OK, 0) == -1 && errno == EACCES;
            errno = 0;
            child_checks.denied_symlink = faccessat(dfd, "denied-link/inside", F_OK, 0) == -1 && errno == EACCES;
            errno = 0;
            child_checks.denied_absolute = access(absolute_denied, F_OK) == -1 && errno == EACCES;
            errno = 0;
            child_checks.denied_start = faccessat(denied_dfd, "inside", F_OK, 0) == -1 && errno == EACCES;
            errno = 0;
            child_checks.dev_console_denied = access("/dev/console", R_OK) == -1 && errno == EACCES;
            errno = 0;
            child_checks.dev_noexec_denied = access("/dev/null", X_OK) == -1 && errno == EACCES;
            int devfd = open("/dev", O_RDONLY | O_DIRECTORY);
            errno = 0;
            child_checks.dev_relative_noexec_denied =
                devfd >= 0 && faccessat(devfd, "null", X_OK, 0) == -1 && errno == EACCES;
            errno = 0;
            child_checks.dev_trailing_not_directory =
                devfd >= 0 && faccessat(devfd, "null/", F_OK, 0) == -1 && errno == ENOTDIR;
            if (devfd >= 0) close(devfd);
            errno = 0;
            child_checks.dev_trailing_not_directory =
                child_checks.dev_trailing_not_directory && access("/dev/null/", F_OK) == -1 && errno == ENOTDIR;
            errno = 0;
            child_checks.dev_aliases_noexec = faccessat(dfd, "dev-absolute", X_OK, 0) == -1 && errno == EACCES;
            errno = 0;
            child_checks.dev_aliases_noexec =
                child_checks.dev_aliases_noexec && faccessat(dfd, "dev-chain", X_OK, 0) == -1 && errno == EACCES;
            child_checks.dev_alias_nofollow =
                syscall(__NR_faccessat2, dfd, "dev-absolute", X_OK, AT_SYMLINK_NOFOLLOW) == 0;
            child_checks.proc_exe_allowed = access("/proc/self/exe", X_OK) == 0;
            child_checks.proc_aliases_allowed =
                faccessat(dfd, "proc-absolute", X_OK, 0) == 0 && faccessat(dfd, "proc-chain", X_OK, 0) == 0;
            errno = 0;
            child_checks.trailing_nofollow =
                syscall(__NR_faccessat2, dfd, "search-link/", F_OK, AT_SYMLINK_NOFOLLOW) == 0;
            errno = 0;
            child_checks.trailing_nofollow =
                child_checks.trailing_nofollow &&
                syscall(__NR_faccessat2, dfd, "proc-absolute/", F_OK, AT_SYMLINK_NOFOLLOW) == -1 && errno == ENOTDIR;
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

int main(int argc, char **argv) {
    if (argc == 2 && !strcmp(argv[1], "--unlink-self")) {
        char renamed[256];
        char alias[256];
        snprintf(renamed, sizeof renamed, "%s.renamed", argv[0]);
        snprintf(alias, sizeof alias, "%s.proc", argv[0]);
        if (symlink("/proc/self/exe", alias) != 0 || rename(argv[0], renamed) != 0 || unlink(renamed) != 0) return 2;
        int allowed = access("/proc/self/exe", X_OK) == 0 && access(alias, X_OK) == 0;
        unlink(alias);
        return allowed ? 0 : 3;
    }
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_faccess_%d", (int)getpid());
    mkdir(dir, 0755);
    int dfd = open(dir, O_RDONLY | O_DIRECTORY);

    int fd = openat(dfd, "rx", O_CREAT | O_WRONLY, 0644);
    close(fd);
    fchmodat(dfd, "rx", 0555, 0); // read+execute, no write

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
    mkdirat(dfd, "denied", 0755);
    search = openat(dfd, "denied", O_RDONLY | O_DIRECTORY);
    fd = openat(search, "inside", O_CREAT | O_WRONLY, 0644);
    close(fd);
    close(search);
    fchownat(dfd, "denied", 1234, 1234, 0);
    fchmodat(dfd, "denied", 0700, 0);
    symlinkat("denied", dfd, "denied-link");
    symlinkat("/dev/null", dfd, "dev-absolute");
    symlinkat("../../dev/null", dfd, "dev-relative");
    symlinkat("dev-relative", dfd, "dev-chain");
    symlinkat("/proc/self/exe", dfd, "proc-absolute");
    symlinkat("../../proc/self/exe", dfd, "proc-relative");
    symlinkat("proc-relative", dfd, "proc-chain");
    symlinkat("search-only", dfd, "search-link");
    mkdirat(dfd, "start-denied", 0755);
    int denied_dfd = openat(dfd, "start-denied", O_RDONLY | O_DIRECTORY);
    fd = openat(denied_dfd, "inside", O_CREAT | O_WRONLY, 0644);
    close(fd);
    fchownat(dfd, "start-denied", 1234, 1234, 0);
    fchmodat(dfd, "start-denied", 0700, 0);
    char absolute_denied[256];
    snprintf(absolute_denied, sizeof absolute_denied, "%s/denied/missing", dir);
    int root_override_allowed = faccessat(dfd, "denied/inside", F_OK, 0) == 0;
    struct identity_checks identities = check_identities(dfd, denied_dfd, absolute_denied);

    char self_copy[256];
    snprintf(self_copy, sizeof self_copy, "/tmp/hl_faccess_self_%d", (int)getpid());
    int source = open("/proc/self/exe", O_RDONLY);
    int destination = open(self_copy, O_CREAT | O_EXCL | O_WRONLY, 0700);
    int copied = source >= 0 && destination >= 0;
    char bytes[16384];
    while (copied) {
        ssize_t count = read(source, bytes, sizeof bytes);
        if (count == 0) break;
        if (count < 0 || write(destination, bytes, (size_t)count) != count) copied = 0;
    }
    if (source >= 0) close(source);
    if (destination >= 0) close(destination);
    pid_t unlink_child = copied ? fork() : -1;
    if (unlink_child == 0) {
        char *arguments[] = {self_copy, "--unlink-self", NULL};
        execv(self_copy, arguments);
        _exit(4);
    }
    int unlink_status = 0;
    if (unlink_child > 0) waitpid(unlink_child, &unlink_status, 0);
    int proc_exe_unlinked_allowed = unlink_child > 0 && WIFEXITED(unlink_status) && WEXITSTATUS(unlink_status) == 0;
    unlink(self_copy);
    identities.proc_exe_unlinked_allowed = proc_exe_unlinked_allowed;

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
    fchmodat(dfd, "denied", 0755, 0);
    unlinkat(dfd, "denied-link", 0);
    unlinkat(dfd, "dev-absolute", 0);
    unlinkat(dfd, "dev-relative", 0);
    unlinkat(dfd, "dev-chain", 0);
    unlinkat(dfd, "proc-absolute", 0);
    unlinkat(dfd, "proc-relative", 0);
    unlinkat(dfd, "proc-chain", 0);
    unlinkat(dfd, "search-link", 0);
    unlinkat(dfd, "denied/inside", 0);
    unlinkat(dfd, "denied", AT_REMOVEDIR);
    fchmodat(dfd, "start-denied", 0755, 0);
    unlinkat(dfd, "start-denied/inside", 0);
    close(denied_dfd);
    unlinkat(dfd, "start-denied", AT_REMOVEDIR);
    close(dfd);
    rmdir(dir);
    printf("faccessat-flags exists=%d readable=%d executable=%d enoent=%d dangling-enoent=%d nofollow-exists=%d "
           "faccessat2=%d mode-zero=%d execute-only=%d search-only=%d denied-directory=%d denied-missing=%d "
           "denied-trailing=%d denied-symlink=%d denied-absolute=%d denied-start=%d dev-console=%d "
           "dev-noexec=%d dev-relative-noexec=%d dev-trailing-not-directory=%d dev-aliases-noexec=%d "
           "dev-alias-nofollow=%d proc-exe=%d proc-aliases=%d trailing-nofollow=%d "
           "proc-exe-unlinked=%d root-override=%d "
           "real-denied=%d effective-allowed=%d "
           "keepcaps-denied=%d "
           "invalid-flags=%d noexec=%d\n",
           exists, readable, executable, enoent, dangling_enoent, nofollow_exists, faccessat2_ok,
           identities.mode_zero_exists, identities.execute_only_executes, identities.execute_only_directory,
           identities.denied_directory, identities.denied_missing, identities.denied_trailing,
           identities.denied_symlink, identities.denied_absolute, identities.denied_start,
           identities.dev_console_denied, identities.dev_noexec_denied, identities.dev_relative_noexec_denied,
           identities.dev_trailing_not_directory, identities.dev_aliases_noexec, identities.dev_alias_nofollow,
           identities.proc_exe_allowed, identities.proc_aliases_allowed, identities.trailing_nofollow,
           identities.proc_exe_unlinked_allowed, root_override_allowed, identities.real_id_denied,
           identities.effective_id_allowed, identities.keepcaps_denied, invalid_flags, noexec_denied);
    return !(exists && readable && executable && enoent && dangling_enoent && nofollow_exists && faccessat2_ok &&
             identities.mode_zero_exists && identities.execute_only_executes && identities.execute_only_directory &&
             identities.denied_directory && identities.denied_missing && identities.denied_trailing &&
             identities.denied_symlink && identities.denied_absolute && identities.denied_start &&
             identities.dev_console_denied && identities.dev_noexec_denied && identities.dev_relative_noexec_denied &&
             identities.dev_trailing_not_directory && identities.dev_aliases_noexec && identities.dev_alias_nofollow &&
             identities.proc_exe_allowed && identities.proc_aliases_allowed && identities.trailing_nofollow &&
             identities.proc_exe_unlinked_allowed && root_override_allowed && identities.real_id_denied &&
             identities.effective_id_allowed && identities.keepcaps_denied && invalid_flags && noexec_denied);
}
