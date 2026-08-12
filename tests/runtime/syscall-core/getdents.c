// Directory enumeration: create a dir with known entries, then opendir/readdir and collect the
// names (skipping . and ..), counting and checksumming them order-independently. Exercises the
// getdents/readdir path + stat-by-dirent. Portable -> all engines, golden-checked.
#include <dirent.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

struct linux_dirent64 {
    uint64_t ino;
    int64_t off;
    unsigned short reclen;
    unsigned char type;
    char name[];
};

int main(void) {
    const char *dir = "/tmp/hl_getdents_dir";
    mkdir(dir, 0755);
    const char *names[] = {"alpha", "bravo", "charlie", "delta"};
    char p[256];
    for (int i = 0; i < 4; i++) {
        snprintf(p, sizeof p, "%s/%s", dir, names[i]);
        FILE *f = fopen(p, "w");
        if (f) fclose(f);
    }
    DIR *d = opendir(dir);
    int count = 0;
    long namechk = 0;
    struct dirent *e;
    while ((e = readdir(d))) {
        if (!strcmp(e->d_name, ".") || !strcmp(e->d_name, "..")) continue;
        count++;
        for (const char *c = e->d_name; *c; c++) namechk += (unsigned char)*c; // order-independent
    }
    closedir(d);
    int raw = open(dir, O_RDONLY | O_DIRECTORY);
    char buf[48];
    int raw_count = 0, layout = 1, typed = 1, eof = 0, rewind_ok = 0, shared = 0, alias_rewind = 0;
    int fork_shared = 0, close_safe = 0;
    long n;
    while ((n = syscall(SYS_getdents64, raw, buf, sizeof buf)) > 0) {
        long at = 0;
        while (at < n) {
            struct linux_dirent64 *entry = (struct linux_dirent64 *)(buf + at);
            size_t names = strnlen(entry->name, entry->reclen > 19 ? entry->reclen - 19 : 0);
            if (entry->reclen < 24 || (entry->reclen & 7) != 0 || at + entry->reclen > n || names == 0)
                layout = 0;
            if (strcmp(entry->name, ".") && strcmp(entry->name, "..") && entry->type != DT_REG) typed = 0;
            raw_count++;
            at += entry->reclen;
        }
    }
    eof = n == 0 && syscall(SYS_getdents64, raw, buf, sizeof buf) == 0;
    rewind_ok = lseek(raw, 0, SEEK_SET) == 0 && syscall(SYS_getdents64, raw, buf, sizeof buf) > 0;
    int shared_fd = open(dir, O_RDONLY | O_DIRECTORY);
    int alias = dup(shared_fd);
    long first = syscall(SYS_getdents64, shared_fd, buf, sizeof buf);
    char first_name[32] = {0};
    if (first > 0) snprintf(first_name, sizeof first_name, "%s", ((struct linux_dirent64 *)buf)->name);
    long second = syscall(SYS_getdents64, alias, buf, sizeof buf);
    shared = first > 0 && second > 0 && strcmp(first_name, ((struct linux_dirent64 *)buf)->name) != 0;
    // Contract: dup'd fds share one offset, so rewinding the alias restarts the shared
    // stream. Assert "same first entry as the first read", NOT literally "." -- readdir
    // order is filesystem-specific (ext4 htree hashes ./.. into the middle).
    if (first > 0 && lseek(alias, 0, SEEK_SET) == 0) {
        long reset = syscall(SYS_getdents64, shared_fd, buf, sizeof buf);
        alias_rewind = reset > 0 && !strcmp(((struct linux_dirent64 *)buf)->name, first_name);
    }
    close(shared_fd);
    close(alias);
    int fork_fd = open(dir, O_RDONLY | O_DIRECTORY);
    int channel[2];
    long before_fork = syscall(SYS_getdents64, fork_fd, buf, sizeof buf);
    if (before_fork > 0 && pipe(channel) == 0) {
        pid_t child = fork();
        if (child == 0) {
            char child_name[32] = {0};
            long child_read = syscall(SYS_getdents64, fork_fd, buf, sizeof buf);
            if (child_read > 0)
                snprintf(child_name, sizeof child_name, "%s", ((struct linux_dirent64 *)buf)->name);
            close(fork_fd);
            close(channel[0]);
            ssize_t sent = write(channel[1], child_name, sizeof child_name);
            close(channel[1]);
            _exit(child_read > 0 && sent == sizeof child_name ? 0 : 1);
        }
        close(channel[1]);
        char child_name[32] = {0};
        ssize_t received = read(channel[0], child_name, sizeof child_name);
        close(channel[0]);
        int status = 0;
        waitpid(child, &status, 0);
        long parent_read = syscall(SYS_getdents64, fork_fd, buf, sizeof buf);
        fork_shared = received == sizeof child_name && WIFEXITED(status) && WEXITSTATUS(status) == 0 &&
                      parent_read > 0 && strcmp(child_name, ((struct linux_dirent64 *)buf)->name) != 0;
        close_safe = parent_read > 0;
    }
    close(fork_fd);
    close(raw);
    for (int i = 0; i < 4; i++) {
        snprintf(p, sizeof p, "%s/%s", dir, names[i]);
        unlink(p);
    }
    rmdir(dir);
    printf("getdents count=%d namechk=%ld raw=%d layout=%d type=%d eof=%d rewind=%d shared=%d alias_rewind=%d fork_shared=%d close_safe=%d\n",
           count, namechk, raw_count, layout, typed, eof, rewind_ok, shared, alias_rewind, fork_shared, close_safe);
    return 0;
}
